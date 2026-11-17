use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BandPreset {
    Ghz2,
    Ghz5,
    Ghz6,
    Custom,
}

impl BandPreset {
    pub fn defaults_mhz(self) -> (f64, f64, f64) {
        match self {
            BandPreset::Ghz2 => (2400.0, 2485.0, 100.0),
            BandPreset::Ghz5 => (5150.0, 5925.0, 250.0),
            BandPreset::Ghz6 => (5925.0, 7125.0, 500.0),
            BandPreset::Custom => (2400.0, 2500.0, 100.0),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BandConfig {
    pub preset: BandPreset,
    pub start_hz: f64,
    pub stop_hz: f64,
    /// Output grid width. This is display/storage resolution, not FFT RBW.
    pub bin_width_hz: f64,
}

impl BandConfig {
    pub fn from_preset(preset: BandPreset) -> Self {
        let (start_mhz, stop_mhz, bin_khz) = preset.defaults_mhz();
        Self {
            preset,
            start_hz: start_mhz * 1.0e6,
            stop_hz: stop_mhz * 1.0e6,
            bin_width_hz: bin_khz * 1.0e3,
        }
    }

    #[cfg(feature = "soapy")]
    pub fn span_hz(&self) -> f64 {
        (self.stop_hz - self.start_hz).max(self.bin_width_hz)
    }

    #[cfg(feature = "soapy")]
    pub fn bin_count(&self) -> usize {
        (self.span_hz() / self.bin_width_hz)
            .ceil()
            .clamp(16.0, 16_384.0) as usize
    }
}

impl Default for BandConfig {
    fn default() -> Self {
        Self::from_preset(BandPreset::Ghz2)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SweepConfig {
    pub sample_rate_hz: f64,
    pub usable_bandwidth_ratio: f64,
    pub overlap_hz: f64,
    pub dwell_ms: f64,
    pub settle_ms: f64,
    pub fft_size: usize,
    #[serde(default = "default_stream_gating")]
    pub stream_gating: bool,
}

fn default_stream_gating() -> bool {
    true
}

impl Default for SweepConfig {
    fn default() -> Self {
        Self {
            sample_rate_hz: 20.0e6,
            usable_bandwidth_ratio: 0.80,
            overlap_hz: 5.0e6,
            dwell_ms: 3.0,
            settle_ms: 2.0,
            fft_size: 4096,
            stream_gating: default_stream_gating(),
        }
    }
}

impl SweepConfig {
    pub fn validate(&self) -> Result<(), String> {
        if !self.sample_rate_hz.is_finite() || self.sample_rate_hz <= 0.0 {
            return Err("sample rate must be finite and positive".to_owned());
        }
        if !self.usable_bandwidth_ratio.is_finite()
            || !(0.10..=0.95).contains(&self.usable_bandwidth_ratio)
        {
            return Err("usable bandwidth ratio must be between 0.10 and 0.95".to_owned());
        }
        if !self.overlap_hz.is_finite() || self.overlap_hz < 0.0 {
            return Err("overlap must be finite and nonnegative".to_owned());
        }
        if !self.dwell_ms.is_finite() || self.dwell_ms <= 0.0 {
            return Err("dwell time must be finite and positive".to_owned());
        }
        if !self.settle_ms.is_finite() || self.settle_ms < 0.0 {
            return Err("settle time must be finite and nonnegative".to_owned());
        }
        if !(256..=65_536).contains(&self.fft_size) {
            return Err("FFT size must be between 256 and 65536".to_owned());
        }
        Ok(())
    }

    #[cfg(feature = "soapy")]
    pub fn usable_bandwidth_hz(&self) -> f64 {
        self.sample_rate_hz * self.usable_bandwidth_ratio.clamp(0.10, 0.95)
    }

    #[cfg(feature = "soapy")]
    pub fn hop_step_hz(&self) -> f64 {
        (self.usable_bandwidth_hz() - self.overlap_hz).max(self.sample_rate_hz * 0.10)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SdrDeviceInfo {
    pub id: String,
    pub label: String,
    pub driver: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpectrumFrame {
    /// Monotonic capture run id assigned by the backend.
    ///
    /// `frame_id` is intentionally reset by each RealSweepEngine instance, so
    /// the frontend must compare `(capture_epoch, frame_id)` instead of only
    /// `frame_id`. Otherwise a stop -> run sequence can suppress the first
    /// frame of the new run when its frame id collides with the previous run.
    pub capture_epoch: u64,
    pub frame_id: u64,
    pub frame_revision: u64,
    pub partial: bool,
    pub sweep_start_time_ns: i64,
    pub sweep_end_time_ns: i64,
    pub band_start_hz: f64,
    pub band_stop_hz: f64,
    pub bin_width_hz: f64,
    pub current_db: Vec<f32>,
    pub average_db: Vec<f32>,
    pub max_hold_db: Vec<f32>,
    pub valid_bin_ranges: Vec<[usize; 2]>,
    pub invalid_bin_count: usize,
    pub estimated_hops: usize,
    pub completed_hops: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WifiBss {
    pub ssid: String,
    pub bssid: String,
    pub primary_freq_hz: f64,
    pub center_freq_hz: f64,
    pub channel: i32,
    pub width_mhz: i32,
    pub rssi_dbm: i32,
    pub phy: String,
    pub last_seen_ns: i64,
}

pub fn now_ns() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let Ok(dur) = SystemTime::now().duration_since(UNIX_EPOCH) else {
        return 0;
    };
    dur.as_nanos().min(i64::MAX as u128) as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_sweep_payloads_keep_stream_gating_enabled() {
        let mut value = serde_json::to_value(SweepConfig::default()).unwrap();
        value.as_object_mut().unwrap().remove("stream_gating");
        let sweep: SweepConfig = serde_json::from_value(value).unwrap();
        assert!(sweep.stream_gating);
    }

    #[test]
    fn continuous_stream_setting_survives_serialization() {
        let sweep = SweepConfig {
            stream_gating: false,
            ..SweepConfig::default()
        };
        let text = serde_json::to_string(&sweep).unwrap();
        let restored: SweepConfig = serde_json::from_str(&text).unwrap();
        assert!(!restored.stream_gating);
    }
}
