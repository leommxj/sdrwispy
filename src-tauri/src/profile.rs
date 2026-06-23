use std::{collections::BTreeMap, fs, path::Path};

use serde::Deserialize;

use crate::model::{BandPreset, SweepConfig};

#[derive(Debug, Clone)]
pub struct RuntimeProfile {
    pub name: String,
    pub default_band: Option<BandPreset>,
    pub bands_mhz: Vec<(BandPreset, (f64, f64, f64))>,
    pub sweep: Option<SweepConfig>,
    pub selected_device_id: Option<String>,
    pub sdr_gain_db: Option<f64>,
    pub soapy_args: Option<String>,
    pub show_current: Option<bool>,
    pub show_average: Option<bool>,
    pub show_max_hold: Option<bool>,
    pub show_wifi_overlay: Option<bool>,
    pub show_wifi_labels: Option<bool>,
    pub label_aps_by_bssid: Option<bool>,
    pub wifi_overlay_rssi_floor_dbm: Option<f64>,
    pub wifi_overlay_rssi_ceiling_dbm: Option<f64>,
    pub wifi_overlay_floor_db: Option<f64>,
    pub wifi_overlay_ceiling_db: Option<f64>,
    pub hidden_wifi_overlay_bssids: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct ProfileToml {
    name: Option<String>,
    default_band: Option<String>,
    bands: Option<BTreeMap<String, BandToml>>,
    sweep: Option<SweepToml>,
    receiver: Option<ReceiverToml>,
    display: Option<DisplayToml>,
    wifi: Option<WifiToml>,
}

#[derive(Debug, Deserialize)]
struct BandToml {
    start_mhz: f64,
    stop_mhz: f64,
    #[serde(default)]
    bin_width_khz: Option<f64>,
    #[serde(default)]
    bin_count: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct SweepToml {
    #[serde(default)]
    sample_rate_msps: Option<f64>,
    #[serde(default)]
    sample_rate_hz: Option<f64>,
    usable_bandwidth_ratio: Option<f64>,
    #[serde(default)]
    overlap_mhz: Option<f64>,
    #[serde(default)]
    overlap_hz: Option<f64>,
    dwell_ms: Option<f64>,
    settle_ms: Option<f64>,
    fft_size: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct ReceiverToml {
    #[serde(default, alias = "selected_device_id")]
    device_id: Option<String>,
    gain_db: Option<f64>,
    soapy_args: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DisplayToml {
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

#[derive(Debug, Deserialize)]
struct WifiToml {
    #[serde(default)]
    hidden_overlay_bssids: Option<Vec<String>>,
}

pub fn load_profile_toml(path: impl AsRef<Path>) -> Result<RuntimeProfile, String> {
    let path = path.as_ref();
    let text = fs::read_to_string(path).map_err(|e| format!("read profile failed: {e}"))?;
    parse_profile_toml_text(&text, &path.to_string_lossy())
}

fn parse_profile_toml_text(text: &str, source_name: &str) -> Result<RuntimeProfile, String> {
    let raw: ProfileToml =
        toml::from_str(text).map_err(|e| format!("parse profile failed: {e}"))?;

    let mut bands_mhz = Vec::new();
    if let Some(bands) = raw.bands {
        for (key, band) in bands {
            let Some(preset) = parse_band_preset(&key) else {
                return Err(format!("unsupported band key {key:?}"));
            };
            let bin_width_khz = match (band.bin_width_khz, band.bin_count) {
                (Some(width), _) => width,
                (None, Some(count)) if count > 0.0 => {
                    (band.stop_mhz - band.start_mhz).max(0.001) * 1000.0 / count
                }
                _ => {
                    return Err(format!(
                        "band {key:?} must provide bin_width_khz or bin_count"
                    ));
                }
            };
            bands_mhz.push((preset, (band.start_mhz, band.stop_mhz, bin_width_khz)));
        }
    }

    let sweep = raw.sweep.map(|s| {
        let mut cfg = SweepConfig::default();
        if let Some(v) = s.sample_rate_hz {
            cfg.sample_rate_hz = v;
        }
        if let Some(v) = s.sample_rate_msps {
            cfg.sample_rate_hz = v * 1.0e6;
        }
        if let Some(v) = s.usable_bandwidth_ratio {
            cfg.usable_bandwidth_ratio = v;
        }
        if let Some(v) = s.overlap_hz {
            cfg.overlap_hz = v;
        }
        if let Some(v) = s.overlap_mhz {
            cfg.overlap_hz = v * 1.0e6;
        }
        if let Some(v) = s.dwell_ms {
            cfg.dwell_ms = v;
        }
        if let Some(v) = s.settle_ms {
            cfg.settle_ms = v;
        }
        if let Some(v) = s.fft_size {
            cfg.fft_size = v;
        }
        cfg
    });

    let display = raw.display;

    let receiver = raw.receiver;
    let wifi = raw.wifi;

    Ok(RuntimeProfile {
        name: raw
            .name
            .unwrap_or_else(|| profile_name_from_source(source_name)),
        default_band: raw.default_band.as_deref().and_then(parse_band_preset),
        bands_mhz,
        sweep,
        selected_device_id: receiver.as_ref().and_then(|r| {
            r.device_id
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(ToOwned::to_owned)
        }),
        sdr_gain_db: receiver.as_ref().and_then(|r| r.gain_db),
        soapy_args: receiver.and_then(|r| r.soapy_args),
        show_current: display.as_ref().and_then(|d| d.show_current),
        show_average: display.as_ref().and_then(|d| d.show_average),
        show_max_hold: display.as_ref().and_then(|d| d.show_max_hold),
        show_wifi_overlay: display.as_ref().and_then(|d| d.show_wifi_overlay),
        show_wifi_labels: display.as_ref().and_then(|d| d.show_wifi_labels),
        label_aps_by_bssid: display.as_ref().and_then(|d| d.label_aps_by_bssid),
        wifi_overlay_rssi_floor_dbm: display.as_ref().and_then(|d| d.wifi_overlay_rssi_floor_dbm),
        wifi_overlay_rssi_ceiling_dbm: display
            .as_ref()
            .and_then(|d| d.wifi_overlay_rssi_ceiling_dbm),
        wifi_overlay_floor_db: display.as_ref().and_then(|d| d.wifi_overlay_floor_db),
        wifi_overlay_ceiling_db: display.as_ref().and_then(|d| d.wifi_overlay_ceiling_db),
        hidden_wifi_overlay_bssids: wifi.and_then(|w| w.hidden_overlay_bssids),
    })
}

fn profile_name_from_source(source_name: &str) -> String {
    Path::new(source_name)
        .file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("Unnamed profile")
        .to_owned()
}

fn parse_band_preset(value: &str) -> Option<BandPreset> {
    let key = value
        .trim()
        .to_ascii_lowercase()
        .replace(['_', '-', ' ', '.'], "");
    match key.as_str() {
        "2g" | "24g" | "2ghz" | "24ghz" | "ghz2" | "ghz24" | "wifi24" => Some(BandPreset::Ghz2),
        "5g" | "5ghz" | "ghz5" | "58g" | "58ghz" | "wifi5" => Some(BandPreset::Ghz5),
        "6g" | "6ghz" | "6e" | "ghz6" | "wifi6e" => Some(BandPreset::Ghz6),
        "custom" => Some(BandPreset::Custom),
        _ => None,
    }
}
