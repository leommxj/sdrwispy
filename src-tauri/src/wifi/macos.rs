use serde::Deserialize;

use super::{WifiScanReport, decode_ssid_bytes, dedup_bss, freq_in_band, scan_complete_status};
use crate::model::{BandConfig, WifiBss, now_ns};

#[derive(Deserialize)]
struct NativeScan {
    interfaces: Vec<String>,
    #[serde(default)]
    interface: String,
    authorization: String,
    networks: Vec<NativeNetwork>,
    error: Option<String>,
}

#[derive(Deserialize)]
struct NativeNetwork {
    ssid: Vec<u8>,
    bssid: String,
    channel: i32,
    band: i32,
    width_mhz: i32,
    rssi_dbm: i32,
    phy: String,
    ies: Vec<u8>,
}

#[cfg(target_os = "macos")]
pub(super) fn scan(band: &BandConfig, selected_interface: Option<&str>) -> WifiScanReport {
    use std::ffi::{CStr, CString, c_char};

    unsafe extern "C" {
        fn sdrwispy_wifi_scan(interface: *const c_char, request_permission: bool) -> *mut c_char;
        fn sdrwispy_wifi_free(json: *mut c_char);
    }

    let result = (|| {
        let interface = CString::new(selected_interface.unwrap_or_default().trim())
            .map_err(|_| "Wi-Fi interface contains a NUL byte".to_owned())?;
        let ptr = unsafe { sdrwispy_wifi_scan(interface.as_ptr(), true) };
        if ptr.is_null() {
            return Err("CoreWLAN could not encode its scan result".to_owned());
        }
        let bytes = unsafe { CStr::from_ptr(ptr).to_bytes().to_vec() };
        unsafe { sdrwispy_wifi_free(ptr) };
        parse_scan(&bytes, band)
    })();
    result.unwrap_or_else(|error| WifiScanReport {
        aps: Vec::new(),
        interfaces: Vec::new(),
        status: format!("macOS Wi-Fi scan failed: {error}"),
    })
}

fn parse_scan(bytes: &[u8], band: &BandConfig) -> Result<WifiScanReport, String> {
    let scan: NativeScan = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    let mut aps = Vec::new();
    let mut redacted = 0;
    for network in &scan.networks {
        if !valid_bssid(&network.bssid) {
            redacted += 1;
            continue;
        }
        let Some(primary_freq_hz) = channel_frequency(network.band, network.channel) else {
            continue;
        };
        if !freq_in_band(primary_freq_hz, band) {
            continue;
        }
        let (width_mhz, center_freq_hz) = channel_geometry(network, primary_freq_hz);
        aps.push(WifiBss {
            ssid: decode_ssid_bytes(&network.ssid),
            bssid: network.bssid.to_ascii_lowercase(),
            primary_freq_hz,
            center_freq_hz,
            channel: network.channel,
            width_mhz,
            rssi_dbm: network.rssi_dbm,
            phy: network.phy.clone(),
            last_seen_ns: now_ns(),
        });
    }
    let aps = dedup_bss(aps);
    let mut status = match scan.error {
        Some(error) => format!("macOS Wi-Fi scan failed: {error}"),
        None => scan_complete_status(
            &format!("macOS CoreWLAN on {}", scan.interface),
            aps.len(),
            band,
        ),
    };
    if redacted > 0 {
        status.push_str(&format!(
            " macOS withheld BSSID identity for {redacted} of {} discovered AP(s); these are not plotted.",
            scan.networks.len()
        ));
    }
    if redacted > 0 || scan.authorization != "authorized" {
        status.push_str(&format!(
            " Location authorization: {}. Allow SDRWispy in System Settings > Privacy & Security > Location Services, then Scan again. Use the .app bundle for the permission prompt; SSH/terminal scans may have redacted SSIDs/BSSIDs.",
            scan.authorization
        ));
    }
    Ok(WifiScanReport {
        aps,
        interfaces: scan.interfaces,
        status,
    })
}

fn valid_bssid(value: &str) -> bool {
    let parts: Vec<_> = value.split(':').collect();
    parts.len() == 6
        && parts
            .iter()
            .all(|part| part.len() == 2 && part.bytes().all(|c| c.is_ascii_hexdigit()))
        && value != "00:00:00:00:00:00"
        && !value.eq_ignore_ascii_case("ff:ff:ff:ff:ff:ff")
}

fn channel_frequency(band: i32, channel: i32) -> Option<f64> {
    let mhz = match (band, channel) {
        (1, 1..=13) => 2407 + 5 * channel,
        (1, 14) => 2484,
        (2, 182..=196) => 4000 + 5 * channel,
        (2, 1..=200) => 5000 + 5 * channel,
        (3, 2) => 5935,
        (3, 1..=233) => 5950 + 5 * channel,
        _ => return None,
    };
    Some(f64::from(mhz) * 1.0e6)
}

fn channel_geometry(network: &NativeNetwork, primary: f64) -> (i32, f64) {
    let mut width = 20;
    let mut center = primary;
    let mut operation_geometry = None;
    let mut ies = network.ies.as_slice();
    while ies.len() >= 2 {
        let id = ies[0];
        let len = usize::from(ies[1]);
        if ies.len() < len + 2 {
            break;
        }
        let body = &ies[2..len + 2];
        if id == 61 && body.len() >= 2 {
            match body[1] & 3 {
                1 => {
                    width = 40;
                    center = primary + 10.0e6;
                }
                3 => {
                    width = 40;
                    center = primary - 10.0e6;
                }
                _ => {}
            }
        }
        if id == 192 && body.len() >= 3 && network.band == 2 {
            let segment0 = i32::from(body[1]);
            let segment1 = i32::from(body[2]);
            let geometry = match body[0] {
                1 if segment1 != 0 && (segment1 - segment0).abs() == 8 => Some((160, segment1)),
                // For 80+80, show only the primary 80 MHz segment, not the gap.
                1 | 3 => Some((80, segment0)),
                2 => Some((160, segment0)),
                _ => None,
            };
            if let Some((width, channel)) = geometry
                && let Some(center) = channel_frequency(network.band, channel)
                && (center - primary).abs() < f64::from(width) * 0.5e6
            {
                operation_geometry = Some((width, center));
            }
        }
        if id == 255
            && body.first() == Some(&36)
            && network.band == 3
            && let Some(geometry) = he_6ghz_geometry(body, primary)
        {
            operation_geometry = Some(geometry);
        }
        ies = &ies[len + 2..];
    }
    if let Some(geometry) = operation_geometry {
        return geometry;
    }
    // CoreWLAN supplies primary channel and width, not the center. Use standard
    // contiguous channel blocks; don't draw a wide channel centered on its primary.
    if let Some(center_channel) = wide_center(network.band, network.channel, network.width_mhz) {
        return (
            network.width_mhz,
            channel_frequency(network.band, center_channel).unwrap_or(primary),
        );
    }
    (width, center)
}

fn he_6ghz_geometry(body: &[u8], primary: f64) -> Option<(i32, f64)> {
    if body.len() < 7 {
        return None;
    }
    let parameters = u32::from_le_bytes([body[1], body[2], body[3], 0]);
    if parameters & 0x20000 == 0 {
        return None;
    }
    let offset = 7
        + if parameters & 0x4000 != 0 { 3 } else { 0 }
        + if parameters & 0x8000 != 0 { 1 } else { 0 };
    let operation = body.get(offset..offset + 5)?;
    let segment0 = i32::from(operation[2]);
    let segment1 = i32::from(operation[3]);
    let (width, center_channel) = match operation[1] & 3 {
        0 => (20, i32::from(operation[0])),
        1 => (40, segment0),
        2 => (80, segment0),
        3 if (segment1 - segment0).abs() == 8 => (160, segment1),
        // Render the primary segment of 80+80 without bridging its gap.
        3 => (80, segment0),
        _ => unreachable!(),
    };
    let center = channel_frequency(3, center_channel)?;
    ((center - primary).abs() < f64::from(width) * 0.5e6).then_some((width, center))
}

fn wide_center(band: i32, primary: i32, width: i32) -> Option<i32> {
    if band == 3
        && (1..=233).contains(&primary)
        && primary % 4 == 1
        && matches!(width, 40 | 80 | 160)
    {
        let channels = width / 5;
        let center = (primary - 1) / channels * channels + channels / 2 - 1;
        return (center + channels / 2 - 2 <= 233).then_some(center);
    }
    if band == 2 {
        let blocks: &[(i32, i32)] = match width {
            40 => &[
                (36, 38),
                (44, 46),
                (52, 54),
                (60, 62),
                (100, 102),
                (108, 110),
                (116, 118),
                (124, 126),
                (132, 134),
                (140, 142),
                (149, 151),
                (157, 159),
                (165, 167),
                (173, 175),
            ],
            80 => &[
                (36, 42),
                (52, 58),
                (100, 106),
                (116, 122),
                (132, 138),
                (149, 155),
                (165, 171),
            ],
            160 => &[(36, 50), (100, 114), (149, 163)],
            _ => return None,
        };
        return blocks.iter().find_map(|&(start, center)| {
            (primary >= start && primary < start + width / 5 && (primary - start) % 4 == 0)
                .then_some(center)
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::BandPreset;
    use serde_json::json;

    fn row(band: i32, channel: i32, width: i32) -> serde_json::Value {
        json!({"ssid": [84, 101, 115, 116], "bssid": "02:00:00:00:00:01",
               "channel": channel, "band": band, "width_mhz": width,
               "rssi_dbm": -53, "phy": "802.11ax", "ies": []})
    }

    fn report(rows: Vec<serde_json::Value>, preset: BandPreset) -> WifiScanReport {
        let bytes = serde_json::to_vec(&json!({"interfaces": ["en0"], "interface": "en0",
            "authorization": "authorized", "networks": rows}))
        .unwrap();
        parse_scan(&bytes, &BandConfig::from_preset(preset)).unwrap()
    }

    #[test]
    fn channels_are_disambiguated_by_native_band() {
        assert_eq!(channel_frequency(1, 1), Some(2412e6));
        assert_eq!(channel_frequency(3, 1), Some(5955e6));
        assert_eq!(channel_frequency(1, 14), Some(2484e6));
        assert_eq!(channel_frequency(3, 2), Some(5935e6));
        assert_eq!(channel_frequency(2, 184), Some(4920e6));
        assert_eq!(channel_frequency(0, 1), None);
    }

    #[test]
    fn converts_filters_and_deduplicates() {
        let scan = report(
            vec![row(2, 36, 80), row(2, 36, 80), row(1, 1, 20)],
            BandPreset::Ghz5,
        );
        assert_eq!(scan.interfaces, ["en0"]);
        assert_eq!(scan.aps.len(), 1);
        let ap = &scan.aps[0];
        assert_eq!(ap.ssid, "Test");
        assert_eq!(ap.rssi_dbm, -53);
        assert_eq!(ap.primary_freq_hz, 5180e6);
        assert_eq!(ap.center_freq_hz, 5210e6);
        assert_eq!(ap.width_mhz, 80);
    }

    #[test]
    fn redacted_identity_is_reported_not_fabricated() {
        let mut network = row(1, 1, 20);
        network["bssid"] = json!("");
        let scan = report(vec![network], BandPreset::Ghz2);
        assert!(scan.aps.is_empty());
        assert!(scan.status.contains("withheld BSSID"));
        assert!(scan.status.contains("Location Services"));
    }

    #[test]
    fn permission_and_interface_errors_are_visible() {
        let bytes = br#"{"interfaces":["en0"],"authorization":"denied","networks":[],"error":"Wi-Fi is powered off on en0."}"#;
        let scan = parse_scan(bytes, &BandConfig::default()).unwrap();
        assert!(scan.status.contains("powered off"));
        assert!(scan.status.contains("denied"));
        assert_eq!(scan.interfaces, ["en0"]);
        assert!(parse_scan(b"not json", &BandConfig::default()).is_err());
    }

    #[test]
    fn hidden_ssid_is_not_a_redacted_bssid() {
        let mut network = row(1, 6, 40);
        network["ssid"] = json!([]);
        network["ies"] = json!([61, 2, 6, 1]);
        let scan = report(vec![network], BandPreset::Ghz2);
        assert_eq!(scan.aps.len(), 1);
        assert_eq!(scan.aps[0].width_mhz, 40);
        assert_eq!(scan.aps[0].center_freq_hz, 2447e6);
        assert!(!scan.status.contains("withheld"));
    }

    #[test]
    fn vht_segments_distinguish_160_from_80_plus_80() {
        for (ies, expected_width, expected_center) in [
            (vec![192, 3, 1, 42, 50], 160, 5250e6),
            (vec![192, 3, 1, 42, 155], 80, 5210e6),
            (vec![192, 3, 3, 42, 155], 80, 5210e6),
        ] {
            let mut network = row(2, 36, 160);
            network["ies"] = json!(ies);
            let scan = report(vec![network], BandPreset::Ghz5);
            assert_eq!(scan.aps[0].width_mhz, expected_width);
            assert_eq!(scan.aps[0].center_freq_hz, expected_center);
        }
    }

    #[test]
    fn public_safety_channels_do_not_leak_into_5ghz_scan() {
        assert!(
            report(vec![row(2, 184, 20)], BandPreset::Ghz5)
                .aps
                .is_empty()
        );
    }

    #[test]
    fn he_6ghz_segments_and_optional_fields() {
        for (segment1, width, center) in [(15, 160, 6025e6), (39, 80, 5985e6)] {
            for optional in [false, true] {
                let mut body = vec![36, 0, if optional { 0xc0 } else { 0 }, 2, 0, 0, 0];
                if optional {
                    body.extend([0, 0, 0, 0]);
                }
                body.extend([1, 3, 7, segment1, 0]);
                let mut ies = vec![255, body.len() as u8];
                ies.extend(body);
                let mut network = row(3, 1, 160);
                network["ies"] = json!(ies);
                let scan = report(vec![network], BandPreset::Ghz6);
                assert_eq!(scan.aps[0].width_mhz, width);
                assert_eq!(scan.aps[0].center_freq_hz, center);
            }
        }
        assert!(he_6ghz_geometry(&[36, 0, 0, 2, 0, 0, 0, 1], 5955e6).is_none());
    }

    #[test]
    fn wide_channels_and_truncated_ies() {
        assert_eq!(wide_center(3, 1, 80), Some(7));
        assert_eq!(wide_center(3, 33, 160), Some(47));
        assert_eq!(wide_center(3, 233, 160), None);
        assert_eq!(wide_center(2, 149, 80), Some(155));
        assert_eq!(wide_center(2, 36, 160), Some(50));
        let mut network = row(1, 6, 40);
        network["ies"] = json!([61, 10, 6]);
        let scan = report(vec![network], BandPreset::Ghz2);
        assert_eq!(scan.aps[0].width_mhz, 20);
        assert_eq!(scan.aps[0].center_freq_hz, 2437e6);
    }
}
