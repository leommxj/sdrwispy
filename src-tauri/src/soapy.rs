use std::collections::HashSet;
use std::ffi::{CStr, CString, c_char, c_int, c_long, c_longlong, c_void};
use std::path::PathBuf;
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
}

impl SoapyRx {
    pub fn open(config: SoapyRxConfig) -> Result<Self, String> {
        configure_windows_runtime_environment(None);
        let (lib, loaded_from) = load_library()
            .ok_or_else(|| "SoapySDR runtime not found. Set POTHOS_ROOT or PATH.".to_owned())?;
        configure_windows_runtime_environment(Some(&loaded_from));

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
        self.config.sample_rate_hz
    }

    pub fn tune(&mut self, center_freq_hz: f64) -> Result<(), String> {
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
        self.check_ret(ret, "setFrequency")
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

        Ok(())
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
    let Some((lib, loaded_from)) = load_library() else {
        return vec![SdrDeviceInfo {
            id: "soapy-unavailable".to_owned(),
            label: "SoapySDR runtime not found".to_owned(),
            driver: "soapy-info".to_owned(),
        }];
    };
    configure_windows_runtime_environment(Some(&loaded_from));

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

fn load_library() -> Option<(&'static Library, PathBuf)> {
    static SOAPY_LIBRARY: OnceLock<Option<(Library, PathBuf)>> = OnceLock::new();

    SOAPY_LIBRARY
        .get_or_init(load_library_uncached)
        .as_ref()
        .map(|(lib, path)| (lib, path.clone()))
}

fn load_library_uncached() -> Option<(Library, PathBuf)> {
    configure_windows_runtime_environment(None);

    for candidate in library_candidates() {
        configure_windows_runtime_environment(Some(&candidate));
        let loaded = unsafe { Library::new(&candidate) };
        if let Ok(lib) = loaded {
            return Some((lib, candidate));
        }
    }
    None
}

fn library_candidates() -> Vec<PathBuf> {
    let mut names = Vec::new();

    #[cfg(target_os = "windows")]
    {
        for root in pothos_roots() {
            names.push(root.join("bin").join("SoapySDR.dll"));
            names.push(root.join("bin").join("libSoapySDR.dll"));
        }
        names.push(PathBuf::from("SoapySDR.dll"));
        names.push(PathBuf::from("libSoapySDR.dll"));
    }

    #[cfg(target_os = "linux")]
    {
        names.push(PathBuf::from("libSoapySDR.so"));
        names.push(PathBuf::from("libSoapySDR.so.0.8"));
        names.push(PathBuf::from("libSoapySDR.so.0.7"));
    }

    dedup_paths(names)
}

fn dedup_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for p in paths {
        let key = p.to_string_lossy().to_ascii_lowercase();
        if seen.insert(key) {
            out.push(p);
        }
    }
    out
}

#[cfg(not(target_os = "windows"))]
fn configure_windows_runtime_environment(_candidate: Option<&std::path::Path>) {}

#[cfg(target_os = "windows")]
fn configure_windows_runtime_environment(candidate: Option<&std::path::Path>) {
    let mut dirs = Vec::new();

    for root in pothos_roots() {
        add_pothos_dirs(&mut dirs, &root);
    }

    if let Some(candidate) = candidate {
        if let Some(bin) = candidate.parent() {
            dirs.push(bin.to_path_buf());
            if bin
                .file_name()
                .and_then(|s| s.to_str())
                .is_some_and(|s| s.eq_ignore_ascii_case("bin"))
            {
                if let Some(root) = bin.parent() {
                    add_pothos_dirs(&mut dirs, root);
                }
            }
        }
    }

    prepend_path_dirs(&dedup_paths(dirs.clone()));
    configure_windows_dll_search(&dedup_paths(dirs));
}

#[cfg(target_os = "windows")]
fn pothos_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();

    for key in ["POTHOS_ROOT", "SOAPYSDR_ROOT", "SOAPY_SDR_ROOT"] {
        if let Some(value) = std::env::var_os(key) {
            roots.push(PathBuf::from(value));
        }
    }

    if let Some(path) = std::env::var_os("PATH") {
        for entry in std::env::split_paths(&path) {
            let contains_soapy =
                entry.join("SoapySDR.dll").exists() || entry.join("libSoapySDR.dll").exists();
            let is_bin = entry
                .file_name()
                .and_then(|s| s.to_str())
                .is_some_and(|s| s.eq_ignore_ascii_case("bin"));
            if contains_soapy
                && is_bin
                && let Some(root) = entry.parent()
            {
                roots.push(root.to_path_buf());
            }
        }
    }

    for key in ["ProgramFiles", "ProgramFiles(x86)"] {
        if let Some(value) = std::env::var_os(key) {
            roots.push(PathBuf::from(value).join("PothosSDR"));
        }
    }

    roots.push(PathBuf::from("C:\\Program Files\\PothosSDR"));
    roots.push(PathBuf::from("C:\\Program Files (x86)\\PothosSDR"));

    dedup_paths(roots)
        .into_iter()
        .filter(|p| p.join("bin").exists())
        .collect()
}

#[cfg(target_os = "windows")]
fn add_pothos_dirs(dirs: &mut Vec<PathBuf>, root: &std::path::Path) {
    dirs.push(root.join("bin"));
    dirs.push(root.join("lib"));
    let modules = root.join("lib").join("SoapySDR");
    dirs.push(modules.join("modules0.8"));
    dirs.push(modules.join("modules0.7"));
}

#[cfg(target_os = "windows")]
fn prepend_path_dirs(dirs: &[PathBuf]) {
    let existing = std::env::var_os("PATH").unwrap_or_default();
    let existing_paths: Vec<PathBuf> = std::env::split_paths(&existing).collect();
    let existing_keys: HashSet<String> = existing_paths
        .iter()
        .map(|p| p.to_string_lossy().to_ascii_lowercase())
        .collect();

    let mut new_paths = Vec::new();
    for dir in dirs {
        if dir.exists() && !existing_keys.contains(&dir.to_string_lossy().to_ascii_lowercase()) {
            new_paths.push(dir.clone());
        }
    }
    new_paths.extend(existing_paths);

    if let Ok(joined) = std::env::join_paths(new_paths) {
        unsafe { std::env::set_var("PATH", joined) };
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
