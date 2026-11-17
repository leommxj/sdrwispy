use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use num_complex::Complex32;
use rustfft::FftPlanner;

use crate::model::{BandConfig, SpectrumFrame, SweepConfig, now_ns};
use crate::soapy::{SoapyReadStats, SoapyRx, SoapyRxConfig};

pub struct RealSweepEngine {
    rx: SoapyRx,
    average_db: Vec<f32>,
    max_hold_db: Vec<f32>,
    cached_fft_size: usize,
    cached_fft: Option<Arc<dyn rustfft::Fft<f32>>>,
    cached_window: Vec<f32>,
    cached_window_power: f32,
    frame_id: u64,
    frame_revision: u64,
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
            average_db: Vec::new(),
            max_hold_db: Vec::new(),
            cached_fft_size: 0,
            cached_fft: None,
            cached_window: Vec::new(),
            cached_window_power: 1.0,
            frame_id: 0,
            frame_revision: 0,
        })
    }

    pub fn receiver_status(&self) -> &str {
        self.rx.receiver_status()
    }

    pub fn pause(&mut self) -> Result<(), String> {
        self.rx.deactivate()
    }

    pub fn shutdown(&mut self) {
        self.rx.shutdown();
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
        if stop.load(Ordering::Relaxed) {
            return Ok(None);
        }

        sweep.validate()?;
        // Small accepted hardware rounding must also feed hop planning, not just the FFT.
        let mut actual_sweep = sweep.clone();
        actual_sweep.sample_rate_hz = self.rx.sample_rate_hz();
        let sweep = &actual_sweep;
        let bins = band.bin_count();
        if self.average_db.len() != bins {
            self.average_db = vec![-110.0; bins];
            self.max_hold_db = vec![-120.0; bins];
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

        self.ensure_fft(fft_size);
        let fft = self
            .cached_fft
            .as_ref()
            .expect("FFT cache was initialized")
            .clone();
        let window = self.cached_window.clone();
        let window_power = self.cached_window_power;
        let mut fft_buf = vec![Complex32::new(0.0, 0.0); fft_size];

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
            self.rx.deactivate()?;
        }
        for center in &hop_centers {
            if stop.load(Ordering::Relaxed) {
                let _ = self.rx.deactivate();
                return Ok(None);
            }

            if gate_stream {
                self.rx.deactivate()?;
            }
            let actual_center = self.rx.tune(*center)?;
            if !sleep_cancelable(
                Duration::from_secs_f64(sweep.settle_ms.max(0.0) / 1000.0),
                stop,
            ) {
                let _ = self.rx.deactivate();
                return Ok(None);
            }

            if gate_stream || !self.rx.is_active() {
                self.rx.activate()?;
            }
            if discard_samples > 0 {
                let discard_result = self.rx.discard(discard_samples, read_timeout_us, stop);
                let deactivate_result = if gate_stream && stop.load(Ordering::Relaxed) {
                    self.rx.deactivate()
                } else {
                    Ok(())
                };
                let s = match discard_result {
                    Ok(s) => s,
                    Err(err) => {
                        if gate_stream {
                            let _ = self.rx.deactivate();
                        }
                        return Err(err);
                    }
                };
                deactivate_result?;
                if stop.load(Ordering::Relaxed) {
                    let _ = self.rx.deactivate();
                    return Ok(None);
                }
                merge_stats(
                    &s,
                    &mut overflow_count,
                    &mut timeout_count,
                    &mut corruption_count,
                    &mut captured_samples,
                );
            }

            let read_result = self
                .rx
                .read_exactish(samples_per_hop, read_timeout_us, stop);
            let deactivate_result = if gate_stream {
                self.rx.deactivate()
            } else {
                Ok(())
            };
            let (samples, stats) = match read_result {
                Ok(v) => v,
                Err(err) => {
                    let _ = deactivate_result;
                    return Err(err);
                }
            };
            deactivate_result?;
            if stop.load(Ordering::Relaxed) {
                let _ = self.rx.deactivate();
                return Ok(None);
            }
            merge_stats(
                &stats,
                &mut overflow_count,
                &mut timeout_count,
                &mut corruption_count,
                &mut captured_samples,
            );
            if samples.len() < fft_size {
                continue;
            }
            process_hop_psd(
                actual_center,
                sample_rate,
                band,
                sweep,
                &samples,
                fft_size,
                &window,
                window_power,
                &fft,
                &mut fft_buf,
                &mut acc,
                &mut count,
            );
            completed_hops += 1;
            if completed_hops == 1 || last_progress.elapsed() >= progress_interval {
                self.frame_revision = self.frame_revision.wrapping_add(1);
                on_progress(build_spectrum_frame(
                    band,
                    &acc,
                    &count,
                    &self.average_db,
                    &self.max_hold_db,
                    next_frame_id,
                    self.frame_revision,
                    true,
                    start_ns,
                    hop_centers.len(),
                    completed_hops,
                ));
                last_progress = Instant::now();
            }
        }

        if gate_stream {
            self.rx.deactivate()?;
        }

        let current = current_from_acc(&acc, &count);
        let valid_bin_count = count.iter().filter(|&&v| v > 0).count();
        let invalid = bins.saturating_sub(valid_bin_count);
        let (min_db, max_db) = min_max_db(&current, &count);
        for i in 0..bins {
            self.average_db[i] = self.average_db[i] * 0.90 + current[i] * 0.10;
            self.max_hold_db[i] = (self.max_hold_db[i] - 0.05).max(current[i]);
        }
        let stream_reset = valid_bin_count == 0 && timeout_count > 0;
        if stream_reset {
            // A full sweep of timeouts means the Soapy/UHD stream accepted the
            // open/activate calls but delivered no RX buffers. Reset the stream
            // state so the next sweep performs tune -> activate again instead
            // of staying in a permanently timed-out active stream.
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
            completed_hops: hop_centers.len(),
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

    fn ensure_fft(&mut self, fft_size: usize) {
        if self.cached_fft_size == fft_size && self.cached_fft.is_some() {
            return;
        }
        let mut planner = FftPlanner::<f32>::new();
        self.cached_fft = Some(planner.plan_fft_forward(fft_size));
        self.cached_window = hann_window(fft_size);
        self.cached_window_power = self
            .cached_window
            .iter()
            .map(|v| v * v)
            .sum::<f32>()
            .max(1e-12);
        self.cached_fft_size = fft_size;
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

#[allow(clippy::too_many_arguments)]
fn process_hop_psd(
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
