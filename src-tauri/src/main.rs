use std::collections::{BTreeSet, VecDeque};
use std::fmt::Write as _;
use std::sync::Mutex;
#[cfg(feature = "soapy")]
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

mod model;
mod profile;
#[cfg(feature = "soapy")]
mod real_sweep;
mod sdr;
#[cfg(feature = "soapy")]
mod soapy;
mod wifi;

use model::{BandConfig, BandPreset, SdrDeviceInfo, SpectrumFrame, SweepConfig, WifiBss, now_ns};
use profile::{RuntimeProfile, load_profile_toml};
use sdr::SdrDeviceCatalog;
use tauri::State;
use wifi::{SystemWifiScanner, WifiBackend, WifiBackendInfo};

#[cfg(feature = "soapy")]
use real_sweep::{RealSweepEngine, RealSweepReport};

const WIFI_AUTO_SCAN_DEFAULT_SECS: u64 = 30;
const WIFI_AUTO_SCAN_MIN_SECS: u64 = 5;
const WIFI_AUTO_SCAN_MAX_SECS: u64 = 600;
const WIFI_AP_CACHE_TTL_SECS: i64 = 180;

#[derive(Debug, Clone, Serialize)]
struct BandDefaultsDto {
    ghz2: [f64; 3],
    ghz5: [f64; 3],
    ghz6: [f64; 3],
}

#[derive(Debug, Clone)]
struct BandDefaults {
    ghz2: (f64, f64, f64),
    ghz5: (f64, f64, f64),
    ghz6: (f64, f64, f64),
}

impl Default for BandDefaults {
    fn default() -> Self {
        Self {
            ghz2: BandPreset::Ghz2.defaults_mhz(),
            ghz5: BandPreset::Ghz5.defaults_mhz(),
            ghz6: BandPreset::Ghz6.defaults_mhz(),
        }
    }
}

impl BandDefaults {
    fn get(&self, preset: BandPreset) -> (f64, f64, f64) {
        match preset {
            BandPreset::Ghz2 => self.ghz2,
            BandPreset::Ghz5 => self.ghz5,
            BandPreset::Ghz6 => self.ghz6,
            BandPreset::Custom => BandPreset::Custom.defaults_mhz(),
        }
    }

    fn set(&mut self, preset: BandPreset, value: (f64, f64, f64)) {
        match preset {
            BandPreset::Ghz2 => self.ghz2 = value,
            BandPreset::Ghz5 => self.ghz5 = value,
            BandPreset::Ghz6 => self.ghz6 = value,
            BandPreset::Custom => {}
        }
    }

    fn dto(&self) -> BandDefaultsDto {
        BandDefaultsDto {
            ghz2: [self.ghz2.0, self.ghz2.1, self.ghz2.2],
            ghz5: [self.ghz5.0, self.ghz5.1, self.ghz5.2],
            ghz6: [self.ghz6.0, self.ghz6.1, self.ghz6.2],
        }
    }
}

#[cfg(feature = "soapy")]
enum WorkerMessage {
    Report { epoch: u64, report: RealSweepReport },
    Error(String),
    Status(String),
}

#[cfg(feature = "soapy")]
struct CaptureWorker {
    epoch: u64,
    run: Arc<AtomicBool>,
    cancel: Arc<AtomicBool>,
    quit: Arc<AtomicBool>,
    rx: mpsc::Receiver<WorkerMessage>,
    progress: Arc<Mutex<Option<SpectrumFrame>>>,
    join: Option<std::thread::JoinHandle<()>>,
}

struct RuntimeState {
    band_defaults: BandDefaults,
    band: BandConfig,
    sweep: SweepConfig,
    display: DisplaySettings,
    sdr_gain_db: f64,
    soapy_args: String,
    devices: Vec<SdrDeviceInfo>,
    selected_device_index: usize,
    aps: Vec<WifiBss>,
    wifi_interfaces: Vec<String>,
    selected_wifi_interface: String,
    selected_wifi_backend: WifiBackend,
    wifi_auto_scan_enabled: bool,
    wifi_auto_scan_interval_secs: u64,
    wifi_scan_in_progress: bool,
    wifi_status: String,
    #[cfg(target_os = "linux")]
    wifi_helper: Option<wifi::PrivilegedWifiHelper>,
    history: VecDeque<SpectrumFrame>,
    #[cfg(feature = "soapy")]
    history_capacity: usize,
    running: bool,
    capture_epoch: u64,
    status: String,
    profile_name: String,
    profile_path: String,
    hidden_wifi_overlay_bssids: BTreeSet<String>,
    last_wifi_scan: Instant,
    #[cfg(feature = "soapy")]
    worker: Option<CaptureWorker>,
}

struct BackendState {
    inner: Mutex<RuntimeState>,
}

#[derive(Debug, Clone, Serialize)]
struct AppSnapshot {
    band: BandConfig,
    band_defaults: BandDefaultsDto,
    sweep: SweepConfig,
    display: DisplaySettings,
    sdr_gain_db: f64,
    soapy_args: String,
    devices: Vec<SdrDeviceInfo>,
    selected_device_index: usize,
    aps: Vec<WifiBss>,
    wifi_interfaces: Vec<String>,
    selected_wifi_interface: String,
    wifi_backends: Vec<WifiBackendInfo>,
    selected_wifi_backend: WifiBackend,
    wifi_auto_scan_enabled: bool,
    wifi_auto_scan_interval_secs: u64,
    wifi_scan_in_progress: bool,
    wifi_helper_running: bool,
    wifi_status: String,
    running: bool,
    capture_epoch: u64,
    status: String,
    profile_name: String,
    profile_path: String,
    latest_frame_id: Option<u64>,
    history_len: usize,
    hidden_wifi_overlay_bssids: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct SweepPatch {
    sample_rate_hz: Option<f64>,
    usable_bandwidth_ratio: Option<f64>,
    overlap_hz: Option<f64>,
    dwell_ms: Option<f64>,
    settle_ms: Option<f64>,
    fft_size: Option<usize>,
}

#[derive(Debug, Clone, Deserialize)]
struct BandPatch {
    start_mhz: Option<f64>,
    stop_mhz: Option<f64>,
    bin_width_khz: Option<f64>,
}

#[derive(Debug, Clone, Deserialize)]
struct ReceiverPatch {
    gain_db: Option<f64>,
    soapy_args: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DisplaySettings {
    show_current: bool,
    show_average: bool,
    show_max_hold: bool,
    show_wifi_overlay: bool,
    show_wifi_labels: bool,
    label_aps_by_bssid: bool,
    wifi_overlay_rssi_floor_dbm: f64,
    wifi_overlay_rssi_ceiling_dbm: f64,
    wifi_overlay_floor_db: f64,
    wifi_overlay_ceiling_db: f64,
}

impl Default for DisplaySettings {
    fn default() -> Self {
        Self {
            show_current: true,
            show_average: true,
            show_max_hold: true,
            show_wifi_overlay: true,
            show_wifi_labels: true,
            label_aps_by_bssid: false,
            wifi_overlay_rssi_floor_dbm: -95.0,
            wifi_overlay_rssi_ceiling_dbm: -30.0,
            wifi_overlay_floor_db: -113.0,
            wifi_overlay_ceiling_db: -30.0,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
struct DisplayPatch {
    show_current: Option<bool>,
    show_average: Option<bool>,
    show_max_hold: Option<bool>,
    show_wifi_overlay: Option<bool>,
    show_wifi_labels: Option<bool>,
    label_aps_by_bssid: Option<bool>,
    wifi_overlay_rssi_floor_dbm: Option<f64>,
    wifi_overlay_rssi_ceiling_dbm: Option<f64>,
    wifi_overlay_floor_db: Option<f64>,
    wifi_overlay_ceiling_db: Option<f64>,
}

fn initial_runtime_state() -> RuntimeState {
    eprintln!(
        "[sdrwispy] backend initializing on {} {}",
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    #[cfg(feature = "soapy")]
    eprintln!("[sdrwispy] feature 'soapy' is enabled");
    #[cfg(not(feature = "soapy"))]
    eprintln!("[sdrwispy] feature 'soapy' is disabled; SDR capture/enumeration is unavailable");

    let band_defaults = BandDefaults::default();
    let mut band = BandConfig::from_preset(BandPreset::Ghz2);
    let (start, stop, bin_khz) = band_defaults.get(BandPreset::Ghz2);
    band.start_hz = start * 1.0e6;
    band.stop_hz = stop * 1.0e6;
    band.bin_width_hz = bin_khz * 1.0e3;

    let selected_wifi_backend = WifiBackend::default_for_platform();
    let mut wifi = SystemWifiScanner::new(selected_wifi_backend);
    let selected_wifi_interface = String::new();
    #[cfg(target_os = "linux")]
    let wifi_report = wifi.scan(&band, None, None);
    #[cfg(not(target_os = "linux"))]
    let wifi_report = wifi.scan(&band, None);
    eprintln!("[sdrwispy] {}", wifi_report.status);
    let sdr = SdrDeviceCatalog;
    eprintln!("[sdrwispy] enumerating SDR devices");
    let devices = sdr.list_devices();
    eprintln!(
        "[sdrwispy] SDR catalog returned {} device entrie(s)",
        devices.len()
    );
    for (idx, dev) in devices.iter().enumerate() {
        eprintln!(
            "[sdrwispy]   device #{idx}: driver={} label={} id={}",
            dev.driver, dev.label, dev.id
        );
    }
    let selected_device_index = devices
        .iter()
        .position(|d| d.driver != "soapy-info")
        .unwrap_or(0);

    let status = if has_real_sdr_device(&devices) {
        "Ready with built-in defaults. Load a TOML profile or press Run.".to_owned()
    } else {
        sdr_catalog_status(&devices)
    };

    RuntimeState {
        band_defaults,
        band,
        sweep: SweepConfig::default(),
        display: DisplaySettings::default(),
        sdr_gain_db: 40.0,
        soapy_args: String::new(),
        devices,
        selected_device_index,
        aps: wifi_report.aps,
        wifi_interfaces: wifi_report.interfaces,
        selected_wifi_interface,
        selected_wifi_backend,
        wifi_auto_scan_enabled: true,
        wifi_auto_scan_interval_secs: WIFI_AUTO_SCAN_DEFAULT_SECS,
        wifi_scan_in_progress: false,
        wifi_status: wifi_report.status,
        #[cfg(target_os = "linux")]
        wifi_helper: None,
        history: VecDeque::with_capacity(900),
        #[cfg(feature = "soapy")]
        history_capacity: 900,
        running: false,
        capture_epoch: 0,
        status,
        profile_name: "Built-in defaults".to_owned(),
        profile_path: String::new(),
        hidden_wifi_overlay_bssids: BTreeSet::new(),
        last_wifi_scan: Instant::now(),
        #[cfg(feature = "soapy")]
        worker: None,
    }
}

fn snapshot(state: &RuntimeState) -> AppSnapshot {
    AppSnapshot {
        band: state.band.clone(),
        band_defaults: state.band_defaults.dto(),
        sweep: state.sweep.clone(),
        display: state.display.clone(),
        sdr_gain_db: state.sdr_gain_db,
        soapy_args: state.soapy_args.clone(),
        devices: state.devices.clone(),
        selected_device_index: state.selected_device_index,
        aps: state.aps.clone(),
        wifi_interfaces: state.wifi_interfaces.clone(),
        selected_wifi_interface: state.selected_wifi_interface.clone(),
        wifi_backends: wifi::available_wifi_backends(),
        selected_wifi_backend: state.selected_wifi_backend,
        wifi_auto_scan_enabled: state.wifi_auto_scan_enabled,
        wifi_auto_scan_interval_secs: state.wifi_auto_scan_interval_secs,
        wifi_scan_in_progress: state.wifi_scan_in_progress,
        wifi_helper_running: wifi_helper_running(state),
        wifi_status: state.wifi_status.clone(),
        running: state.running,
        capture_epoch: state.capture_epoch,
        status: state.status.clone(),
        profile_name: state.profile_name.clone(),
        profile_path: state.profile_path.clone(),
        latest_frame_id: state.history.back().map(|f| f.frame_id),
        history_len: state.history.len(),
        hidden_wifi_overlay_bssids: state.hidden_wifi_overlay_bssids.iter().cloned().collect(),
    }
}

#[tauri::command]
fn get_snapshot(app: State<'_, BackendState>) -> Result<AppSnapshot, String> {
    let mut state = app.inner.lock().map_err(|_| "state lock poisoned")?;
    drain_worker_messages(&mut state);
    Ok(snapshot(&state))
}

#[tauri::command]
fn get_latest_frame(
    app: State<'_, BackendState>,
    after_capture_epoch: Option<u64>,
    after_frame_id: Option<u64>,
    after_frame_revision: Option<u64>,
) -> Result<Option<SpectrumFrame>, String> {
    let mut state = app.inner.lock().map_err(|_| "state lock poisoned")?;
    drain_worker_messages(&mut state);
    let latest = latest_display_frame(&state);
    if let Some(frame) = latest {
        if after_capture_epoch == Some(frame.capture_epoch)
            && after_frame_id == Some(frame.frame_id)
            && after_frame_revision == Some(frame.frame_revision)
        {
            Ok(None)
        } else {
            Ok(Some(frame))
        }
    } else {
        Ok(None)
    }
}

#[tauri::command]
fn list_sdr_devices(app: State<'_, BackendState>) -> Result<Vec<SdrDeviceInfo>, String> {
    let mut state = app.inner.lock().map_err(|_| "state lock poisoned")?;
    let sdr = SdrDeviceCatalog;
    state.devices = sdr.list_devices();
    state.selected_device_index = state
        .devices
        .iter()
        .position(|d| d.driver != "soapy-info")
        .unwrap_or(0);
    state.status = sdr_catalog_status(&state.devices);
    Ok(state.devices.clone())
}

#[tauri::command]
fn select_sdr_device(app: State<'_, BackendState>, index: usize) -> Result<AppSnapshot, String> {
    let mut state = app.inner.lock().map_err(|_| "state lock poisoned")?;
    if index >= state.devices.len() {
        return Err("SDR device index out of range".to_owned());
    }
    stop_worker_locked(&mut state, true);
    state.selected_device_index = index;
    state.status = "SDR device changed. Press Run.".to_owned();
    Ok(snapshot(&state))
}

#[tauri::command]
fn set_band_preset(app: State<'_, BackendState>, preset: String) -> Result<AppSnapshot, String> {
    let mut state = app.inner.lock().map_err(|_| "state lock poisoned")?;
    let preset = parse_preset(&preset).ok_or_else(|| format!("unknown band preset {preset}"))?;
    let (start, stop, bin_khz) = state.band_defaults.get(preset);
    stop_worker_locked(&mut state, true);
    state.band = BandConfig {
        preset,
        start_hz: start * 1.0e6,
        stop_hz: stop * 1.0e6,
        bin_width_hz: bin_khz * 1.0e3,
    };
    clear_history(&mut state);
    scan_wifi_now_if_idle(&mut state);
    Ok(snapshot(&state))
}

#[tauri::command]
fn patch_band(app: State<'_, BackendState>, patch: BandPatch) -> Result<AppSnapshot, String> {
    let mut state = app.inner.lock().map_err(|_| "state lock poisoned")?;
    stop_worker_locked(&mut state, true);

    let start_hz = patch
        .start_mhz
        .filter(|v| v.is_finite())
        .map(|v| v * 1.0e6)
        .unwrap_or(state.band.start_hz);
    let stop_hz = patch
        .stop_mhz
        .filter(|v| v.is_finite())
        .map(|v| v * 1.0e6)
        .unwrap_or(state.band.stop_hz);
    let bin_width_hz = patch
        .bin_width_khz
        .filter(|v| v.is_finite())
        .map(|v| (v * 1.0e3).clamp(1.0e3, 20.0e6))
        .unwrap_or(state.band.bin_width_hz);

    if stop_hz <= start_hz {
        return Err("frequency stop must be greater than start".to_owned());
    }
    if (stop_hz - start_hz) / bin_width_hz > 16_384.0 {
        return Err("frequency range/bin width would exceed 16384 bins".to_owned());
    }

    state.band = BandConfig {
        preset: BandPreset::Custom,
        start_hz,
        stop_hz,
        bin_width_hz,
    };
    clear_history(&mut state);
    scan_wifi_now_if_idle(&mut state);
    state.status = format!(
        "Frequency range set to {:.3}-{:.3} MHz, bin {:.1} kHz. Press Run.",
        start_hz / 1.0e6,
        stop_hz / 1.0e6,
        bin_width_hz / 1.0e3
    );
    Ok(snapshot(&state))
}

#[tauri::command]
fn patch_sweep(app: State<'_, BackendState>, patch: SweepPatch) -> Result<AppSnapshot, String> {
    let mut state = app.inner.lock().map_err(|_| "state lock poisoned")?;
    stop_worker_locked(&mut state, true);
    if let Some(v) = patch.sample_rate_hz {
        state.sweep.sample_rate_hz = v;
    }
    if let Some(v) = patch.usable_bandwidth_ratio {
        state.sweep.usable_bandwidth_ratio = v;
    }
    if let Some(v) = patch.overlap_hz {
        state.sweep.overlap_hz = v;
    }
    if let Some(v) = patch.dwell_ms {
        state.sweep.dwell_ms = v;
    }
    if let Some(v) = patch.settle_ms {
        state.sweep.settle_ms = v;
    }
    if let Some(v) = patch.fft_size {
        state.sweep.fft_size = v;
    }
    clear_history(&mut state);
    state.status = "Sweep config changed. Press Run.".to_owned();
    Ok(snapshot(&state))
}

#[tauri::command]
fn patch_display(app: State<'_, BackendState>, patch: DisplayPatch) -> Result<AppSnapshot, String> {
    let mut state = app.inner.lock().map_err(|_| "state lock poisoned")?;
    if let Some(v) = patch.show_current {
        state.display.show_current = v;
    }
    if let Some(v) = patch.show_average {
        state.display.show_average = v;
    }
    if let Some(v) = patch.show_max_hold {
        state.display.show_max_hold = v;
    }
    if let Some(v) = patch.show_wifi_overlay {
        state.display.show_wifi_overlay = v;
    }
    if let Some(v) = patch.show_wifi_labels {
        state.display.show_wifi_labels = v;
    }
    if let Some(v) = patch.label_aps_by_bssid {
        state.display.label_aps_by_bssid = v;
    }
    if let Some(v) = patch.wifi_overlay_rssi_floor_dbm {
        state.display.wifi_overlay_rssi_floor_dbm = v;
    }
    if let Some(v) = patch.wifi_overlay_rssi_ceiling_dbm {
        state.display.wifi_overlay_rssi_ceiling_dbm = v;
    }
    if let Some(v) = patch.wifi_overlay_floor_db {
        state.display.wifi_overlay_floor_db = v;
    }
    if let Some(v) = patch.wifi_overlay_ceiling_db {
        state.display.wifi_overlay_ceiling_db = v;
    }
    state.status = "Display settings updated.".to_owned();
    Ok(snapshot(&state))
}

#[tauri::command]
fn patch_receiver(
    app: State<'_, BackendState>,
    patch: ReceiverPatch,
) -> Result<AppSnapshot, String> {
    let mut state = app.inner.lock().map_err(|_| "state lock poisoned")?;
    stop_worker_locked(&mut state, true);
    if let Some(gain) = patch.gain_db {
        state.sdr_gain_db = gain.clamp(0.0, 100.0);
    }
    if let Some(args) = patch.soapy_args {
        state.soapy_args = args.trim().to_owned();
    }
    clear_history(&mut state);
    state.status = "Receiver settings changed. Press Run.".to_owned();
    Ok(snapshot(&state))
}

#[tauri::command]
fn load_profile(app: State<'_, BackendState>, path: String) -> Result<AppSnapshot, String> {
    let mut state = app.inner.lock().map_err(|_| "state lock poisoned")?;
    stop_worker_locked(&mut state, true);
    let resolved = resolve_profile_path(&path);
    let profile = load_profile_toml(&resolved)?;
    let source_path = resolved.to_string_lossy().replace('\\', "/");
    apply_profile_locked(&mut state, profile, &source_path);
    state.profile_path = source_path;
    Ok(snapshot(&state))
}

#[tauri::command]
fn save_profile(app: State<'_, BackendState>, path: String) -> Result<AppSnapshot, String> {
    let mut state = app.inner.lock().map_err(|_| "state lock poisoned")?;
    let text = profile_toml_from_state(&state);
    std::fs::write(&path, text).map_err(|e| format!("write profile failed: {e}"))?;
    state.profile_path = path.replace('\\', "/");
    state.profile_name = profile_name_from_path(&state.profile_path);
    state.status = format!("Saved profile to {}.", state.profile_path);
    Ok(snapshot(&state))
}

#[tauri::command]
fn start_capture(app: State<'_, BackendState>) -> Result<AppSnapshot, String> {
    let mut state = app.inner.lock().map_err(|_| "state lock poisoned")?;
    match start_worker_locked(&mut state) {
        Ok(()) => Ok(snapshot(&state)),
        Err(err) => {
            state.status = format!("Capture start failed: {err}");
            Err(state.status.clone())
        }
    }
}

#[tauri::command]
fn stop_capture(app: State<'_, BackendState>) -> Result<AppSnapshot, String> {
    let mut state = app.inner.lock().map_err(|_| "state lock poisoned")?;
    stop_worker_locked(&mut state, true);
    state.status = "Capture stopped; SDR stream closed.".to_owned();
    Ok(snapshot(&state))
}

#[tauri::command]
fn scan_wifi(app: State<'_, BackendState>) -> Result<AppSnapshot, String> {
    scan_wifi_command(app, false)
}

#[tauri::command]
fn scan_wifi_auto(app: State<'_, BackendState>) -> Result<AppSnapshot, String> {
    scan_wifi_command(app, true)
}

#[tauri::command]
fn set_wifi_auto_scan(
    app: State<'_, BackendState>,
    enabled: bool,
    interval_secs: u64,
) -> Result<AppSnapshot, String> {
    let mut state = app.inner.lock().map_err(|_| "state lock poisoned")?;
    state.wifi_auto_scan_enabled = enabled;
    state.wifi_auto_scan_interval_secs = clamp_wifi_auto_scan_interval(interval_secs);
    state.wifi_status = if enabled {
        format!(
            "Wi-Fi auto scan enabled; refreshing every {} seconds.",
            state.wifi_auto_scan_interval_secs
        )
    } else {
        "Wi-Fi auto scan disabled. Press Scan to refresh APs.".to_owned()
    };
    Ok(snapshot(&state))
}

#[tauri::command]
fn select_wifi_interface(
    app: State<'_, BackendState>,
    interface: String,
) -> Result<AppSnapshot, String> {
    let mut state = app.inner.lock().map_err(|_| "state lock poisoned")?;
    state.selected_wifi_interface = interface.trim().to_owned();
    scan_wifi_now_if_idle(&mut state);
    Ok(snapshot(&state))
}

#[tauri::command]
fn select_wifi_backend(
    app: State<'_, BackendState>,
    backend: String,
) -> Result<AppSnapshot, String> {
    let mut state = app.inner.lock().map_err(|_| "state lock poisoned")?;
    let backend = WifiBackend::from_id(&backend)
        .ok_or_else(|| format!("unknown Wi-Fi backend `{backend}`"))?;
    let available = wifi::available_wifi_backends()
        .iter()
        .any(|info| info.id == backend.id());
    if !available {
        return Err(format!(
            "Wi-Fi backend `{}` is not available on {}",
            backend.id(),
            std::env::consts::OS
        ));
    }

    state.selected_wifi_backend = backend;
    #[cfg(target_os = "linux")]
    if backend != WifiBackend::PrivilegedHelper {
        state.wifi_helper = None;
    }
    state.wifi_status = if backend == WifiBackend::PrivilegedHelper && !wifi_helper_running(&state)
    {
        "Wi-Fi backend set to privileged helper. Start the helper before scanning.".to_owned()
    } else {
        format!("Wi-Fi backend set to {}.", backend.label())
    };
    Ok(snapshot(&state))
}

#[tauri::command]
fn start_wifi_helper(app: State<'_, BackendState>) -> Result<AppSnapshot, String> {
    let mut state = app.inner.lock().map_err(|_| "state lock poisoned")?;
    match start_wifi_helper_locked(&mut state) {
        Ok(()) => state.status = "Privileged Wi-Fi helper is running.".to_owned(),
        Err(err) => state.status = err,
    }
    Ok(snapshot(&state))
}

#[tauri::command]
fn set_wifi_overlay_hidden(
    app: State<'_, BackendState>,
    bssid: String,
    hidden: bool,
) -> Result<AppSnapshot, String> {
    let mut state = app.inner.lock().map_err(|_| "state lock poisoned")?;
    let key = bssid.trim().to_ascii_lowercase();
    if hidden {
        state.hidden_wifi_overlay_bssids.insert(key);
    } else {
        state.hidden_wifi_overlay_bssids.remove(&key);
    }
    Ok(snapshot(&state))
}

#[tauri::command]
fn set_wifi_overlay_all(
    app: State<'_, BackendState>,
    visible: bool,
) -> Result<AppSnapshot, String> {
    let mut state = app.inner.lock().map_err(|_| "state lock poisoned")?;
    if visible {
        state.hidden_wifi_overlay_bssids.clear();
    } else {
        let bssids: Vec<String> = state
            .aps
            .iter()
            .map(|ap| ap.bssid.trim().to_ascii_lowercase())
            .filter(|bssid| !bssid.is_empty())
            .collect();
        state.hidden_wifi_overlay_bssids.extend(bssids);
    }
    Ok(snapshot(&state))
}

fn apply_profile_locked(state: &mut RuntimeState, profile: RuntimeProfile, source_path: &str) {
    let mut custom_band = None;
    for (preset, defaults) in &profile.bands_mhz {
        if *preset == BandPreset::Custom {
            custom_band = Some(*defaults);
        }
        state.band_defaults.set(*preset, *defaults);
    }
    if let Some(sweep) = profile.sweep {
        state.sweep = sweep;
    }
    if let Some(device_id) = profile.selected_device_id.as_deref()
        && let Some(index) = state.devices.iter().position(|d| d.id == device_id)
    {
        state.selected_device_index = index;
    }
    if let Some(gain) = profile.sdr_gain_db {
        state.sdr_gain_db = gain;
    }
    if let Some(args) = profile.soapy_args {
        state.soapy_args = args;
    }
    if let Some(v) = profile.show_current {
        state.display.show_current = v;
    }
    if let Some(v) = profile.show_average {
        state.display.show_average = v;
    }
    if let Some(v) = profile.show_max_hold {
        state.display.show_max_hold = v;
    }
    if let Some(v) = profile.show_wifi_overlay {
        state.display.show_wifi_overlay = v;
    }
    if let Some(v) = profile.show_wifi_labels {
        state.display.show_wifi_labels = v;
    }
    if let Some(v) = profile.label_aps_by_bssid {
        state.display.label_aps_by_bssid = v;
    }
    if let Some(v) = profile.wifi_overlay_rssi_floor_dbm {
        state.display.wifi_overlay_rssi_floor_dbm = v;
    }
    if let Some(v) = profile.wifi_overlay_rssi_ceiling_dbm {
        state.display.wifi_overlay_rssi_ceiling_dbm = v;
    }
    if let Some(v) = profile.wifi_overlay_floor_db {
        state.display.wifi_overlay_floor_db = v;
    }
    if let Some(v) = profile.wifi_overlay_ceiling_db {
        state.display.wifi_overlay_ceiling_db = v;
    }
    if let Some(hidden) = profile.hidden_wifi_overlay_bssids {
        state.hidden_wifi_overlay_bssids = hidden
            .into_iter()
            .map(|bssid| bssid.trim().to_ascii_lowercase())
            .filter(|bssid| !bssid.is_empty())
            .collect();
    }
    let selected_band = profile.default_band.unwrap_or(state.band.preset);
    let (start, stop, bin_khz) = if selected_band == BandPreset::Custom {
        custom_band.unwrap_or_else(|| state.band_defaults.get(selected_band))
    } else {
        state.band_defaults.get(selected_band)
    };
    state.band = BandConfig {
        preset: selected_band,
        start_hz: start * 1.0e6,
        stop_hz: stop * 1.0e6,
        bin_width_hz: bin_khz * 1.0e3,
    };
    state.profile_name = profile.name;
    state.profile_path = source_path.to_owned();
    clear_history(state);
    scan_wifi_now_if_idle(state);
    state.status = format!(
        "Loaded profile '{}' from {source_path}.",
        state.profile_name
    );
}

fn scan_wifi_now(state: &mut RuntimeState) {
    let mut scanner = SystemWifiScanner::new(state.selected_wifi_backend);
    let band = state.band.clone();
    let selected = state.selected_wifi_interface.trim().to_owned();
    let selected = if selected.is_empty() {
        None
    } else {
        Some(selected.as_str())
    };
    #[cfg(target_os = "linux")]
    let report = {
        let helper = if state.selected_wifi_backend == WifiBackend::PrivilegedHelper {
            state.wifi_helper.as_mut()
        } else {
            None
        };
        scanner.scan(&band, selected, helper)
    };
    #[cfg(not(target_os = "linux"))]
    let report = scanner.scan(&band, selected);
    eprintln!("[sdrwispy] {}", report.status);
    state.aps = report.aps;
    state.wifi_interfaces = report.interfaces;
    state.wifi_status = report.status;
    state.last_wifi_scan = Instant::now();
}

fn scan_wifi_now_if_idle(state: &mut RuntimeState) {
    if state.wifi_scan_in_progress {
        state.wifi_status =
            "Wi-Fi scan is already running; current RF/interface change will use the next scan."
                .to_owned();
        return;
    }

    scan_wifi_now(state);
}

#[derive(Clone)]
struct WifiScanPlan {
    backend: WifiBackend,
    band: BandConfig,
    selected_interface: String,
}

fn scan_wifi_command(app: State<'_, BackendState>, auto: bool) -> Result<AppSnapshot, String> {
    #[cfg(target_os = "linux")]
    let (plan, mut helper) = {
        let mut state = app.inner.lock().map_err(|_| "state lock poisoned")?;
        drain_worker_messages(&mut state);
        if let Some(snapshot) = skip_wifi_scan_snapshot(&mut state, auto) {
            return Ok(snapshot);
        }

        let plan = wifi_scan_plan(&state);
        let helper = if plan.backend == WifiBackend::PrivilegedHelper {
            state.wifi_helper.take()
        } else {
            None
        };
        state.wifi_scan_in_progress = true;
        (plan, helper)
    };

    #[cfg(not(target_os = "linux"))]
    let plan = {
        let mut state = app.inner.lock().map_err(|_| "state lock poisoned")?;
        drain_worker_messages(&mut state);
        if let Some(snapshot) = skip_wifi_scan_snapshot(&mut state, auto) {
            return Ok(snapshot);
        }

        let plan = wifi_scan_plan(&state);
        state.wifi_scan_in_progress = true;
        plan
    };

    #[cfg(target_os = "linux")]
    let report = run_wifi_scan_plan(&plan, helper.as_mut());
    #[cfg(not(target_os = "linux"))]
    let report = run_wifi_scan_plan(&plan);

    let mut state = app.inner.lock().map_err(|_| "state lock poisoned")?;
    #[cfg(target_os = "linux")]
    let helper_stopped = if let Some(mut helper) = helper.take() {
        let usable = helper.is_usable();
        if usable
            && state.selected_wifi_backend == WifiBackend::PrivilegedHelper
            && state.wifi_helper.is_none()
        {
            state.wifi_helper = Some(helper);
            false
        } else {
            !usable
        }
    } else {
        false
    };

    state.wifi_scan_in_progress = false;
    if wifi_scan_plan_matches(&plan, &state) {
        apply_wifi_scan_report(&mut state, report);
        #[cfg(target_os = "linux")]
        if helper_stopped && state.selected_wifi_backend == WifiBackend::PrivilegedHelper {
            state.wifi_status.push_str(
                " Privileged Wi-Fi helper stopped; start it again before the next privileged scan.",
            );
        }
    } else {
        state.wifi_status = "Wi-Fi scan completed, but RF band, interface, or backend changed before the result was applied.".to_owned();
        state.last_wifi_scan = Instant::now();
    }
    Ok(snapshot(&state))
}

fn skip_wifi_scan_snapshot(state: &mut RuntimeState, auto: bool) -> Option<AppSnapshot> {
    if state.wifi_scan_in_progress {
        if !auto {
            state.wifi_status = "Wi-Fi scan is already running.".to_owned();
        }
        return Some(snapshot(state));
    }

    if auto {
        let interval = Duration::from_secs(state.wifi_auto_scan_interval_secs);
        if !state.wifi_auto_scan_enabled || state.last_wifi_scan.elapsed() < interval {
            return Some(snapshot(state));
        }
        #[cfg(target_os = "linux")]
        if state.selected_wifi_backend == WifiBackend::PrivilegedHelper
            && state.wifi_helper.is_none()
        {
            return Some(snapshot(state));
        }
    }

    None
}

fn wifi_scan_plan(state: &RuntimeState) -> WifiScanPlan {
    WifiScanPlan {
        backend: state.selected_wifi_backend,
        band: state.band.clone(),
        selected_interface: state.selected_wifi_interface.trim().to_owned(),
    }
}

#[cfg(target_os = "linux")]
fn run_wifi_scan_plan(
    plan: &WifiScanPlan,
    helper: Option<&mut wifi::PrivilegedWifiHelper>,
) -> wifi::WifiScanReport {
    let mut scanner = SystemWifiScanner::new(plan.backend);
    let selected = selected_wifi_interface_arg(plan);
    scanner.scan(&plan.band, selected, helper)
}

#[cfg(not(target_os = "linux"))]
fn run_wifi_scan_plan(plan: &WifiScanPlan) -> wifi::WifiScanReport {
    let mut scanner = SystemWifiScanner::new(plan.backend);
    let selected = selected_wifi_interface_arg(plan);
    scanner.scan(&plan.band, selected)
}

fn selected_wifi_interface_arg(plan: &WifiScanPlan) -> Option<&str> {
    let selected = plan.selected_interface.trim();
    if selected.is_empty() {
        None
    } else {
        Some(selected)
    }
}

fn wifi_scan_plan_matches(plan: &WifiScanPlan, state: &RuntimeState) -> bool {
    plan.backend == state.selected_wifi_backend
        && plan.selected_interface == state.selected_wifi_interface.trim()
        && plan.band.start_hz == state.band.start_hz
        && plan.band.stop_hz == state.band.stop_hz
        && plan.band.bin_width_hz == state.band.bin_width_hz
}

fn apply_wifi_scan_report(state: &mut RuntimeState, report: wifi::WifiScanReport) {
    let scanned_count = report.aps.len();
    let mut status = report.status;
    state.aps = merge_wifi_ap_cache(&state.aps, report.aps, &state.band);
    let shown_count = state.aps.len();
    if shown_count > scanned_count {
        if scanned_count == 0 {
            status.push_str(&format!(
                " Keeping {shown_count} AP(s) from the recent scan cache."
            ));
        } else {
            status.push_str(&format!(
                " Showing {shown_count} AP(s), including {} from the recent scan cache.",
                shown_count - scanned_count
            ));
        }
    }
    eprintln!("[sdrwispy] {status}");
    state.wifi_interfaces = report.interfaces;
    state.wifi_status = status;
    state.last_wifi_scan = Instant::now();
}

fn merge_wifi_ap_cache(
    existing: &[WifiBss],
    scanned: Vec<WifiBss>,
    band: &BandConfig,
) -> Vec<WifiBss> {
    let now = now_ns();
    let ttl_ns = WIFI_AP_CACHE_TTL_SECS.saturating_mul(1_000_000_000);
    let cutoff = now.saturating_sub(ttl_ns);
    let mut merged = existing
        .iter()
        .filter(|ap| ap.last_seen_ns >= cutoff && wifi_ap_in_band(ap, band))
        .cloned()
        .collect::<Vec<_>>();

    for ap in scanned {
        if !wifi_ap_in_band(&ap, band) {
            continue;
        }
        if let Some(slot) = merged
            .iter_mut()
            .find(|old| old.bssid.eq_ignore_ascii_case(&ap.bssid))
        {
            *slot = ap;
        } else {
            merged.push(ap);
        }
    }

    merged.sort_by(|a, b| {
        a.center_freq_hz
            .partial_cmp(&b.center_freq_hz)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.last_seen_ns.cmp(&a.last_seen_ns))
            .then_with(|| b.rssi_dbm.cmp(&a.rssi_dbm))
    });
    merged
}

fn wifi_ap_in_band(ap: &WifiBss, band: &BandConfig) -> bool {
    ap.primary_freq_hz >= band.start_hz && ap.primary_freq_hz <= band.stop_hz
}

fn clamp_wifi_auto_scan_interval(secs: u64) -> u64 {
    secs.clamp(WIFI_AUTO_SCAN_MIN_SECS, WIFI_AUTO_SCAN_MAX_SECS)
}

fn wifi_helper_running(state: &RuntimeState) -> bool {
    #[cfg(target_os = "linux")]
    {
        state.wifi_helper.is_some()
            || (state.selected_wifi_backend == WifiBackend::PrivilegedHelper
                && state.wifi_scan_in_progress)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = state;
        false
    }
}

#[cfg(target_os = "linux")]
fn start_wifi_helper_locked(state: &mut RuntimeState) -> Result<(), String> {
    if state.wifi_scan_in_progress {
        return Err(
            "Wi-Fi scan is already running; wait for it to finish before starting the helper."
                .to_owned(),
        );
    }
    if let Some(helper) = state.wifi_helper.as_mut()
        && !helper.is_usable()
    {
        state.wifi_helper = None;
    }
    if state.wifi_helper.is_none() {
        state.wifi_status = "Starting privileged Wi-Fi helper...".to_owned();
        match wifi::PrivilegedWifiHelper::start() {
            Ok(helper) => state.wifi_helper = Some(helper),
            Err(err) => {
                state.wifi_status = format!("Could not start privileged Wi-Fi helper: {err}");
                return Err(state.wifi_status.clone());
            }
        }
    }
    state.selected_wifi_backend = WifiBackend::PrivilegedHelper;
    state.wifi_status = "Privileged Wi-Fi helper is running. Press Scan to refresh APs.".to_owned();
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn start_wifi_helper_locked(_state: &mut RuntimeState) -> Result<(), String> {
    Err(format!(
        "Privileged Wi-Fi helper is only available on Linux, not {}.",
        std::env::consts::OS
    ))
}

fn has_real_sdr_device(devices: &[SdrDeviceInfo]) -> bool {
    devices.iter().any(|d| d.driver != "soapy-info")
}

fn sdr_catalog_status(devices: &[SdrDeviceInfo]) -> String {
    if has_real_sdr_device(devices) {
        return "SDR devices refreshed. Select a device or press Run.".to_owned();
    }

    let detail = devices
        .iter()
        .find(|d| d.driver == "soapy-info")
        .map(|d| d.label.as_str())
        .unwrap_or("No SDR devices were reported by the catalog");
    format!("No usable SDR device: {detail}")
}

fn clear_history(state: &mut RuntimeState) {
    state.history.clear();
}

#[cfg(feature = "soapy")]
fn push_history(state: &mut RuntimeState, frame: SpectrumFrame) {
    while state.history.len() >= state.history_capacity {
        state.history.pop_front();
    }
    state.history.push_back(frame);
}

#[cfg(feature = "soapy")]
fn latest_display_frame(state: &RuntimeState) -> Option<SpectrumFrame> {
    let complete = state.history.back().cloned();
    let progress = state
        .worker
        .as_ref()
        .and_then(|worker| worker.progress.lock().ok().and_then(|frame| frame.clone()));
    match (complete, progress) {
        (Some(complete), Some(progress))
            if (progress.capture_epoch, progress.frame_revision)
                > (complete.capture_epoch, complete.frame_revision) =>
        {
            Some(progress)
        }
        (Some(complete), _) => Some(complete),
        (None, Some(progress)) => Some(progress),
        (None, None) => None,
    }
}

#[cfg(not(feature = "soapy"))]
fn latest_display_frame(state: &RuntimeState) -> Option<SpectrumFrame> {
    state.history.back().cloned()
}

#[cfg(feature = "soapy")]
fn start_worker_locked(state: &mut RuntimeState) -> Result<(), String> {
    cleanup_finished_worker_locked(state);
    if state.worker.is_some() {
        stop_worker_locked(state, true);
    }
    if state.worker.is_some() {
        return Err("Previous SDR worker is still closing".to_owned());
    }
    let dev = state
        .devices
        .get(state.selected_device_index)
        .cloned()
        .ok_or_else(|| "No SDR selected".to_owned())?;
    if dev.driver == "soapy-info" {
        return Err("Select a real SoapySDR device".to_owned());
    }

    clear_history(state);
    state.capture_epoch = state.capture_epoch.wrapping_add(1).max(1);
    let epoch = state.capture_epoch;
    let merged_args = merge_soapy_args(&dev.id, &state.soapy_args);
    let args = merged_args;
    let band = state.band.clone();
    let sweep = state.sweep.clone();
    let gain_db = state.sdr_gain_db;
    let sample_rate_hz = sweep.sample_rate_hz;
    let bandwidth_hz = sweep.sample_rate_hz.max(1.0);
    let run = Arc::new(AtomicBool::new(true));
    let cancel = Arc::new(AtomicBool::new(false));
    let quit = Arc::new(AtomicBool::new(false));
    let progress = Arc::new(Mutex::new(None));
    let run_t = run.clone();
    let cancel_t = cancel.clone();
    let quit_t = quit.clone();
    let progress_t = progress.clone();
    let (tx, rx) = mpsc::sync_channel(16);

    state.status = format!(
        "Opening SoapySDR epoch #{epoch}: {args}, {:.2} MS/s",
        sample_rate_hz / 1.0e6
    );

    let join = std::thread::spawn(move || {
        let mut engine = match RealSweepEngine::open(args, sample_rate_hz, bandwidth_hz, gain_db) {
            Ok(engine) => engine,
            Err(err) => {
                let _ = tx.try_send(WorkerMessage::Error(format!("Open failed: {err}")));
                return;
            }
        };
        let _ = tx.try_send(WorkerMessage::Status(format!(
            "epoch #{epoch}: SoapySDR stream opened; waiting for first sweep frame"
        )));
        let mut empty_sweeps = 0usize;
        while !quit_t.load(Ordering::Relaxed) {
            if !run_t.load(Ordering::Relaxed) {
                let _ = engine.pause();
                std::thread::sleep(Duration::from_millis(25));
                continue;
            }
            match engine.generate(&band, &sweep, &cancel_t, |mut frame| {
                if quit_t.load(Ordering::Relaxed) || cancel_t.load(Ordering::Relaxed) {
                    return;
                }
                frame.capture_epoch = epoch;
                if let Ok(mut latest) = progress_t.lock() {
                    *latest = Some(frame);
                }
            }) {
                Ok(Some(report)) => {
                    empty_sweeps = 0;
                    if quit_t.load(Ordering::Relaxed) || cancel_t.load(Ordering::Relaxed) {
                        continue;
                    }
                    let _ = tx.try_send(WorkerMessage::Report { epoch, report });
                }
                Ok(None) => {
                    empty_sweeps = empty_sweeps.saturating_add(1);
                    if empty_sweeps == 1 || empty_sweeps.is_multiple_of(50) {
                        let _ = tx.try_send(WorkerMessage::Status(format!(
                            "epoch #{epoch}: no complete sweep frame yet (cancel={}, quit={}, none={empty_sweeps})",
                            cancel_t.load(Ordering::Relaxed),
                            quit_t.load(Ordering::Relaxed),
                        )));
                    }
                }
                Err(err) => {
                    let _ = tx.try_send(WorkerMessage::Error(err));
                    engine.shutdown();
                    return;
                }
            }
        }
        engine.shutdown();
        let _ = tx.try_send(WorkerMessage::Status("SoapySDR worker stopped".to_owned()));
    });

    state.worker = Some(CaptureWorker {
        epoch,
        run,
        cancel,
        quit,
        rx,
        progress,
        join: Some(join),
    });
    state.running = true;
    Ok(())
}

#[cfg(not(feature = "soapy"))]
fn start_worker_locked(_state: &mut RuntimeState) -> Result<(), String> {
    Err("This Tauri build was compiled with SoapySDR support disabled".to_owned())
}

fn stop_worker_locked(state: &mut RuntimeState, blocking: bool) {
    #[cfg(not(feature = "soapy"))]
    let _ = blocking;
    #[cfg(feature = "soapy")]
    if let Some(worker) = &state.worker {
        worker.run.store(false, Ordering::Relaxed);
        worker.cancel.store(true, Ordering::Relaxed);
        worker.quit.store(true, Ordering::Relaxed);
        if let Ok(mut progress) = worker.progress.lock() {
            *progress = None;
        }
    }
    state.running = false;

    #[cfg(feature = "soapy")]
    if blocking && let Some(mut worker) = state.worker.take() {
        if let Some(join) = worker.join.take() {
            let _ = join.join();
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

fn drain_worker_messages(state: &mut RuntimeState) {
    #[cfg(not(feature = "soapy"))]
    let _ = state;
    #[cfg(feature = "soapy")]
    {
        let mut latest = None;
        let mut latest_status = None;
        let mut saw_error = None;
        let mut disconnected = false;
        let mut worker_epoch = state.capture_epoch;
        if let Some(worker) = &state.worker {
            worker_epoch = worker.epoch;
            for _ in 0..32 {
                match worker.rx.try_recv() {
                    Ok(WorkerMessage::Report { epoch, mut report }) => {
                        report.frame.capture_epoch = epoch;
                        worker_epoch = epoch;
                        latest = Some(report);
                    }
                    Ok(WorkerMessage::Status(status)) => latest_status = Some(status),
                    Ok(WorkerMessage::Error(err)) => {
                        saw_error = Some(err);
                        break;
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }
        }
        let had_report = latest.is_some();
        if let Some(report) = latest {
            let frame_id = report.frame.frame_id;
            let frame_revision = report.frame.frame_revision;
            let total_bins = report.frame.current_db.len();
            let valid = report.valid_bin_count;
            let empty_timeout_frame = report.captured_samples == 0 && report.valid_bin_count == 0;
            if !empty_timeout_frame {
                push_history(state, report.frame);
                clear_stale_progress_frame(state, worker_epoch, frame_revision);
            }
            state.status = format!(
                "epoch #{worker_epoch} frame #{frame_id}: hops {}, samples {}, valid bins {}/{}, min/max {:.1}/{:.1} dB, overflow {}, timeout {}, corruption {}, stream reset {}, {}",
                report.hop_count,
                report.captured_samples,
                valid,
                total_bins,
                report.min_db,
                report.max_db,
                report.overflow_count,
                report.timeout_count,
                report.corruption_count,
                if report.stream_reset { "yes" } else { "no" },
                if empty_timeout_frame {
                    "empty timeout frame not plotted"
                } else {
                    "plotted"
                }
            );
        }
        if !had_report && let Some(status) = latest_status {
            state.status = status;
        }
        if let Some(err) = saw_error {
            state.status = format!("Real sweep failed: {err}");
            stop_worker_locked(state, false);
        }
        if disconnected {
            state.running = false;
            cleanup_finished_worker_locked(state);
        }
        cleanup_finished_worker_locked(state);
    }
}

#[cfg(feature = "soapy")]
fn clear_stale_progress_frame(state: &RuntimeState, epoch: u64, completed_revision: u64) {
    let Some(worker) = &state.worker else {
        return;
    };
    let Ok(mut progress) = worker.progress.lock() else {
        return;
    };
    let stale = progress.as_ref().is_some_and(|frame| {
        frame.capture_epoch == epoch && frame.frame_revision <= completed_revision
    });
    if stale {
        *progress = None;
    }
}

fn profile_toml_from_state(state: &RuntimeState) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "name = \"{}\"", toml_escape(&state.profile_name));
    let _ = writeln!(
        out,
        "default_band = \"{}\"\n",
        preset_key(state.band.preset)
    );

    out.push_str("[receiver]\n");
    if let Some(device) = state.devices.get(state.selected_device_index)
        && device.driver != "soapy-info"
    {
        let _ = writeln!(out, "device_id = \"{}\"", toml_escape(&device.id));
    }
    let _ = writeln!(out, "gain_db = {:.1}", state.sdr_gain_db);
    if !state.soapy_args.trim().is_empty() {
        let _ = writeln!(
            out,
            "soapy_args = \"{}\"",
            toml_escape(state.soapy_args.trim())
        );
    }

    out.push_str("\n[sweep]\n");
    let _ = writeln!(
        out,
        "sample_rate_msps = {:.6}",
        state.sweep.sample_rate_hz / 1.0e6
    );
    let _ = writeln!(
        out,
        "usable_bandwidth_ratio = {:.4}",
        state.sweep.usable_bandwidth_ratio
    );
    let _ = writeln!(out, "overlap_mhz = {:.6}", state.sweep.overlap_hz / 1.0e6);
    let _ = writeln!(out, "dwell_ms = {:.3}", state.sweep.dwell_ms);
    let _ = writeln!(out, "settle_ms = {:.3}", state.sweep.settle_ms);
    let _ = writeln!(out, "fft_size = {}", state.sweep.fft_size);

    out.push_str("\n[display]\n");
    let _ = writeln!(out, "show_current = {}", state.display.show_current);
    let _ = writeln!(out, "show_average = {}", state.display.show_average);
    let _ = writeln!(out, "show_max_hold = {}", state.display.show_max_hold);
    let _ = writeln!(
        out,
        "show_wifi_overlay = {}",
        state.display.show_wifi_overlay
    );
    let _ = writeln!(out, "show_wifi_labels = {}", state.display.show_wifi_labels);
    let _ = writeln!(
        out,
        "label_aps_by_bssid = {}",
        state.display.label_aps_by_bssid
    );
    let _ = writeln!(
        out,
        "wifi_overlay_rssi_floor_dbm = {:.1}",
        state.display.wifi_overlay_rssi_floor_dbm
    );
    let _ = writeln!(
        out,
        "wifi_overlay_rssi_ceiling_dbm = {:.1}",
        state.display.wifi_overlay_rssi_ceiling_dbm
    );
    let _ = writeln!(
        out,
        "wifi_overlay_floor_db = {:.1}",
        state.display.wifi_overlay_floor_db
    );
    let _ = writeln!(
        out,
        "wifi_overlay_ceiling_db = {:.1}",
        state.display.wifi_overlay_ceiling_db
    );

    for preset in [BandPreset::Ghz2, BandPreset::Ghz5, BandPreset::Ghz6] {
        let (start, stop, bin_khz) = state.band_defaults.get(preset);
        let _ = writeln!(out, "\n[bands.{}]", preset_key(preset));
        let _ = writeln!(out, "start_mhz = {start:.6}");
        let _ = writeln!(out, "stop_mhz = {stop:.6}");
        let _ = writeln!(out, "bin_width_khz = {bin_khz:.6}");
    }
    if state.band.preset == BandPreset::Custom {
        out.push_str("\n[bands.custom]\n");
        let _ = writeln!(out, "start_mhz = {:.6}", state.band.start_hz / 1.0e6);
        let _ = writeln!(out, "stop_mhz = {:.6}", state.band.stop_hz / 1.0e6);
        let _ = writeln!(
            out,
            "bin_width_khz = {:.6}",
            state.band.bin_width_hz / 1.0e3
        );
    }

    out.push_str("\n[wifi]\n");
    let hidden: Vec<_> = state.hidden_wifi_overlay_bssids.iter().cloned().collect();
    let _ = writeln!(
        out,
        "hidden_overlay_bssids = {}",
        toml_string_array(&hidden)
    );

    out
}

fn preset_key(preset: BandPreset) -> &'static str {
    match preset {
        BandPreset::Ghz2 => "2.4g",
        BandPreset::Ghz5 => "5g",
        BandPreset::Ghz6 => "6e",
        BandPreset::Custom => "custom",
    }
}

fn toml_escape(value: &str) -> String {
    value
        .chars()
        .flat_map(|ch| match ch {
            '\\' => "\\\\".chars().collect::<Vec<_>>(),
            '"' => "\\\"".chars().collect::<Vec<_>>(),
            '\n' => "\\n".chars().collect::<Vec<_>>(),
            '\r' => "\\r".chars().collect::<Vec<_>>(),
            '\t' => "\\t".chars().collect::<Vec<_>>(),
            _ => vec![ch],
        })
        .collect()
}

fn toml_string_array(values: &[String]) -> String {
    let escaped = values
        .iter()
        .map(|value| format!("\"{}\"", toml_escape(value)))
        .collect::<Vec<_>>()
        .join(", ");
    format!("[{escaped}]")
}

fn profile_name_from_path(path: &str) -> String {
    std::path::Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("Saved profile")
        .to_owned()
}

#[cfg(feature = "soapy")]
fn merge_soapy_args(base: &str, extra: &str) -> String {
    let extra = extra.trim();
    if extra.is_empty() {
        return base.trim().to_owned();
    }
    let mut parts: Vec<String> = base
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .collect();

    for item in extra.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        if let Some((key, _)) = item.split_once('=') {
            let key = key.trim();
            parts.retain(|part| {
                part.split_once('=')
                    .map(|(existing, _)| !existing.trim().eq_ignore_ascii_case(key))
                    .unwrap_or(true)
            });
        }
        parts.push(item.to_owned());
    }

    parts.join(", ")
}

#[cfg(feature = "soapy")]
fn cleanup_finished_worker_locked(state: &mut RuntimeState) {
    let finished = state
        .worker
        .as_ref()
        .and_then(|w| w.join.as_ref())
        .is_some_and(|j| j.is_finished());
    if finished {
        if let Some(mut worker) = state.worker.take()
            && let Some(join) = worker.join.take()
        {
            let _ = join.join();
        }
        state.running = false;
    }
}

fn parse_preset(value: &str) -> Option<BandPreset> {
    match value.trim().to_ascii_lowercase().as_str() {
        "2g" | "2.4g" | "24g" | "ghz2" => Some(BandPreset::Ghz2),
        "5g" | "ghz5" => Some(BandPreset::Ghz5),
        "6g" | "6e" | "ghz6" => Some(BandPreset::Ghz6),
        _ => None,
    }
}

fn resolve_profile_path(path: &str) -> std::path::PathBuf {
    let requested = std::path::PathBuf::from(path);
    if requested.exists() {
        return requested;
    }

    if let Some(name) = requested.file_name() {
        for dir in ["profiles", "../profiles"] {
            let candidate = std::path::Path::new(dir).join(name);
            if candidate.exists() {
                return candidate;
            }
        }
    }

    requested
}

fn main() {
    #[cfg(target_os = "linux")]
    if let Some(code) = wifi::run_wifi_helper_from_args() {
        std::process::exit(code);
    }

    tauri::Builder::default()
        .manage(BackendState {
            inner: Mutex::new(initial_runtime_state()),
        })
        .invoke_handler(tauri::generate_handler![
            get_snapshot,
            get_latest_frame,
            list_sdr_devices,
            select_sdr_device,
            set_band_preset,
            patch_band,
            patch_sweep,
            patch_display,
            patch_receiver,
            load_profile,
            save_profile,
            start_capture,
            stop_capture,
            scan_wifi,
            scan_wifi_auto,
            set_wifi_auto_scan,
            select_wifi_interface,
            select_wifi_backend,
            start_wifi_helper,
            set_wifi_overlay_hidden,
            set_wifi_overlay_all,
        ])
        .plugin(tauri_plugin_dialog::init())
        .run(tauri::generate_context!())
        .expect("error while running SDRWispy Tauri app");
}
