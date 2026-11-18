use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError},
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use num_complex::Complex32;
use rustfft::FftPlanner;

use crate::model::{BandConfig, SpectrumFrame, SweepConfig, now_ns};
use crate::soapy::{SoapyReadStats, SoapyRx, SoapyRxConfig};

pub struct RealSweepEngine {
    rx: SoapyRx,
    worker: FftPipeline,
    last_tune: Option<TunePair>,
    average_db: Vec<f32>,
    max_hold_db: Vec<f32>,
    peak_initialized: Vec<bool>,
    frame_id: u64,
    frame_revision: u64,
}

#[derive(Clone, Copy)]
struct TunePair {
    requested_hz: f64,
    actual_hz: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HopId {
    frame: u64,
    index: usize,
}

struct HopMetadata {
    id: HopId,
    center_hz: f64,
    sample_rate_hz: f64,
    band: BandConfig,
    usable_bandwidth_hz: f64,
    fft_size: usize,
}

#[derive(Default)]
struct HopPacket {
    samples: Vec<Complex32>,
    acc: Vec<f64>,
    count: Vec<u32>,
    metadata: Option<HopMetadata>,
}

struct HopResult {
    packet: HopPacket,
    completed: Result<bool, String>,
}

#[derive(Clone, Copy)]
struct PendingHop {
    slot: usize,
    id: HopId,
}

struct FftPipeline {
    input: Option<SyncSender<HopPacket>>,
    output: Option<Receiver<HopResult>>,
    thread: Option<JoinHandle<()>>,
    cancel: Arc<AtomicBool>,
    packets: [Option<HopPacket>; 2],
    next_slot: usize,
    pending: Option<PendingHop>,
}

impl FftPipeline {
    fn new() -> Result<Self, String> {
        let (input, jobs) = mpsc::sync_channel(1);
        let (results, output) = mpsc::sync_channel(1);
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        let thread = thread::Builder::new()
            .name("sdrwispy-fft".to_owned())
            .spawn(move || fft_worker(jobs, results, worker_cancel))
            .map_err(|err| format!("start FFT worker: {err}"))?;
        Ok(Self {
            input: Some(input),
            output: Some(output),
            thread: Some(thread),
            cancel,
            packets: [Some(HopPacket::default()), Some(HopPacket::default())],
            next_slot: 0,
            pending: None,
        })
    }

    fn begin_frame(&mut self) -> Result<(), String> {
        if self.pending.is_some() || self.input.is_none() {
            return Err("FFT pipeline is not ready for a new frame".to_owned());
        }
        self.cancel.store(false, Ordering::Relaxed);
        Ok(())
    }

    fn capture_slot(&self) -> Result<usize, String> {
        self.packets[self.next_slot]
            .as_ref()
            .map(|_| self.next_slot)
            .ok_or_else(|| "FFT capture packet is unavailable".to_owned())
    }

    fn submit(&mut self, slot: usize, metadata: HopMetadata) -> Result<(), String> {
        if self.pending.is_some() {
            return Err("FFT pipeline already has an outstanding hop".to_owned());
        }
        let input = self
            .input
            .as_ref()
            .ok_or_else(|| "FFT worker is shut down".to_owned())?;
        let mut packet = self.packets[slot]
            .take()
            .ok_or_else(|| "FFT packet is unavailable".to_owned())?;
        let id = metadata.id;
        packet.metadata = Some(metadata);
        match input.try_send(packet) {
            Ok(()) => {
                self.pending = Some(PendingHop { slot, id });
                self.next_slot = slot ^ 1;
                Ok(())
            }
            Err(err) => {
                let (packet, message) = match err {
                    TrySendError::Full(packet) => (packet, "FFT job channel is full"),
                    TrySendError::Disconnected(packet) => (packet, "FFT worker disconnected"),
                };
                self.packets[slot] = Some(packet);
                Err(message.to_owned())
            }
        }
    }

    fn drain(&mut self, stop: &AtomicBool) -> Result<Option<&HopPacket>, String> {
        let Some(pending) = self.pending else {
            return Ok(None);
        };
        let output = self
            .output
            .as_ref()
            .ok_or_else(|| "FFT worker is shut down".to_owned())?;
        let result = loop {
            if stop.load(Ordering::Relaxed) {
                self.cancel.store(true, Ordering::Relaxed);
            }
            match output.recv_timeout(Duration::from_millis(2)) {
                Ok(result) => break result,
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => {
                    self.pending = None;
                    return Err("FFT worker disconnected while processing a hop".to_owned());
                }
            }
        };
        self.pending = None;
        self.packets[pending.slot] = Some(result.packet);
        let packet = self.packets[pending.slot].as_ref().unwrap();
        if packet.metadata.as_ref().map(|meta| meta.id) != Some(pending.id) {
            return Err("FFT result does not match the outstanding hop".to_owned());
        }
        if !result.completed? || stop.load(Ordering::Relaxed) || self.cancel.load(Ordering::Relaxed)
        {
            return Ok(None);
        }
        Ok(Some(packet))
    }

    fn cancel_and_drain(&mut self) -> Result<(), String> {
        self.cancel.store(true, Ordering::Relaxed);
        let cancel = Arc::clone(&self.cancel);
        self.drain(&cancel).map(|_| ())
    }

    fn shutdown(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        self.input.take();
        // Close the receiver before joining: even a full result channel cannot block exit.
        self.output.take();
        self.pending = None;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for FftPipeline {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn fft_worker(jobs: Receiver<HopPacket>, results: SyncSender<HopResult>, cancel: Arc<AtomicBool>) {
    let mut processor = PsdProcessor::default();
    while let Ok(mut packet) = jobs.recv() {
        let completed = processor.process(&mut packet, &cancel);
        if results.send(HopResult { packet, completed }).is_err() {
            break;
        }
    }
}

#[derive(Debug, Clone)]
pub struct RealSweepReport {
    pub frame: SpectrumFrame,
    pub overflow_count: usize,
    pub timeout_count: usize,
    pub corruption_count: usize,
    pub captured_samples: usize,
    pub hop_count: usize,
    pub valid_bin_count: usize,
    pub min_db: f32,
    pub max_db: f32,
    pub stream_reset: bool,
    pub receiver_status: String,
}

impl RealSweepEngine {
    pub fn open(
        args: String,
        sample_rate_hz: f64,
        bandwidth_hz: f64,
        gain_db: f64,
    ) -> Result<Self, String> {
        let rx = SoapyRx::open(SoapyRxConfig {
            args: args.clone(),
            sample_rate_hz,
            bandwidth_hz,
            gain_db,
        })?;
        Ok(Self {
            rx,
            worker: FftPipeline::new()?,
            last_tune: None,
            average_db: Vec::new(),
            max_hold_db: Vec::new(),
            peak_initialized: Vec::new(),
            frame_id: 0,
            frame_revision: 0,
        })
    }

    pub fn receiver_status(&self) -> &str {
        self.rx.receiver_status()
    }

    pub fn pause(&mut self) -> Result<(), String> {
        self.last_tune = None;
        self.worker.cancel.store(true, Ordering::Relaxed);
        let hardware = self.rx.deactivate();
        let worker = self.worker.cancel_and_drain();
        hardware.and(worker)
    }

    pub fn shutdown(&mut self) {
        self.last_tune = None;
        self.worker.cancel.store(true, Ordering::Relaxed);
        self.rx.shutdown();
        self.worker.shutdown();
    }

    pub fn generate<F>(
        &mut self,
        band: &BandConfig,
        sweep: &SweepConfig,
        stop: &AtomicBool,
        mut on_progress: F,
    ) -> Result<Option<RealSweepReport>, String>
    where
        F: FnMut(SpectrumFrame),
    {
        let result = self.generate_frame(band, sweep, stop, &mut on_progress);
        if !matches!(&result, Ok(Some(_))) {
            let cleanup = self.pause();
            if result.is_ok() {
                cleanup?;
            }
        }
        result
    }

    fn generate_frame<F>(
        &mut self,
        band: &BandConfig,
        sweep: &SweepConfig,
        stop: &AtomicBool,
        on_progress: &mut F,
    ) -> Result<Option<RealSweepReport>, String>
    where
        F: FnMut(SpectrumFrame),
    {
        if stop.load(Ordering::Relaxed) {
            return Ok(None);
        }

        sweep.validate()?;
        self.worker.begin_frame()?;
        // Small accepted hardware rounding must also feed hop planning, not just the FFT.
        let mut actual_sweep = sweep.clone();
        actual_sweep.sample_rate_hz = self.rx.sample_rate_hz();
        let sweep = &actual_sweep;
        let bins = band.bin_count();
        if self.average_db.len() != bins {
            self.average_db = vec![-110.0; bins];
            self.max_hold_db = vec![-120.0; bins];
            self.peak_initialized = vec![false; bins];
        }

        let mut acc = vec![0.0f64; bins];
        let mut count = vec![0u32; bins];
        let hop_centers = hop_centers(band, sweep);
        let next_frame_id = self.frame_id.wrapping_add(1);
        let fft_size = sweep.fft_size.next_power_of_two().clamp(256, 65_536);
        let sample_rate = self.rx.sample_rate_hz();
        let samples_per_hop = ((sample_rate * sweep.dwell_ms.max(0.1) / 1000.0) as usize)
            .max(fft_size)
            .min(5_000_000);
        let discard_samples = ((sample_rate * 0.001) as usize).min(262_144);
        let requested_read_us = samples_per_hop as f64 / sample_rate.max(1.0) * 1.0e6;
        let timeout_us = ((sweep.dwell_ms + sweep.settle_ms + 25.0) * 1000.0)
            .max(requested_read_us * 4.0 + 20_000.0)
            .clamp(20_000.0, 200_000.0) as i64;
        let read_timeout_us = timeout_us;

        let start_ns = now_ns();
        let mut overflow_count = 0usize;
        let mut timeout_count = 0usize;
        let mut corruption_count = 0usize;
        let mut captured_samples = 0usize;
        let gate_stream = stream_gating_enabled(
            sweep.stream_gating,
            std::env::var("SDRWISPY_STREAM_GATING").ok().as_deref(),
        );
        let progress_interval = Duration::from_millis(50);
        let mut last_progress = Instant::now() - progress_interval;
        let mut completed_hops = 0usize;

        if gate_stream {
            self.last_tune = None;
            self.rx.deactivate()?;
        }
        for (index, center) in hop_centers.iter().enumerate() {
            if stop.load(Ordering::Relaxed) {
                return Ok(None);
            }

            let cached_center = reusable_tune(
                self.last_tune,
                gate_stream,
                hop_centers.len(),
                *center,
                self.rx.is_active(),
            );
            let skip_tune = if cached_center.is_some() {
                let (drained, stats) = self.rx.discard_pending(stop)?;
                merge_stats(
                    &stats,
                    &mut overflow_count,
                    &mut timeout_count,
                    &mut corruption_count,
                    &mut captured_samples,
                );
                drained
            } else {
                false
            };
            if stop.load(Ordering::Relaxed) {
                return Ok(None);
            }
            let actual_center = if skip_tune {
                cached_center.unwrap()
            } else {
                self.last_tune = None;
                if gate_stream {
                    self.rx.deactivate()?;
                }
                let actual_center = self.rx.tune(*center)?;
                if !sleep_cancelable(
                    Duration::from_secs_f64(sweep.settle_ms.max(0.0) / 1000.0),
                    stop,
                ) {
                    return Ok(None);
                }
                if gate_stream || !self.rx.is_active() {
                    self.rx.activate()?;
                }
                if discard_samples > 0 {
                    let stats = self.rx.discard(discard_samples, read_timeout_us, stop)?;
                    merge_stats(
                        &stats,
                        &mut overflow_count,
                        &mut timeout_count,
                        &mut corruption_count,
                        &mut captured_samples,
                    );
                }
                self.last_tune = Some(TunePair {
                    requested_hz: *center,
                    actual_hz: actual_center,
                });
                actual_center
            };
            if stop.load(Ordering::Relaxed) {
                return Ok(None);
            }

            let slot = self.worker.capture_slot()?;
            let packet = self.worker.packets[slot].as_mut().unwrap();
            let read_result = self.rx.read_into_buffer(
                &mut packet.samples,
                samples_per_hop,
                read_timeout_us,
                stop,
            );
            let deactivate_result = if gate_stream {
                self.rx.deactivate()
            } else {
                Ok(())
            };
            let stats = read_result?;
            deactivate_result?;
            if stop.load(Ordering::Relaxed) {
                return Ok(None);
            }
            merge_stats(
                &stats,
                &mut overflow_count,
                &mut timeout_count,
                &mut corruption_count,
                &mut captured_samples,
            );

            // Receive the next hop first, while the previous hop's FFT is running.
            self.finish_hop(
                stop,
                band,
                &mut acc,
                &mut count,
                next_frame_id,
                start_ns,
                hop_centers.len(),
                &mut completed_hops,
                &mut last_progress,
                on_progress,
            )?;
            if stop.load(Ordering::Relaxed) {
                return Ok(None);
            }
            if self.worker.packets[slot].as_ref().unwrap().samples.len() >= fft_size {
                self.worker.submit(
                    slot,
                    HopMetadata {
                        id: HopId {
                            frame: next_frame_id,
                            index,
                        },
                        center_hz: actual_center,
                        sample_rate_hz: sample_rate,
                        band: band.clone(),
                        usable_bandwidth_hz: sweep.usable_bandwidth_hz(),
                        fft_size,
                    },
                )?;
            }
        }

        self.finish_hop(
            stop,
            band,
            &mut acc,
            &mut count,
            next_frame_id,
            start_ns,
            hop_centers.len(),
            &mut completed_hops,
            &mut last_progress,
            on_progress,
        )?;
        if stop.load(Ordering::Relaxed) {
            return Ok(None);
        }
        if gate_stream {
            self.rx.deactivate()?;
        }

        let current = current_from_acc(&acc, &count);
        let valid_bin_count = count.iter().filter(|&&v| v > 0).count();
        let invalid = bins.saturating_sub(valid_bin_count);
        let (min_db, max_db) = min_max_db(&current, &count);
        update_history(
            &current,
            &count,
            &mut self.average_db,
            &mut self.max_hold_db,
            &mut self.peak_initialized,
        );
        let stream_reset = valid_bin_count == 0 && timeout_count > 0;
        if stream_reset {
            // A full sweep of timeouts means the Soapy/UHD stream accepted the
            // open/activate calls but delivered no RX buffers. Reset the stream
            // state so the next sweep performs tune -> activate again instead
            // of staying in a permanently timed-out active stream.
            self.last_tune = None;
            let _ = self.rx.deactivate();
        }

        self.frame_id = next_frame_id;
        self.frame_revision = self.frame_revision.wrapping_add(1);
        let frame = SpectrumFrame {
            capture_epoch: 0,
            frame_id: self.frame_id,
            frame_revision: self.frame_revision,
            partial: false,
            sweep_start_time_ns: start_ns,
            sweep_end_time_ns: now_ns(),
            band_start_hz: band.start_hz,
            band_stop_hz: band.stop_hz,
            bin_width_hz: band.bin_width_hz,
            current_db: current,
            average_db: self.average_db.clone(),
            max_hold_db: self.max_hold_db.clone(),
            valid_bin_ranges: valid_bin_ranges(&count),
            invalid_bin_count: invalid,
            estimated_hops: hop_centers.len(),
            completed_hops,
        };

        Ok(Some(RealSweepReport {
            frame,
            overflow_count,
            timeout_count,
            corruption_count,
            captured_samples,
            hop_count: hop_centers.len(),
            valid_bin_count,
            min_db,
            max_db,
            stream_reset,
            receiver_status: self.receiver_status().to_owned(),
        }))
    }

    #[allow(clippy::too_many_arguments)]
    fn finish_hop<F>(
        &mut self,
        stop: &AtomicBool,
        band: &BandConfig,
        acc: &mut [f64],
        count: &mut [u32],
        frame_id: u64,
        start_ns: i64,
        estimated_hops: usize,
        completed_hops: &mut usize,
        last_progress: &mut Instant,
        on_progress: &mut F,
    ) -> Result<(), String>
    where
        F: FnMut(SpectrumFrame),
    {
        let Some(packet) = self.worker.drain(stop)? else {
            return Ok(());
        };
        merge_hop(packet, frame_id, acc, count)?;
        *completed_hops += 1;
        if *completed_hops == 1 || last_progress.elapsed() >= Duration::from_millis(50) {
            self.frame_revision = self.frame_revision.wrapping_add(1);
            on_progress(build_spectrum_frame(
                band,
                acc,
                count,
                &self.average_db,
                &self.max_hold_db,
                frame_id,
                self.frame_revision,
                true,
                start_ns,
                estimated_hops,
                *completed_hops,
            ));
            *last_progress = Instant::now();
        }
        Ok(())
    }
}

impl Drop for RealSweepEngine {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn reusable_tune(
    cached: Option<TunePair>,
    gate_stream: bool,
    hops: usize,
    requested_hz: f64,
    active: bool,
) -> Option<f64> {
    cached
        .filter(|tune| !gate_stream && hops == 1 && active && tune.requested_hz == requested_hz)
        .map(|tune| tune.actual_hz)
}

fn merge_hop(
    packet: &HopPacket,
    frame_id: u64,
    acc: &mut [f64],
    count: &mut [u32],
) -> Result<(), String> {
    if packet.metadata.as_ref().map(|meta| meta.id.frame) != Some(frame_id)
        || packet.acc.len() != acc.len()
        || packet.count.len() != count.len()
    {
        return Err("FFT result does not match the current frame".to_owned());
    }
    for ((sum, count), (hop_sum, hop_count)) in acc
        .iter_mut()
        .zip(count.iter_mut())
        .zip(packet.acc.iter().zip(&packet.count))
    {
        *sum += hop_sum;
        *count = count.saturating_add(*hop_count);
    }
    Ok(())
}

fn update_history(
    current: &[f32],
    count: &[u32],
    average_db: &mut [f32],
    max_hold_db: &mut [f32],
    initialized: &mut [bool],
) {
    for i in 0..current.len() {
        if count[i] == 0 {
            continue;
        }
        average_db[i] = average_db[i] * 0.90 + current[i] * 0.10;
        max_hold_db[i] = if initialized[i] {
            max_hold_db[i].max(current[i])
        } else {
            current[i]
        };
        initialized[i] = true;
    }
}

fn hop_centers(band: &BandConfig, sweep: &SweepConfig) -> Vec<f64> {
    let usable = sweep
        .usable_bandwidth_hz()
        .min(band.span_hz())
        .max(band.bin_width_hz);
    if band.span_hz() <= usable {
        return vec![(band.start_hz + band.stop_hz) * 0.5];
    }

    let step = sweep.hop_step_hz().min(usable).max(band.bin_width_hz);
    let mut centers = Vec::new();
    let mut center = band.start_hz + usable * 0.5;
    let last_center = band.stop_hz - usable * 0.5;
    while center <= last_center {
        centers.push(center);
        center += step;
    }
    if centers
        .last()
        .is_none_or(|last| *last < last_center - band.bin_width_hz)
    {
        centers.push(last_center);
    }
    centers
}

#[allow(clippy::too_many_arguments)]
fn build_spectrum_frame(
    band: &BandConfig,
    acc: &[f64],
    count: &[u32],
    average_db: &[f32],
    max_hold_db: &[f32],
    frame_id: u64,
    frame_revision: u64,
    partial: bool,
    sweep_start_time_ns: i64,
    estimated_hops: usize,
    completed_hops: usize,
) -> SpectrumFrame {
    let current = current_from_acc(acc, count);
    let valid_bin_count = count.iter().filter(|&&v| v > 0).count();
    SpectrumFrame {
        capture_epoch: 0,
        frame_id,
        frame_revision,
        partial,
        sweep_start_time_ns,
        sweep_end_time_ns: now_ns(),
        band_start_hz: band.start_hz,
        band_stop_hz: band.stop_hz,
        bin_width_hz: band.bin_width_hz,
        current_db: current,
        average_db: average_db.to_vec(),
        max_hold_db: max_hold_db.to_vec(),
        valid_bin_ranges: valid_bin_ranges(count),
        invalid_bin_count: count.len().saturating_sub(valid_bin_count),
        estimated_hops,
        completed_hops,
    }
}

fn current_from_acc(acc: &[f64], count: &[u32]) -> Vec<f32> {
    acc.iter()
        .zip(count.iter())
        .map(|(sum, samples)| {
            if *samples > 0 {
                (*sum / *samples as f64) as f32
            } else {
                -120.0
            }
        })
        .collect()
}

fn min_max_db(current: &[f32], count: &[u32]) -> (f32, f32) {
    let mut min_db = f32::INFINITY;
    let mut max_db = f32::NEG_INFINITY;
    for (db, samples) in current.iter().zip(count.iter()) {
        if *samples == 0 {
            continue;
        }
        min_db = min_db.min(*db);
        max_db = max_db.max(*db);
    }
    if min_db.is_finite() && max_db.is_finite() {
        (min_db, max_db)
    } else {
        (-120.0, -120.0)
    }
}

fn valid_bin_ranges(count: &[u32]) -> Vec<[usize; 2]> {
    let mut ranges = Vec::new();
    let mut start = None;
    for (idx, samples) in count.iter().enumerate() {
        if *samples > 0 {
            if start.is_none() {
                start = Some(idx);
            }
        } else if let Some(s) = start.take() {
            ranges.push([s, idx]);
        }
    }
    if let Some(s) = start {
        ranges.push([s, count.len()]);
    }
    ranges
}

#[derive(Default)]
struct PsdProcessor {
    fft_size: usize,
    fft: Option<Arc<dyn rustfft::Fft<f32>>>,
    window: Vec<f32>,
    window_power: f32,
    buf: Vec<Complex32>,
    scratch: Vec<Complex32>,
    mapping: Vec<usize>,
}

impl PsdProcessor {
    fn ensure_fft(&mut self, fft_size: usize) {
        if self.fft_size == fft_size {
            return;
        }
        let fft = FftPlanner::<f32>::new().plan_fft_forward(fft_size);
        self.window = hann_window(fft_size);
        self.window_power = self.window.iter().map(|v| v * v).sum::<f32>().max(1e-12);
        self.buf.resize(fft_size, Complex32::new(0.0, 0.0));
        self.scratch
            .resize(fft.get_inplace_scratch_len(), Complex32::new(0.0, 0.0));
        self.mapping.resize(fft_size, usize::MAX);
        self.fft = Some(fft);
        self.fft_size = fft_size;
    }

    fn process(&mut self, packet: &mut HopPacket, cancel: &AtomicBool) -> Result<bool, String> {
        if cancel.load(Ordering::Relaxed) {
            return Ok(false);
        }
        let meta = packet
            .metadata
            .as_ref()
            .ok_or("FFT hop metadata is missing")?;
        let fft_size = meta.fft_size;
        if !(256..=65_536).contains(&fft_size)
            || !fft_size.is_power_of_two()
            || !meta.sample_rate_hz.is_finite()
            || meta.sample_rate_hz <= 0.0
        {
            return Err("Invalid FFT hop parameters".to_owned());
        }
        self.ensure_fft(fft_size);
        let bins = meta.band.bin_count();
        packet.acc.resize(bins, 0.0);
        packet.acc.fill(0.0);
        packet.count.resize(bins, 0);
        packet.count.fill(0);
        let usable_half = meta.usable_bandwidth_hz * 0.5;
        let dc_guard_hz = (meta.sample_rate_hz / fft_size as f64 * 3.0).max(50_000.0);
        for (k, mapping) in self.mapping.iter_mut().enumerate() {
            *mapping = usize::MAX;
            let offset_hz = if k < fft_size / 2 {
                k as f64 * meta.sample_rate_hz / fft_size as f64
            } else {
                (k as f64 - fft_size as f64) * meta.sample_rate_hz / fft_size as f64
            };
            if offset_hz.abs() > usable_half || offset_hz.abs() < dc_guard_hz {
                continue;
            }
            let freq_hz = meta.center_hz + offset_hz;
            if freq_hz < meta.band.start_hz || freq_hz >= meta.band.stop_hz {
                continue;
            }
            let bin = ((freq_hz - meta.band.start_hz) / meta.band.bin_width_hz) as usize;
            if bin < bins {
                *mapping = bin;
            }
        }
        let fft = self.fft.as_ref().unwrap();
        let mut idx = 0;
        while idx + fft_size <= packet.samples.len() {
            if cancel.load(Ordering::Relaxed) {
                return Ok(false);
            }
            for i in 0..fft_size {
                self.buf[i] = packet.samples[idx + i] * self.window[i];
            }
            fft.process_with_scratch(&mut self.buf, &mut self.scratch);
            for (v, &bin) in self.buf.iter().zip(&self.mapping) {
                if bin == usize::MAX {
                    continue;
                }
                let linear = (v.norm_sqr() / (fft_size as f32 * self.window_power)).max(1e-20);
                let db = 10.0 * linear.log10() as f64;
                packet.acc[bin] += db;
                packet.count[bin] = packet.count[bin].saturating_add(1);
            }
            idx += (fft_size / 2).max(1);
        }
        Ok(!cancel.load(Ordering::Relaxed))
    }
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn reference_hop_psd(
    center_hz: f64,
    sample_rate_hz: f64,
    band: &BandConfig,
    sweep: &SweepConfig,
    samples: &[Complex32],
    fft_size: usize,
    window: &[f32],
    window_power: f32,
    fft: &Arc<dyn rustfft::Fft<f32>>,
    buf: &mut [Complex32],
    acc: &mut [f64],
    count: &mut [u32],
) {
    let usable_half = sweep.usable_bandwidth_hz() * 0.5;
    let dc_guard_hz = (sample_rate_hz / fft_size as f64 * 3.0).max(50_000.0);
    let hop = (fft_size / 2).max(1);
    let mut idx = 0usize;

    while idx + fft_size <= samples.len() {
        for i in 0..fft_size {
            buf[i] = samples[idx + i] * window[i];
        }
        fft.process(buf);

        for (k, v) in buf.iter().enumerate() {
            let offset_hz = if k < fft_size / 2 {
                k as f64 * sample_rate_hz / fft_size as f64
            } else {
                (k as f64 - fft_size as f64) * sample_rate_hz / fft_size as f64
            };
            if offset_hz.abs() > usable_half || offset_hz.abs() < dc_guard_hz {
                continue;
            }
            let freq_hz = center_hz + offset_hz;
            if freq_hz < band.start_hz || freq_hz >= band.stop_hz {
                continue;
            }
            let bin = ((freq_hz - band.start_hz) / band.bin_width_hz) as usize;
            if bin >= acc.len() {
                continue;
            }

            let linear = (v.norm_sqr() / (fft_size as f32 * window_power)).max(1e-20);
            let db = 10.0 * linear.log10() as f64;
            acc[bin] += db;
            count[bin] = count[bin].saturating_add(1);
        }

        idx += hop;
    }
}

fn hann_window(n: usize) -> Vec<f32> {
    if n <= 1 {
        return vec![1.0; n];
    }
    (0..n)
        .map(|i| {
            let x = i as f32 / (n - 1) as f32;
            0.5 - 0.5 * (std::f32::consts::TAU * x).cos()
        })
        .collect()
}

fn merge_stats(
    stats: &SoapyReadStats,
    overflow_count: &mut usize,
    timeout_count: &mut usize,
    corruption_count: &mut usize,
    captured_samples: &mut usize,
) {
    *overflow_count += stats.overflow_count;
    *timeout_count += stats.timeout_count;
    *corruption_count += stats.corruption_count;
    *captured_samples += stats.samples;
}

fn sleep_cancelable(duration: Duration, stop: &AtomicBool) -> bool {
    if duration.is_zero() {
        return !stop.load(Ordering::Relaxed);
    }

    let deadline = std::time::Instant::now() + duration;
    while !stop.load(Ordering::Relaxed) {
        let now = std::time::Instant::now();
        if now >= deadline {
            return true;
        }
        std::thread::sleep((deadline - now).min(Duration::from_millis(2)));
    }
    false
}

fn stream_gating_enabled(configured: bool, env_override: Option<&str>) -> bool {
    env_override
        .map(|value| {
            let value = value.trim().to_ascii_lowercase();
            matches!(value.as_str(), "1" | "true" | "yes" | "on")
        })
        .unwrap_or(configured)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::BandPreset;

    fn metadata(frame: u64, index: usize, fft_size: usize, rate: f64, center: f64) -> HopMetadata {
        HopMetadata {
            id: HopId { frame, index },
            center_hz: center,
            sample_rate_hz: rate,
            band: BandConfig {
                preset: BandPreset::Custom,
                start_hz: center - rate * 0.6,
                stop_hz: center + rate * 0.6,
                bin_width_hz: rate / 128.0,
            },
            usable_bandwidth_hz: rate * 0.8,
            fft_size,
        }
    }

    fn fill_samples(out: &mut Vec<Complex32>, len: usize, seed: u32) {
        out.clear();
        let mut state = seed;
        for i in 0..len {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let re = (state >> 8) as f32 / 16_777_216.0 - 0.5;
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let im = (state >> 8) as f32 / 16_777_216.0 - 0.5;
            let scale = if i % 1024 < 512 { 0.01 } else { 0.8 };
            out.push(Complex32::new(re, im) * scale);
        }
    }

    fn reference(packet: &HopPacket, acc: &mut [f64], count: &mut [u32]) {
        let meta = packet.metadata.as_ref().unwrap();
        let sweep = SweepConfig {
            sample_rate_hz: meta.sample_rate_hz,
            usable_bandwidth_ratio: meta.usable_bandwidth_hz / meta.sample_rate_hz,
            fft_size: meta.fft_size,
            ..SweepConfig::default()
        };
        let window = hann_window(meta.fft_size);
        let power = window.iter().map(|v| v * v).sum::<f32>().max(1e-12);
        let fft = FftPlanner::new().plan_fft_forward(meta.fft_size);
        reference_hop_psd(
            meta.center_hz,
            meta.sample_rate_hz,
            &meta.band,
            &sweep,
            &packet.samples,
            meta.fft_size,
            &window,
            power,
            &fft,
            &mut vec![Complex32::new(0.0, 0.0); meta.fft_size],
            acc,
            count,
        );
    }

    fn assert_psd_equal(acc: &[f64], count: &[u32], expected: &[f64], expected_count: &[u32]) {
        assert_eq!(count, expected_count);
        for ((sum, reference), n) in acc.iter().zip(expected).zip(count) {
            if *n > 0 {
                assert!((sum / *n as f64 - reference / *n as f64).abs() <= 1e-4);
            } else {
                assert_eq!(*sum, 0.0);
            }
        }
    }

    #[test]
    fn optimized_psd_matches_literal_reference_across_sizes_rates_and_bands() {
        let mut processor = PsdProcessor::default();
        for fft_size in [256, 1024, 4096, 65_536] {
            for (rate, center) in [(1_000_001.0, 2_440_000_037.0), (20e6, 5.5e9), (56e6, 6.2e9)] {
                for narrow in [false, true] {
                    let mut meta = metadata(1, 0, fft_size, rate, center);
                    if narrow {
                        meta.band.start_hz = center - rate * 0.137;
                        meta.band.stop_hz = center + rate * 0.271;
                        meta.band.bin_width_hz = rate / 79.0;
                        meta.usable_bandwidth_hz = rate * 0.55;
                    }
                    let bins = meta.band.bin_count();
                    let mut packet = HopPacket {
                        metadata: Some(meta),
                        ..HopPacket::default()
                    };
                    fill_samples(&mut packet.samples, fft_size * 3 + fft_size / 4, 17);
                    let mut expected = vec![0.0; bins];
                    let mut expected_count = vec![0; bins];
                    reference(&packet, &mut expected, &mut expected_count);
                    assert!(
                        processor
                            .process(&mut packet, &AtomicBool::new(false))
                            .unwrap()
                    );
                    assert_psd_equal(&packet.acc, &packet.count, &expected, &expected_count);
                    assert!(packet.count.iter().any(|&n| n > 0));
                }
            }
        }
    }

    #[test]
    fn dc_guard_band_edges_and_half_overlap_keep_exact_coverage() {
        let rate = 1_024_000.0;
        let center = 100e6;
        let mut meta = metadata(1, 0, 256, rate, center);
        meta.band.start_hz = center - rate / 2.0;
        meta.band.stop_hz = center + rate / 2.0;
        meta.band.bin_width_hz = rate / 256.0;
        let mut packet = HopPacket {
            metadata: Some(meta),
            ..HopPacket::default()
        };
        fill_samples(&mut packet.samples, 256 * 2 + 127, 42);
        let mut processor = PsdProcessor::default();
        assert!(
            processor
                .process(&mut packet, &AtomicBool::new(false))
                .unwrap()
        );
        for bin in 0..256 {
            let offset = (bin as f64 - 128.0) * 4000.0;
            let valid = offset.abs() >= 50_000.0 && offset.abs() <= rate * 0.4;
            assert_eq!(packet.count[bin], if valid { 3 } else { 0 });
        }
        assert_eq!(valid_bin_ranges(&packet.count), vec![[26, 116], [141, 231]]);
        assert_eq!(current_from_acc(&packet.acc, &packet.count)[128], -120.0);
    }

    #[test]
    fn fft_working_buffers_and_plan_are_reused() {
        let mut processor = PsdProcessor::default();
        let mut packet = HopPacket {
            metadata: Some(metadata(1, 0, 1024, 20e6, 2.45e9)),
            ..HopPacket::default()
        };
        fill_samples(&mut packet.samples, 4096, 1);
        processor
            .process(&mut packet, &AtomicBool::new(false))
            .unwrap();
        let fft = Arc::clone(processor.fft.as_ref().unwrap());
        let pointers = (
            processor.window.as_ptr(),
            processor.buf.as_ptr(),
            processor.scratch.as_ptr(),
            processor.mapping.as_ptr(),
            packet.acc.as_ptr(),
            packet.count.as_ptr(),
        );
        packet.metadata.as_mut().unwrap().center_hz += 1e6;
        processor
            .process(&mut packet, &AtomicBool::new(false))
            .unwrap();
        assert!(Arc::ptr_eq(&fft, processor.fft.as_ref().unwrap()));
        assert_eq!(
            pointers,
            (
                processor.window.as_ptr(),
                processor.buf.as_ptr(),
                processor.scratch.as_ptr(),
                processor.mapping.as_ptr(),
                packet.acc.as_ptr(),
                packet.count.as_ptr(),
            )
        );
    }

    #[test]
    fn pipeline_orders_hops_reuses_two_iq_packets_and_drains_between_configs() {
        let mut pipeline = FftPipeline::new().unwrap();
        assert_eq!(
            pipeline.thread.as_ref().unwrap().thread().name(),
            Some("sdrwispy-fft")
        );
        for packet in &mut pipeline.packets {
            packet.as_mut().unwrap().samples.reserve(32_768);
        }
        let pointers: Vec<_> = pipeline
            .packets
            .iter()
            .map(|p| p.as_ref().unwrap().samples.as_ptr())
            .collect();
        let stop = AtomicBool::new(false);
        for (frame, fft_size, rate) in [(1, 256, 20e6), (2, 4096, 56e6), (3, 1024, 1_000_001.0)] {
            pipeline.begin_frame().unwrap();
            let base = metadata(frame, 0, fft_size, rate, 2.45e9);
            let bins = base.band.bin_count();
            let mut acc = vec![0.0; bins];
            let mut count = vec![0; bins];
            let mut expected = vec![0.0; bins];
            let mut expected_count = vec![0; bins];
            let mut merged = Vec::new();
            for index in 0..4 {
                let slot = pipeline.capture_slot().unwrap();
                let mut meta = metadata(
                    frame,
                    index,
                    fft_size,
                    rate,
                    2.45e9 + index as f64 * rate * 0.05,
                );
                meta.band = base.band.clone();
                let packet = pipeline.packets[slot].as_mut().unwrap();
                fill_samples(&mut packet.samples, fft_size * 3 + 13, index as u32 + 1);
                packet.metadata = Some(meta);
                reference(packet, &mut expected, &mut expected_count);
                let meta = packet.metadata.take().unwrap();
                if index > 0 {
                    assert!(pipeline.begin_frame().is_err());
                    assert!(
                        pipeline
                            .submit(slot, metadata(frame, index, fft_size, rate, 0.0))
                            .is_err()
                    );
                }
                if let Some(previous) = pipeline.drain(&stop).unwrap() {
                    assert_eq!(
                        previous.metadata.as_ref().unwrap().id,
                        HopId {
                            frame,
                            index: index - 1
                        }
                    );
                    merged.push(index - 1);
                    merge_hop(previous, frame, &mut acc, &mut count).unwrap();
                }
                pipeline.submit(slot, meta).unwrap();
            }
            let last = pipeline.drain(&stop).unwrap().unwrap();
            merged.push(last.metadata.as_ref().unwrap().id.index);
            merge_hop(last, frame, &mut acc, &mut count).unwrap();
            assert!(merge_hop(last, frame + 1, &mut acc, &mut count).is_err());
            assert_eq!(merged, vec![0, 1, 2, 3]);
            assert_psd_equal(&acc, &count, &expected, &expected_count);
            assert!(pipeline.pending.is_none());
            for (slot, pointer) in pointers.iter().enumerate() {
                assert_eq!(
                    pipeline.packets[slot].as_ref().unwrap().samples.as_ptr(),
                    *pointer
                );
            }
        }
    }

    #[test]
    fn cancellation_discards_pending_results_and_allows_a_clean_next_frame() {
        let mut pipeline = FftPipeline::new().unwrap();
        for frame in 1..=3 {
            pipeline.begin_frame().unwrap();
            let slot = pipeline.capture_slot().unwrap();
            fill_samples(
                &mut pipeline.packets[slot].as_mut().unwrap().samples,
                65_536,
                5,
            );
            pipeline
                .submit(slot, metadata(frame, 0, 4096, 20e6, 2.45e9))
                .unwrap();
            if frame == 1 {
                pipeline.cancel_and_drain().unwrap();
            } else if frame == 2 {
                assert!(pipeline.drain(&AtomicBool::new(true)).unwrap().is_none());
            } else {
                let result = pipeline.drain(&AtomicBool::new(false)).unwrap().unwrap();
                assert_eq!(result.metadata.as_ref().unwrap().id.frame, 3);
                assert!(result.count.iter().any(|&n| n > 0));
            }
            assert!(pipeline.pending.is_none());
            assert!(pipeline.packets.iter().all(Option::is_some));
        }
    }

    #[test]
    fn canceled_processor_does_not_touch_an_old_packet() {
        let mut packet = HopPacket {
            acc: vec![17.0],
            count: vec![9],
            metadata: Some(metadata(1, 0, 256, 20e6, 2.45e9)),
            ..HopPacket::default()
        };
        assert!(
            !PsdProcessor::default()
                .process(&mut packet, &AtomicBool::new(true))
                .unwrap()
        );
        assert_eq!(packet.acc, vec![17.0]);
        assert_eq!(packet.count, vec![9]);
    }

    #[test]
    fn worker_error_returns_the_packet_and_can_be_drained() {
        let mut pipeline = FftPipeline::new().unwrap();
        pipeline.begin_frame().unwrap();
        pipeline
            .submit(0, metadata(1, 0, 257, 20e6, 2.45e9))
            .unwrap();
        assert!(
            pipeline
                .drain(&AtomicBool::new(false))
                .err()
                .unwrap()
                .contains("Invalid FFT")
        );
        assert!(pipeline.pending.is_none());
        assert!(pipeline.packets.iter().all(Option::is_some));
        pipeline.cancel_and_drain().unwrap();
        pipeline.begin_frame().unwrap();
        let slot = pipeline.capture_slot().unwrap();
        fill_samples(
            &mut pipeline.packets[slot].as_mut().unwrap().samples,
            1024,
            6,
        );
        pipeline
            .submit(slot, metadata(2, 0, 256, 20e6, 2.45e9))
            .unwrap();
        assert!(pipeline.drain(&AtomicBool::new(false)).unwrap().is_some());
    }

    fn shutdown_without_hanging(mut pipeline: FftPipeline) {
        let (done, joined) = mpsc::sync_channel(1);
        let thread = thread::spawn(move || {
            pipeline.shutdown();
            pipeline.shutdown();
            assert!(pipeline.thread.is_none());
            assert!(pipeline.input.is_none());
            assert!(pipeline.output.is_none());
            done.send(()).unwrap();
        });
        joined
            .recv_timeout(Duration::from_secs(5))
            .expect("shutdown must not deadlock");
        thread.join().unwrap();
    }

    #[test]
    fn shutdown_joins_with_a_pending_job() {
        let mut pipeline = FftPipeline::new().unwrap();
        fill_samples(
            &mut pipeline.packets[0].as_mut().unwrap().samples,
            262_144,
            7,
        );
        pipeline
            .submit(0, metadata(1, 0, 65_536, 56e6, 2.45e9))
            .unwrap();
        shutdown_without_hanging(pipeline);
        shutdown_without_hanging(FftPipeline::new().unwrap());
    }

    #[test]
    fn shutdown_closes_a_full_output_channel_before_joining() {
        let mut pipeline = FftPipeline::new().unwrap();
        pipeline.shutdown();
        let (input, jobs) = mpsc::sync_channel::<HopPacket>(1);
        let (results, output) = mpsc::sync_channel(1);
        let (ready, filled) = mpsc::sync_channel(1);
        pipeline.input = Some(input);
        pipeline.output = Some(output);
        let second = pipeline.packets[1].take().unwrap();
        pipeline.thread = Some(thread::spawn(move || {
            let packet = jobs.recv().unwrap();
            results
                .send(HopResult {
                    packet,
                    completed: Ok(true),
                })
                .ok()
                .unwrap();
            ready.send(()).unwrap();
            // Simulate a faulty producer blocked on an already-full result channel.
            assert!(
                results
                    .send(HopResult {
                        packet: second,
                        completed: Ok(true)
                    })
                    .is_err()
            );
        }));
        pipeline
            .submit(0, metadata(1, 0, 256, 20e6, 2.45e9))
            .unwrap();
        filled.recv_timeout(Duration::from_secs(5)).unwrap();
        shutdown_without_hanging(pipeline);
    }

    #[test]
    fn disconnected_worker_returns_an_error_without_waiting_forever() {
        let mut pipeline = FftPipeline::new().unwrap();
        pipeline.shutdown();
        let (input, jobs) = mpsc::sync_channel::<HopPacket>(1);
        let (results, output) = mpsc::sync_channel::<HopResult>(1);
        pipeline.input = Some(input);
        pipeline.output = Some(output);
        pipeline.thread = Some(thread::spawn(move || {
            let _ = jobs.recv();
            drop(results);
        }));
        pipeline
            .submit(0, metadata(1, 0, 256, 20e6, 2.45e9))
            .unwrap();
        assert!(
            pipeline
                .drain(&AtomicBool::new(false))
                .err()
                .unwrap()
                .contains("disconnected")
        );
        assert!(pipeline.pending.is_none());
        shutdown_without_hanging(pipeline);
    }

    #[test]
    fn tune_reuse_requires_continuous_single_hop_same_request_and_active_stream() {
        let cached = Some(TunePair {
            requested_hz: 2.45e9,
            actual_hz: 2.45e9 + 3.0,
        });
        assert_eq!(
            reusable_tune(cached, false, 1, 2.45e9, true),
            Some(2.45e9 + 3.0)
        );
        assert!(reusable_tune(None, false, 1, 2.45e9, true).is_none());
        assert!(reusable_tune(cached, true, 1, 2.45e9, true).is_none());
        assert!(reusable_tune(cached, false, 2, 2.45e9, true).is_none());
        assert!(reusable_tune(cached, false, 1, 2.45e9 + 1.0, true).is_none());
        assert!(reusable_tune(cached, false, 1, 2.45e9, false).is_none());
    }

    #[test]
    fn history_updates_only_valid_bins_and_initializes_unclipped_true_peaks() {
        let mut average = vec![-110.0; 3];
        let mut peak = vec![-120.0; 3];
        let mut initialized = vec![false; 3];
        update_history(
            &[-150.0, -90.0, -30.0],
            &[1, 1, 0],
            &mut average,
            &mut peak,
            &mut initialized,
        );
        assert_eq!(peak, vec![-150.0, -90.0, -120.0]);
        assert_eq!(average, vec![-114.0, -108.0, -110.0]);
        update_history(
            &[-160.0, -50.0, -180.0],
            &[1, 0, 1],
            &mut average,
            &mut peak,
            &mut initialized,
        );
        assert_eq!(peak, vec![-150.0, -90.0, -180.0]);
        assert_eq!(average[1], -108.0);
        update_history(
            &[-100.0, -50.0, -120.0],
            &[1, 1, 0],
            &mut average,
            &mut peak,
            &mut initialized,
        );
        assert_eq!(peak, vec![-100.0, -50.0, -180.0]);
    }

    #[test]
    fn stream_gating_uses_profile_unless_environment_overrides_it() {
        for configured in [false, true] {
            assert_eq!(stream_gating_enabled(configured, None), configured);
            for value in ["1", "true", "yes", "on", " TRUE "] {
                assert!(stream_gating_enabled(configured, Some(value)));
            }
            // Preserve the existing override: all other explicit values disable gating.
            for value in ["0", "false", "no", "off", "", "unknown"] {
                assert!(!stream_gating_enabled(configured, Some(value)));
            }
        }
    }
}
