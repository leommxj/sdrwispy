use std::collections::HashSet;
use std::ffi::{CStr, CString, c_char, c_int, c_long, c_longlong, c_void};
use std::path::{Path, PathBuf};
use std::sync::{
    OnceLock,
    atomic::{AtomicBool, Ordering},
};

use libloading::Library;
use num_complex::Complex32;

use crate::model::SdrDeviceInfo;

const SOAPY_SDR_RX: c_int = 1;
const SOAPY_SDR_TIMEOUT: c_int = -1;
const SOAPY_SDR_OVERFLOW: c_int = -4;
const SOAPY_SDR_CORRUPTION: c_int = -3;

#[repr(C)]
struct SoapySdrKwargs {
    size: usize,
    keys: *mut *mut c_char,
    vals: *mut *mut c_char,
}

type EnumerateStrArgs = unsafe extern "C" fn(*const c_char, *mut usize) -> *mut SoapySdrKwargs;
type KwargsListClear = unsafe extern "C" fn(*mut SoapySdrKwargs, usize);
type DeviceMakeStrArgs = unsafe extern "C" fn(*const c_char) -> *mut c_void;
type DeviceUnmake = unsafe extern "C" fn(*mut c_void) -> c_int;
type LastStatus = unsafe extern "C" fn() -> c_int;
type GetRxValue = unsafe extern "C" fn(*const c_void, c_int, usize) -> f64;
type LastError = unsafe extern "C" fn() -> *const c_char;
type SetSampleRate = unsafe extern "C" fn(*mut c_void, c_int, usize, f64) -> c_int;
type SetBandwidth = unsafe extern "C" fn(*mut c_void, c_int, usize, f64) -> c_int;
type SetGain = unsafe extern "C" fn(*mut c_void, c_int, usize, f64) -> c_int;
type SetGainMode = unsafe extern "C" fn(*mut c_void, c_int, usize, bool) -> c_int;
type SetFrequency =
    unsafe extern "C" fn(*mut c_void, c_int, usize, f64, *const SoapySdrKwargs) -> c_int;
type SetupStream = unsafe extern "C" fn(
    *mut c_void,
    c_int,
    *const c_char,
    *const usize,
    usize,
    *const SoapySdrKwargs,
) -> *mut c_void;
type CloseStream = unsafe extern "C" fn(*mut c_void, *mut c_void) -> c_int;
type ActivateStream =
    unsafe extern "C" fn(*mut c_void, *mut c_void, c_int, c_longlong, usize) -> c_int;
type DeactivateStream = unsafe extern "C" fn(*mut c_void, *mut c_void, c_int, c_longlong) -> c_int;
type ReadStream = unsafe extern "C" fn(
    *mut c_void,
    *mut c_void,
    *const *mut c_void,
    usize,
    *mut c_int,
    *mut c_longlong,
    c_long,
) -> c_int;
type GetStreamMtu = unsafe extern "C" fn(*const c_void, *mut c_void) -> usize;

#[derive(Debug, Clone)]
pub struct SoapyRxConfig {
    pub args: String,
    pub sample_rate_hz: f64,
    pub bandwidth_hz: f64,
    pub gain_db: f64,
}

#[derive(Debug, Clone)]
pub struct SoapyReadStats {
    pub samples: usize,
    pub overflow_count: usize,
    pub timeout_count: usize,
    pub corruption_count: usize,
    pub flags: c_int,
    pub time_ns: c_longlong,
}

pub struct SoapyRx {
    lib: &'static Library,
    device: *mut c_void,
    stream: *mut c_void,
    mtu: usize,
    active: bool,
    config: SoapyRxConfig,
    actual_sample_rate_hz: f64,
    receiver_status: String,
}

fn positive_value(name: &str, value: f64) -> Result<f64, String> {
    if !value.is_finite() || value <= 0.0 {
        return Err(format!("{name} must be finite and positive, got {value}"));
    }
    Ok(value)
}

fn decode_readback(
    operation: &str,
    value: f64,
    status: c_int,
    error: impl FnOnce() -> String,
) -> Result<f64, String> {
    if status != 0 {
        Err(format!("{operation} failed (status {status}): {}", error()))
    } else if !value.is_finite() {
        Err(format!("{operation} returned non-finite value {value}"))
    } else {
        Ok(value)
    }
}

fn checked_readback(
    name: &str,
    requested: f64,
    actual: Result<f64, String>,
    tolerance_hz: f64,
) -> Result<f64, String> {
    positive_value(name, requested)?;
    let actual =
        actual.map_err(|e| format!("Cannot verify {name} requested {requested} Hz: {e}"))?;
    positive_value(&format!("actual {name}"), actual)?;
    if (actual - requested).abs() > tolerance_hz {
        return Err(format!(
            "RX {name} mismatch: requested {requested} Hz, actual {actual} Hz. \
             Select a supported sample rate / frequency range and check the device band and control connection; capture stopped to avoid incorrect spectrum coordinates."
        ));
    }
    Ok(actual)
}

fn checked_sample_rate(requested: f64, actual: Result<f64, String>) -> Result<f64, String> {
    checked_readback(
        "sample rate",
        requested,
        actual,
        (requested.abs() * 1.0e-6).max(1.0),
    )
}

fn optional_readback(value: Result<f64, String>, positive: bool) -> String {
    match value {
        Ok(value) if value.is_finite() && (!positive || value > 0.0) => value.to_string(),
        Ok(value) => format!("unknown (unsupported/invalid readback {value})"),
        Err(error) => format!("unknown ({error})"),
    }
}

impl SoapyRx {
    pub fn open(config: SoapyRxConfig) -> Result<Self, String> {
        positive_value("sample rate", config.sample_rate_hz)?;
        positive_value("bandwidth", config.bandwidth_hz)?;
        if !config.gain_db.is_finite() {
            return Err("RX gain must be finite (units are driver-defined)".to_owned());
        }
        let (lib, _) = load_library().ok_or_else(|| {
            "SoapySDR runtime not found. Set SDRWISPY_SOAPY_ROOT or use a SoapySDR-enabled environment."
                .to_owned()
        })?;

        // Keep SoapyRx::open literal: callers decide whether to add UHD/Soapy
        // transport args. This is important for the Tauri frontend, where all
        // additional device arguments should be visible/editable in the GUI
        // instead of being injected implicitly through process environment.
        let effective_args = config.args.trim().to_owned();
        let args = CString::new(effective_args.clone()).map_err(|_| "Soapy args contain NUL")?;
        let make: libloading::Symbol<'_, DeviceMakeStrArgs> = unsafe {
            lib.get(b"SoapySDRDevice_makeStrArgs\0")
                .map_err(|e| e.to_string())?
        };
        let device = unsafe { make(args.as_ptr()) };
        if device.is_null() {
            return Err(format!(
                "SoapySDRDevice_makeStrArgs failed: {}",
                last_error(lib)
            ));
        }

        let mut rx = Self {
            lib,
            device,
            stream: std::ptr::null_mut(),
            mtu: 4096,
            active: false,
            actual_sample_rate_hz: 0.0,
            receiver_status: String::new(),
            config: SoapyRxConfig {
                args: effective_args,
                ..config
            },
        };

        rx.configure_device()?;
        rx.setup_stream()?;
        Ok(rx)
    }

    pub fn sample_rate_hz(&self) -> f64 {
        self.actual_sample_rate_hz
    }

    pub fn receiver_status(&self) -> &str {
        &self.receiver_status
    }

    pub fn tune(&mut self, center_freq_hz: f64) -> Result<f64, String> {
        positive_value("center frequency", center_freq_hz)?;
        let set_frequency: libloading::Symbol<'_, SetFrequency> = unsafe {
            self.lib
                .get(b"SoapySDRDevice_setFrequency\0")
                .map_err(|e| e.to_string())?
        };
        let ret = unsafe {
            set_frequency(
                self.device,
                SOAPY_SDR_RX,
                0,
                center_freq_hz,
                std::ptr::null(),
            )
        };
        self.check_ret(ret, "setFrequency")?;
        checked_readback(
            "center frequency",
            center_freq_hz,
            self.read_rx_value(b"SoapySDRDevice_getFrequency\0", "getFrequency"),
            100.0,
        )
    }

    pub fn discard(
        &mut self,
        samples: usize,
        timeout_us: i64,
        stop: &AtomicBool,
    ) -> Result<SoapyReadStats, String> {
        let mut tmp = vec![Complex32::new(0.0, 0.0); self.read_chunk_len().min(samples.max(1))];
        let mut remaining = samples;
        let mut stats = SoapyReadStats {
            samples: 0,
            overflow_count: 0,
            timeout_count: 0,
            corruption_count: 0,
            flags: 0,
            time_ns: 0,
        };
        let mut empty_reads = 0usize;
        while remaining > 0 && !stop.load(Ordering::Relaxed) {
            let n = tmp.len().min(remaining);
            let read = self.read_into(&mut tmp[..n], timeout_us)?;
            stats.samples += read.samples;
            stats.overflow_count += read.overflow_count;
            stats.timeout_count += read.timeout_count;
            stats.corruption_count += read.corruption_count;
            stats.flags = read.flags;
            stats.time_ns = read.time_ns;
            if read.samples == 0 {
                empty_reads += 1;
                if empty_reads >= 16 {
                    break;
                }
                continue;
            }
            empty_reads = 0;
            remaining -= read.samples.min(remaining);
        }
        Ok(stats)
    }

    pub fn read_exactish(
        &mut self,
        samples: usize,
        timeout_us: i64,
        stop: &AtomicBool,
    ) -> Result<(Vec<Complex32>, SoapyReadStats), String> {
        let mut out = vec![Complex32::new(0.0, 0.0); samples];
        let mut offset = 0usize;
        let mut stats = SoapyReadStats {
            samples: 0,
            overflow_count: 0,
            timeout_count: 0,
            corruption_count: 0,
            flags: 0,
            time_ns: 0,
        };
        let mut empty_reads = 0usize;

        while offset < samples && !stop.load(Ordering::Relaxed) {
            let chunk = self.read_chunk_len().min(samples - offset);
            let read = self.read_into(&mut out[offset..offset + chunk], timeout_us)?;
            stats.samples += read.samples;
            stats.overflow_count += read.overflow_count;
            stats.timeout_count += read.timeout_count;
            stats.corruption_count += read.corruption_count;
            stats.flags = read.flags;
            stats.time_ns = read.time_ns;
            if read.samples == 0 {
                empty_reads += 1;
                if empty_reads >= 16 {
                    break;
                }
                continue;
            }
            empty_reads = 0;
            offset += read.samples;
        }

        out.truncate(offset);
        Ok((out, stats))
    }

    pub fn activate(&mut self) -> Result<(), String> {
        if self.active {
            return Ok(());
        }
        let activate: libloading::Symbol<'_, ActivateStream> = unsafe {
            self.lib
                .get(b"SoapySDRDevice_activateStream\0")
                .map_err(|e| e.to_string())?
        };
        let ret = unsafe { activate(self.device, self.stream, 0, 0, 0) };
        self.check_ret(ret, "activateStream")?;
        self.active = true;
        Ok(())
    }

    pub fn deactivate(&mut self) -> Result<(), String> {
        if !self.active {
            return Ok(());
        }
        let deactivate: libloading::Symbol<'_, DeactivateStream> = unsafe {
            self.lib
                .get(b"SoapySDRDevice_deactivateStream\0")
                .map_err(|e| e.to_string())?
        };
        let ret = unsafe { deactivate(self.device, self.stream, 0, 0) };
        self.check_ret(ret, "deactivateStream")?;
        self.active = false;
        Ok(())
    }

    pub fn shutdown(&mut self) {
        unsafe {
            if !self.stream.is_null() {
                if self.active {
                    if let Ok(deactivate) = self
                        .lib
                        .get::<DeactivateStream>(b"SoapySDRDevice_deactivateStream\0")
                    {
                        let _ = deactivate(self.device, self.stream, 0, 0);
                    }
                    self.active = false;
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                if let Ok(close) = self.lib.get::<CloseStream>(b"SoapySDRDevice_closeStream\0") {
                    let _ = close(self.device, self.stream);
                }
                self.stream = std::ptr::null_mut();
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            if !self.device.is_null() {
                if let Ok(unmake) = self.lib.get::<DeviceUnmake>(b"SoapySDRDevice_unmake\0") {
                    let _ = unmake(self.device);
                }
                self.device = std::ptr::null_mut();
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    fn configure_device(&mut self) -> Result<(), String> {
        let set_sample_rate: libloading::Symbol<'_, SetSampleRate> = unsafe {
            self.lib
                .get(b"SoapySDRDevice_setSampleRate\0")
                .map_err(|e| e.to_string())?
        };
        let ret =
            unsafe { set_sample_rate(self.device, SOAPY_SDR_RX, 0, self.config.sample_rate_hz) };
        self.check_ret(ret, "setSampleRate")?;

        if self.config.bandwidth_hz.is_finite() && self.config.bandwidth_hz > 0.0 {
            let set_bandwidth: libloading::Symbol<'_, SetBandwidth> = unsafe {
                self.lib
                    .get(b"SoapySDRDevice_setBandwidth\0")
                    .map_err(|e| e.to_string())?
            };
            let ret =
                unsafe { set_bandwidth(self.device, SOAPY_SDR_RX, 0, self.config.bandwidth_hz) };
            // Some modules do not implement bandwidth. Keep going if unsupported.
            if ret != 0 {
                eprintln!("[WARN] SoapySDR setBandwidth failed: {}", self.last_error());
            }
        }

        let set_gain_mode: Result<libloading::Symbol<'_, SetGainMode>, _> =
            unsafe { self.lib.get(b"SoapySDRDevice_setGainMode\0") };
        if let Ok(set_gain_mode) = set_gain_mode {
            let _ = unsafe { set_gain_mode(self.device, SOAPY_SDR_RX, 0, false) };
        }

        if self.config.gain_db.is_finite() {
            let set_gain: libloading::Symbol<'_, SetGain> = unsafe {
                self.lib
                    .get(b"SoapySDRDevice_setGain\0")
                    .map_err(|e| e.to_string())?
            };
            let ret = unsafe { set_gain(self.device, SOAPY_SDR_RX, 0, self.config.gain_db) };
            if ret != 0 {
                eprintln!("[WARN] SoapySDR setGain failed: {}", self.last_error());
            }
        }

        self.actual_sample_rate_hz = checked_sample_rate(
            self.config.sample_rate_hz,
            self.read_rx_value(b"SoapySDRDevice_getSampleRate\0", "getSampleRate"),
        )?;

        let bandwidth = optional_readback(
            self.read_rx_value(b"SoapySDRDevice_getBandwidth\0", "getBandwidth"),
            true,
        );
        let gain = optional_readback(
            self.read_rx_value(b"SoapySDRDevice_getGain\0", "getGain"),
            false,
        );
        self.receiver_status = format!(
            "RX actual: rate {} MS/s, bandwidth {} Hz (requested {}), gain {} (requested {}; driver units)",
            self.actual_sample_rate_hz / 1.0e6,
            bandwidth,
            self.config.bandwidth_hz,
            gain,
            self.config.gain_db,
        );
        eprintln!("{}", self.receiver_status);
        Ok(())
    }

    fn read_rx_value(&self, symbol: &[u8], operation: &str) -> Result<f64, String> {
        let getter: libloading::Symbol<'_, GetRxValue> = unsafe {
            self.lib
                .get(symbol)
                .map_err(|e| format!("{operation}: {e}"))?
        };
        let last_status: libloading::Symbol<'_, LastStatus> = unsafe {
            self.lib
                .get(b"SoapySDRDevice_lastStatus\0")
                .map_err(|e| e.to_string())?
        };
        let value = unsafe { getter(self.device, SOAPY_SDR_RX, 0) };
        // Numeric C getters return a sentinel on exceptions; lastError alone can be stale.
        let status = unsafe { last_status() };
        decode_readback(operation, value, status, || self.last_error())
    }

    fn setup_stream(&mut self) -> Result<(), String> {
        let setup: libloading::Symbol<'_, SetupStream> = unsafe {
            self.lib
                .get(b"SoapySDRDevice_setupStream\0")
                .map_err(|e| e.to_string())?
        };
        let format = CString::new("CF32").expect("literal has no NUL");
        let channel = [0usize];
        let stream = unsafe {
            setup(
                self.device,
                SOAPY_SDR_RX,
                format.as_ptr(),
                channel.as_ptr(),
                channel.len(),
                std::ptr::null(),
            )
        };
        if stream.is_null() {
            return Err(format!("setupStream(CF32) failed: {}", self.last_error()));
        }
        self.stream = stream;

        let get_mtu: libloading::Symbol<'_, GetStreamMtu> = unsafe {
            self.lib
                .get(b"SoapySDRDevice_getStreamMTU\0")
                .map_err(|e| e.to_string())?
        };
        self.mtu = unsafe { get_mtu(self.device, self.stream) }.clamp(1024, 1_048_576);
        // Do not activate here. The sweep engine knows the first hop center;
        // activating only after the first tune avoids a stale "streaming before
        // frequency is set" state that some Soapy/UHD stacks can get into after
        // a close/open cycle.
        Ok(())
    }

    fn read_chunk_len(&self) -> usize {
        let env_cap = std::env::var("SDRWISPY_READ_CHUNK_SAMPLES")
            .ok()
            .and_then(|value| value.trim().parse::<usize>().ok())
            .unwrap_or(32_768)
            .clamp(1024, 262_144);
        self.mtu.clamp(1024, env_cap)
    }

    fn read_into(
        &mut self,
        dst: &mut [Complex32],
        timeout_us: i64,
    ) -> Result<SoapyReadStats, String> {
        let read_stream: libloading::Symbol<'_, ReadStream> = unsafe {
            self.lib
                .get(b"SoapySDRDevice_readStream\0")
                .map_err(|e| e.to_string())?
        };
        let mut flags: c_int = 0;
        let mut time_ns: c_longlong = 0;
        let mut buff = dst.as_mut_ptr() as *mut c_void;
        let ret = unsafe {
            read_stream(
                self.device,
                self.stream,
                &mut buff as *mut *mut c_void as *const *mut c_void,
                dst.len(),
                &mut flags as *mut c_int,
                &mut time_ns as *mut c_longlong,
                timeout_us as c_long,
            )
        };

        if ret >= 0 {
            Ok(SoapyReadStats {
                samples: ret as usize,
                overflow_count: 0,
                timeout_count: 0,
                corruption_count: 0,
                flags,
                time_ns,
            })
        } else if ret == SOAPY_SDR_TIMEOUT {
            Ok(SoapyReadStats {
                samples: 0,
                overflow_count: 0,
                timeout_count: 1,
                corruption_count: 0,
                flags,
                time_ns,
            })
        } else if ret == SOAPY_SDR_OVERFLOW {
            Ok(SoapyReadStats {
                samples: 0,
                overflow_count: 1,
                timeout_count: 0,
                corruption_count: 0,
                flags,
                time_ns,
            })
        } else if ret == SOAPY_SDR_CORRUPTION {
            Ok(SoapyReadStats {
                samples: 0,
                overflow_count: 0,
                timeout_count: 0,
                corruption_count: 1,
                flags,
                time_ns,
            })
        } else {
            Err(format!("readStream failed ({ret}): {}", self.last_error()))
        }
    }

    fn check_ret(&self, ret: c_int, op: &str) -> Result<(), String> {
        if ret == 0 {
            Ok(())
        } else {
            Err(format!("{op} failed ({ret}): {}", self.last_error()))
        }
    }

    fn last_error(&self) -> String {
        last_error(self.lib)
    }
}

impl Drop for SoapyRx {
    fn drop(&mut self) {
        self.shutdown();
    }
}

pub fn enumerate_devices() -> Vec<SdrDeviceInfo> {
    eprintln!("[sdrwispy] loading SoapySDR runtime for device enumeration");
    let Some((lib, loaded_from)) = load_library() else {
        eprintln!("[sdrwispy] SoapySDR runtime not found");
        return vec![SdrDeviceInfo {
            id: "soapy-unavailable".to_owned(),
            label: "SoapySDR runtime not found".to_owned(),
            driver: "soapy-info".to_owned(),
        }];
    };
    eprintln!(
        "[sdrwispy] SoapySDR runtime loaded from {}",
        loaded_from.display()
    );

    let mut devices = unsafe { enumerate_devices_from_library(lib).unwrap_or_default() };
    if devices.is_empty() {
        devices.push(SdrDeviceInfo {
            id: loaded_from.display().to_string(),
            label: format!(
                "SoapySDR loaded from {}, but no devices were enumerated",
                loaded_from.display()
            ),
            driver: "soapy-info".to_owned(),
        });
    }
    devices
}

unsafe fn enumerate_devices_from_library(lib: &Library) -> Result<Vec<SdrDeviceInfo>, String> {
    let enumerate: libloading::Symbol<'_, EnumerateStrArgs> = unsafe {
        lib.get(b"SoapySDRDevice_enumerateStrArgs\0")
            .map_err(|e| e.to_string())?
    };
    let clear: libloading::Symbol<'_, KwargsListClear> = unsafe {
        lib.get(b"SoapySDRKwargsList_clear\0")
            .map_err(|e| e.to_string())?
    };

    let empty = CString::new("").expect("empty string has no NUL");
    let mut len = 0usize;
    let ptr = unsafe { enumerate(empty.as_ptr(), &mut len as *mut usize) };
    if ptr.is_null() || len == 0 {
        return Ok(Vec::new());
    }

    let mut out = Vec::with_capacity(len);
    for idx in 0..len {
        let kwargs = unsafe { &*ptr.add(idx) };
        let markup = kwargs_to_string(kwargs);
        let driver = kwargs_get(kwargs, "driver").unwrap_or_else(|| "soapy".to_owned());
        let label = if markup.is_empty() {
            format!("SoapySDR device #{idx}")
        } else {
            markup.clone()
        };
        out.push(SdrDeviceInfo {
            id: markup,
            label,
            driver,
        });
    }

    unsafe { clear(ptr, len) };
    Ok(out)
}

fn kwargs_to_string(kwargs: &SoapySdrKwargs) -> String {
    let mut parts = Vec::new();
    for idx in 0..kwargs.size {
        let key = unsafe { cstr_ptr_to_string(*kwargs.keys.add(idx)) };
        let val = unsafe { cstr_ptr_to_string(*kwargs.vals.add(idx)) };
        if let Some(key) = key {
            if let Some(val) = val {
                parts.push(format!("{key}={val}"));
            } else {
                parts.push(key);
            }
        }
    }
    parts.join(", ")
}

fn kwargs_get(kwargs: &SoapySdrKwargs, wanted: &str) -> Option<String> {
    for idx in 0..kwargs.size {
        let key = unsafe { cstr_ptr_to_string(*kwargs.keys.add(idx)) }?;
        if key == wanted {
            return unsafe { cstr_ptr_to_string(*kwargs.vals.add(idx)) };
        }
    }
    None
}

unsafe fn cstr_ptr_to_string(ptr: *const c_char) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    Some(
        unsafe { CStr::from_ptr(ptr) }
            .to_string_lossy()
            .into_owned(),
    )
}

fn last_error(lib: &Library) -> String {
    let Ok(last_error) = (unsafe { lib.get::<LastError>(b"SoapySDRDevice_lastError\0") }) else {
        return "lastError unavailable".to_owned();
    };
    let ptr = unsafe { last_error() };
    if ptr.is_null() {
        "unknown SoapySDR error".to_owned()
    } else {
        unsafe { CStr::from_ptr(ptr) }
            .to_string_lossy()
            .into_owned()
    }
}

#[derive(Debug, Clone)]
struct LibraryCandidate {
    path: PathBuf,
    root: Option<PathBuf>,
}

fn load_library() -> Option<(&'static Library, PathBuf)> {
    static SOAPY_LIBRARY: OnceLock<(Library, PathBuf)> = OnceLock::new();

    if SOAPY_LIBRARY.get().is_none()
        && let Some(loaded) = load_library_uncached()
    {
        let _ = SOAPY_LIBRARY.set(loaded);
    }

    SOAPY_LIBRARY.get().map(|(lib, path)| (lib, path.clone()))
}

fn load_library_uncached() -> Option<(Library, PathBuf)> {
    let mut failures = Vec::new();

    for candidate in library_candidates() {
        prepare_library_load(&candidate);
        let loaded = unsafe { Library::new(&candidate.path) };
        match loaded {
            Ok(lib) => {
                configure_runtime_environment(&candidate);
                return Some((lib, candidate.path));
            }
            Err(err) => {
                failures.push(format!("{}: {err}", candidate.path.display()));
            }
        }
    }

    log_load_failures(&failures);
    None
}

fn library_candidates() -> Vec<LibraryCandidate> {
    let mut candidates = Vec::new();

    for root in runtime_roots() {
        for path in library_files_in_root(&root) {
            candidates.push(LibraryCandidate {
                path,
                root: Some(root.clone()),
            });
        }
    }

    for name in platform_library_names() {
        candidates.push(LibraryCandidate {
            path: PathBuf::from(name),
            root: None,
        });
    }

    dedup_candidates(candidates)
}

fn runtime_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();

    push_env_roots(&mut roots, &["SDRWISPY_SOAPY_ROOT"]);
    push_env_roots(
        &mut roots,
        &["SOAPY_SDR_ROOT", "SOAPYSDR_ROOT", "POTHOS_ROOT"],
    );
    push_env_roots(&mut roots, &["CONDA_PREFIX"]);
    roots.extend(path_roots());

    #[cfg(target_os = "windows")]
    roots.extend(windows_default_roots());

    dedup_paths(roots)
        .into_iter()
        .filter(|root| root.exists())
        .collect()
}

fn push_env_roots(roots: &mut Vec<PathBuf>, keys: &[&str]) {
    for key in keys {
        if let Some(value) = std::env::var_os(key) {
            roots.push(PathBuf::from(value));
        }
    }
}

fn path_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let Some(path) = std::env::var_os("PATH") else {
        return roots;
    };

    for entry in std::env::split_paths(&path) {
        if path_entry_contains_soapy(&entry) {
            if let Some(root) = root_from_bin_dir(&entry) {
                roots.push(root);
            } else {
                roots.push(entry);
            }
        }
    }

    dedup_paths(roots)
}

fn path_entry_contains_soapy(entry: &Path) -> bool {
    entry.join(platform_util_name()).exists()
        || platform_library_names()
            .iter()
            .any(|name| entry.join(name).exists())
}

fn root_from_bin_dir(dir: &Path) -> Option<PathBuf> {
    let name = dir.file_name()?.to_str()?;
    if name.eq_ignore_ascii_case("bin") {
        dir.parent().map(Path::to_path_buf)
    } else {
        None
    }
}

fn library_files_in_root(root: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();

    for dir in library_search_dirs(root) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            if path.is_file() && is_soapy_library_name(name) {
                paths.push(path);
            }
        }
    }

    paths.sort_by(|a, b| {
        library_preference_score(a)
            .cmp(&library_preference_score(b))
            .then_with(|| a.cmp(b))
    });
    dedup_paths(paths)
}

fn library_search_dirs(root: &Path) -> Vec<PathBuf> {
    dedup_paths(vec![root.join("lib"), root.join("bin"), root.to_path_buf()])
}

fn library_preference_score(path: &Path) -> usize {
    let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
        return usize::MAX;
    };
    platform_library_names()
        .iter()
        .position(|candidate| library_name_eq(name, candidate))
        .unwrap_or(platform_library_names().len())
}

fn is_soapy_library_name(name: &str) -> bool {
    #[cfg(target_os = "windows")]
    {
        name.eq_ignore_ascii_case("SoapySDR.dll") || name.eq_ignore_ascii_case("libSoapySDR.dll")
    }
    #[cfg(target_os = "linux")]
    {
        name == "libSoapySDR.so" || name.starts_with("libSoapySDR.so.")
    }
    #[cfg(target_os = "macos")]
    {
        name == "libSoapySDR.dylib"
            || (name.starts_with("libSoapySDR.") && name.ends_with(".dylib"))
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    {
        false
    }
}

fn platform_library_names() -> &'static [&'static str] {
    #[cfg(target_os = "windows")]
    {
        &["SoapySDR.dll", "libSoapySDR.dll"]
    }
    #[cfg(target_os = "linux")]
    {
        &["libSoapySDR.so", "libSoapySDR.so.0.8", "libSoapySDR.so.0.7"]
    }
    #[cfg(target_os = "macos")]
    {
        &[
            "libSoapySDR.dylib",
            "libSoapySDR.0.8.dylib",
            "libSoapySDR.0.7.dylib",
        ]
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    {
        &[]
    }
}

fn platform_util_name() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        "SoapySDRUtil.exe"
    }
    #[cfg(not(target_os = "windows"))]
    {
        "SoapySDRUtil"
    }
}

fn library_name_eq(a: &str, b: &str) -> bool {
    #[cfg(target_os = "windows")]
    {
        a.eq_ignore_ascii_case(b)
    }
    #[cfg(not(target_os = "windows"))]
    {
        a == b
    }
}

fn module_dirs(root: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let base = root.join("lib").join("SoapySDR");
    let Ok(entries) = std::fs::read_dir(base) else {
        return dirs;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if path.is_dir() && name.starts_with("modules") {
            dirs.push(path);
        }
    }

    dirs.sort();
    dedup_paths(dirs)
}

fn configure_runtime_environment(candidate: &LibraryCandidate) {
    if let Some(root) = candidate_runtime_root(candidate) {
        configure_runtime_environment_for_root(&root, &candidate.path);
    }
}

fn configure_runtime_environment_for_root(root: &Path, library_path: &Path) {
    prepend_env_paths("SOAPY_SDR_PLUGIN_PATH", &module_dirs(root));
    #[cfg(not(target_os = "windows"))]
    let _ = library_path;

    #[cfg(target_os = "windows")]
    {
        let dirs = dependency_dirs(root, library_path);
        prepend_env_paths("PATH", &dirs);
        configure_windows_dll_search(&dirs);
    }
}

fn runtime_root_from_library_path(library_path: &Path) -> Option<PathBuf> {
    let dir = library_path.parent()?;
    let name = dir.file_name()?.to_str()?;
    if name.eq_ignore_ascii_case("bin") || name.eq_ignore_ascii_case("lib") {
        dir.parent().map(Path::to_path_buf)
    } else {
        None
    }
}

fn candidate_runtime_root(candidate: &LibraryCandidate) -> Option<PathBuf> {
    runtime_root_from_library_path(&candidate.path).or_else(|| candidate.root.clone())
}

#[cfg(not(target_os = "windows"))]
fn prepare_library_load(_candidate: &LibraryCandidate) {}

#[cfg(target_os = "windows")]
fn prepare_library_load(candidate: &LibraryCandidate) {
    if let Some(root) = candidate_runtime_root(candidate) {
        let dirs = dependency_dirs(&root, &candidate.path);
        prepend_env_paths("PATH", &dirs);
        configure_windows_dll_search(&dirs);
    }
}

#[cfg(target_os = "windows")]
fn dependency_dirs(root: &Path, library_path: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(parent) = library_path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
    {
        dirs.push(parent.to_path_buf());
    }
    dirs.push(root.join("bin"));
    dirs.push(root.join("lib"));
    dirs.extend(module_dirs(root));
    dedup_paths(dirs)
        .into_iter()
        .filter(|path| path.exists())
        .collect()
}

#[cfg(target_os = "windows")]
fn windows_default_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();

    for key in ["ProgramFiles", "ProgramFiles(x86)"] {
        if let Some(value) = std::env::var_os(key) {
            roots.push(PathBuf::from(value).join("PothosSDR"));
        }
    }

    roots.push(PathBuf::from("C:\\Program Files\\PothosSDR"));
    roots.push(PathBuf::from("C:\\Program Files (x86)\\PothosSDR"));
    dedup_paths(roots)
}

fn dedup_candidates(candidates: Vec<LibraryCandidate>) -> Vec<LibraryCandidate> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for candidate in candidates {
        if seen.insert(path_key(&candidate.path)) {
            out.push(candidate);
        }
    }
    out
}

fn dedup_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for path in paths {
        if seen.insert(path_key(&path)) {
            out.push(path);
        }
    }
    out
}

fn prepend_env_paths(key: &str, dirs: &[PathBuf]) {
    let existing = std::env::var_os(key).unwrap_or_default();
    let existing_paths: Vec<PathBuf> = std::env::split_paths(&existing).collect();
    let mut seen: HashSet<String> = existing_paths.iter().map(|path| path_key(path)).collect();

    let mut new_paths = Vec::new();
    for dir in dirs {
        if dir.exists() && seen.insert(path_key(dir)) {
            new_paths.push(dir.clone());
        }
    }
    if new_paths.is_empty() {
        return;
    }

    new_paths.extend(existing_paths);
    if let Ok(joined) = std::env::join_paths(new_paths) {
        unsafe { std::env::set_var(key, joined) };
    }
}

fn path_key(path: &Path) -> String {
    let key = path.to_string_lossy().into_owned();
    #[cfg(target_os = "windows")]
    {
        key.to_ascii_lowercase()
    }
    #[cfg(not(target_os = "windows"))]
    {
        key
    }
}

fn log_load_failures(failures: &[String]) {
    eprintln!(
        "[sdrwispy] SoapySDR runtime not found; tried {} candidate(s)",
        failures.len()
    );
    for failure in failures.iter().take(12) {
        eprintln!("[sdrwispy]   {failure}");
    }
    if failures.len() > 12 {
        eprintln!("[sdrwispy]   ... {} more candidate(s)", failures.len() - 12);
    }
}

#[cfg(target_os = "windows")]
fn configure_windows_dll_search(dirs: &[PathBuf]) {
    type DllDirectoryCookie = *mut c_void;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn SetDllDirectoryW(lpPathName: *const u16) -> i32;
        fn AddDllDirectory(NewDirectory: *const u16) -> DllDirectoryCookie;
        fn SetDefaultDllDirectories(DirectoryFlags: u32) -> i32;
    }

    const LOAD_LIBRARY_SEARCH_DEFAULT_DIRS: u32 = 0x0000_1000;
    const LOAD_LIBRARY_SEARCH_USER_DIRS: u32 = 0x0000_0400;

    // Prefer a matched PothosSDR bin directory for module dependencies
    // such as uhd.dll, boost_*.dll, libusb-1.0.dll, and MSVC runtime DLLs.
    if let Some(bin) = dirs.iter().find(|p| {
        p.file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|s| s.eq_ignore_ascii_case("bin"))
            && p.join("uhd.dll").exists()
    }) {
        let wide = wide_path(bin);
        unsafe {
            let _ = SetDllDirectoryW(wide.as_ptr());
        }
    }

    unsafe {
        let _ = SetDefaultDllDirectories(
            LOAD_LIBRARY_SEARCH_DEFAULT_DIRS | LOAD_LIBRARY_SEARCH_USER_DIRS,
        );
    }

    for dir in dirs {
        if dir.exists() {
            let wide = wide_path(dir);
            unsafe {
                // Intentionally keep the directory cookie for process lifetime.
                let _ = AddDllDirectory(wide.as_ptr());
            }
        }
    }
}

#[cfg(target_os = "windows")]
fn wide_path(path: &std::path::Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;

    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

#[cfg(test)]
mod readback_tests {
    use super::*;

    #[test]
    fn rounded_sample_rate_stops_capture_instead_of_using_wrong_coordinates() {
        let error = checked_sample_rate(20.0e6, Ok(40.0e6)).unwrap_err();
        assert!(error.contains("requested 20000000 Hz, actual 40000000 Hz"));
        assert!(error.contains("capture stopped"));
        assert_eq!(checked_sample_rate(320.0e6, Ok(320.0e6)).unwrap(), 320.0e6);
    }

    #[test]
    fn small_rate_rounding_returns_actual_not_requested() {
        assert_eq!(
            checked_sample_rate(40.0e6, Ok(40.0e6 + 10.0)).unwrap(),
            40.0e6 + 10.0
        );
    }

    #[test]
    fn clamped_or_unchanged_center_stops_capture() {
        let error = checked_readback("center frequency", 6.0e9, Ok(2.5e9), 100.0).unwrap_err();
        assert!(error.contains("requested 6000000000 Hz, actual 2500000000 Hz"));
        assert!(checked_readback("center frequency", 2.45e9, Ok(2.44e9), 100.0).is_err());
        assert_eq!(
            checked_readback("center frequency", 2.45e9, Ok(2.45e9 + 10.0), 100.0).unwrap(),
            2.45e9 + 10.0
        );
    }

    #[test]
    fn getter_status_errors_are_not_mistaken_for_values() {
        let readback = decode_readback("getSampleRate", 40.0e6, -1, || "control failed".into());
        let error = checked_sample_rate(40.0e6, readback).unwrap_err();
        assert!(error.contains("getSampleRate failed"));
        assert!(error.contains("control failed"));
        assert!(
            checked_readback(
                "center frequency",
                2.4e9,
                Err("missing getter".into()),
                100.0
            )
            .is_err()
        );
        assert_eq!(
            decode_readback("getGain", -5.0, 0, || panic!(
                "stale lastError must not be read"
            ))
            .unwrap(),
            -5.0
        );
    }

    #[test]
    fn invalid_rates_and_centers_are_rejected() {
        for invalid in [0.0, -1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(checked_sample_rate(invalid, Ok(40.0e6)).is_err());
            assert!(checked_sample_rate(40.0e6, Ok(invalid)).is_err());
            assert!(checked_readback("center frequency", 2.4e9, Ok(invalid), 100.0).is_err());
        }
    }

    #[test]
    fn unsupported_bandwidth_and_gain_are_explicitly_unknown() {
        assert!(optional_readback(Err("not supported".into()), true).contains("unknown"));
        assert!(optional_readback(Ok(0.0), true).contains("unknown"));
        assert!(optional_readback(Ok(f64::NAN), true).contains("unknown"));
        assert_eq!(optional_readback(Ok(40.0e6), true), "40000000");
        assert_eq!(optional_readback(Ok(-12.5), false), "-12.5");
        assert_eq!(optional_readback(Ok(0.0), false), "0");
    }
}
