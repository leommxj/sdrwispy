#[cfg(target_os = "linux")]
use std::{io::ErrorKind, process::Command};

#[cfg(target_os = "windows")]
use std::{ptr, slice};

#[cfg(target_os = "windows")]
use windows_sys::Win32::{
    Foundation::HANDLE,
    NetworkManagement::WiFi::{
        DOT11_PHY_TYPE, DOT11_SSID, WLAN_BSS_ENTRY, WLAN_BSS_LIST, WLAN_INTERFACE_INFO,
        WLAN_INTERFACE_INFO_LIST, WlanCloseHandle, WlanEnumInterfaces, WlanFreeMemory,
        WlanGetNetworkBssList, WlanOpenHandle, WlanScan, dot11_BSS_type_any, dot11_phy_type_dmg,
        dot11_phy_type_dsss, dot11_phy_type_eht, dot11_phy_type_erp, dot11_phy_type_fhss,
        dot11_phy_type_he, dot11_phy_type_hrdsss, dot11_phy_type_ht, dot11_phy_type_ofdm,
        dot11_phy_type_vht,
    },
};

use crate::model::{BandConfig, WifiBss, now_ns};

#[derive(Debug, Clone)]
pub struct WifiScanReport {
    pub aps: Vec<WifiBss>,
    pub interfaces: Vec<String>,
    pub status: String,
}

pub trait WifiScanner {
    fn scan(&mut self, band: &BandConfig, selected_interface: Option<&str>) -> WifiScanReport;
}

#[derive(Default)]
pub struct SystemWifiScanner;

impl WifiScanner for SystemWifiScanner {
    fn scan(&mut self, band: &BandConfig, selected_interface: Option<&str>) -> WifiScanReport {
        let mut report = system_scan(band, selected_interface);
        report.aps.sort_by(|a, b| {
            a.center_freq_hz
                .partial_cmp(&b.center_freq_hz)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.rssi_dbm.cmp(&a.rssi_dbm))
        });
        report
    }
}

#[cfg(target_os = "windows")]
fn system_scan(band: &BandConfig, selected_interface: Option<&str>) -> WifiScanReport {
    match unsafe { native_wlan_scan(band, selected_interface) } {
        Ok(report) => report,
        Err(err) => WifiScanReport {
            aps: Vec::new(),
            interfaces: Vec::new(),
            status: format!("Windows Wi-Fi scan failed: {err}"),
        },
    }
}

#[cfg(target_os = "linux")]
fn system_scan(band: &BandConfig, selected_interface: Option<&str>) -> WifiScanReport {
    let interfaces = match iw_interfaces() {
        Ok(interfaces) => interfaces,
        Err(err) => {
            return WifiScanReport {
                aps: Vec::new(),
                interfaces: Vec::new(),
                status: format!("Linux Wi-Fi scan failed: {err}"),
            };
        }
    };

    if interfaces.is_empty() {
        return WifiScanReport {
            aps: Vec::new(),
            interfaces,
            status: "Linux Wi-Fi scan failed: `iw dev` did not report any wireless interfaces."
                .to_owned(),
        };
    }

    let targets = match selected_scan_targets(&interfaces, selected_interface) {
        Ok(targets) => targets,
        Err(status) => {
            return WifiScanReport {
                aps: Vec::new(),
                interfaces,
                status,
            };
        }
    };

    let mut aps = Vec::new();
    let mut scanned = Vec::new();
    let mut errors = Vec::new();
    for iface in targets {
        match Command::new("iw").args(["dev", &iface, "scan"]).output() {
            Ok(output) if output.status.success() => {
                scanned.push(iface.clone());
                aps.extend(parse_iw_scan(
                    &String::from_utf8_lossy(&output.stdout),
                    band,
                ));
            }
            Ok(output) => {
                errors.push(format!(
                    "`iw dev {iface} scan` exited with {}{}",
                    output.status,
                    command_stderr_suffix(&output.stderr)
                ));
            }
            Err(err) => {
                errors.push(format!(
                    "could not run `iw dev {iface} scan`: {}",
                    command_start_error("iw", &err)
                ));
            }
        }
    }

    let aps = dedup_bss(aps);
    let status = if scanned.is_empty() {
        format!(
            "Linux Wi-Fi scan failed: no interface scan completed. {}",
            join_diagnostics(&errors)
        )
    } else {
        let mut status = scan_complete_status(
            &format!("Linux iw on {}", scanned.join(", ")),
            aps.len(),
            band,
        );
        if !errors.is_empty() {
            status.push_str(" Some interfaces failed: ");
            status.push_str(&join_diagnostics(&errors));
        }
        status
    };

    WifiScanReport {
        aps,
        interfaces,
        status,
    }
}

#[cfg(not(any(target_os = "windows", target_os = "linux")))]
fn system_scan(_band: &BandConfig, _selected_interface: Option<&str>) -> WifiScanReport {
    WifiScanReport {
        aps: Vec::new(),
        interfaces: Vec::new(),
        status: format!(
            "Wi-Fi scan is not implemented for {}.",
            std::env::consts::OS
        ),
    }
}

#[cfg(target_os = "windows")]
unsafe fn native_wlan_scan(
    band: &BandConfig,
    selected_interface: Option<&str>,
) -> Result<WifiScanReport, String> {
    let mut negotiated_version = 0u32;
    let mut handle: HANDLE = ptr::null_mut();
    let ret = unsafe {
        WlanOpenHandle(
            2,
            ptr::null(),
            &mut negotiated_version as *mut u32,
            &mut handle as *mut HANDLE,
        )
    };
    if ret != 0 {
        return Err(windows_wlan_error("WlanOpenHandle", ret));
    }
    let _client = WlanClient { handle };

    let mut iface_list_ptr: *mut WLAN_INTERFACE_INFO_LIST = ptr::null_mut();
    let ret = unsafe {
        WlanEnumInterfaces(
            handle,
            ptr::null(),
            &mut iface_list_ptr as *mut *mut WLAN_INTERFACE_INFO_LIST,
        )
    };
    if ret != 0 {
        return Err(windows_wlan_error("WlanEnumInterfaces", ret));
    }
    let iface_list = WlanMemory(iface_list_ptr.cast());
    if iface_list.0.is_null() {
        return Err("WlanEnumInterfaces returned a null interface list".to_owned());
    }

    let iface_count = unsafe { (*iface_list_ptr).dwNumberOfItems as usize };
    if iface_count == 0 {
        return Err("Native Wi-Fi reported no wireless interfaces".to_owned());
    }
    let iface_entries = unsafe { &(*iface_list_ptr).InterfaceInfo as *const WLAN_INTERFACE_INFO };
    let interfaces = unsafe { slice::from_raw_parts(iface_entries, iface_count) };
    let interface_names: Vec<String> = interfaces.iter().map(wlan_interface_name).collect();
    let targets = match selected_scan_targets(&interface_names, selected_interface) {
        Ok(targets) => targets,
        Err(status) => {
            return Ok(WifiScanReport {
                aps: Vec::new(),
                interfaces: interface_names,
                status,
            });
        }
    };

    let mut out = Vec::new();
    let mut errors = Vec::new();
    for (idx, iface) in interfaces.iter().enumerate() {
        let interface_name = &interface_names[idx];
        if !targets.iter().any(|target| target == interface_name) {
            continue;
        }

        // Asynchronous refresh request. We intentionally do not block the GUI waiting
        // for completion; WlanGetNetworkBssList below returns the current native BSS cache.
        let scan_ret = unsafe {
            WlanScan(
                handle,
                &iface.InterfaceGuid,
                ptr::null(),
                ptr::null(),
                ptr::null(),
            )
        };
        if scan_ret != 0 {
            errors.push(format!(
                "{interface_name} WlanScan returned error code {scan_ret}"
            ));
        }

        let mut bss_list_ptr: *mut WLAN_BSS_LIST = ptr::null_mut();
        let ret = unsafe {
            WlanGetNetworkBssList(
                handle,
                &iface.InterfaceGuid,
                ptr::null(),
                dot11_BSS_type_any,
                0,
                ptr::null(),
                &mut bss_list_ptr as *mut *mut WLAN_BSS_LIST,
            )
        };
        if ret != 0 || bss_list_ptr.is_null() {
            if ret != 0 {
                errors.push(format!(
                    "{interface_name} WlanGetNetworkBssList returned error code {ret}"
                ));
            } else {
                errors.push(format!(
                    "{interface_name} WlanGetNetworkBssList returned a null BSS list"
                ));
            }
            continue;
        }
        let bss_list = WlanMemory(bss_list_ptr.cast());

        let bss_count = unsafe { (*bss_list_ptr).dwNumberOfItems as usize };
        let bss_entries = unsafe { &(*bss_list_ptr).wlanBssEntries as *const WLAN_BSS_ENTRY };
        let entries = unsafe { slice::from_raw_parts(bss_entries, bss_count) };

        for entry in entries {
            if let Some(ap) = wlan_bss_entry_to_wifi_bss(entry, band) {
                out.push(ap);
            }
        }

        drop(bss_list);
    }

    if out.is_empty() && !errors.is_empty() {
        return Err(join_diagnostics(&errors));
    }

    let aps = dedup_bss(out);
    let mut status = scan_complete_status(
        &format!("Windows Native Wi-Fi on {}", targets.join(", ")),
        aps.len(),
        band,
    );
    if !errors.is_empty() {
        status.push_str(" Some interface operations failed: ");
        status.push_str(&join_diagnostics(&errors));
    }

    Ok(WifiScanReport {
        aps,
        interfaces: interface_names,
        status,
    })
}

#[cfg(target_os = "windows")]
struct WlanClient {
    handle: HANDLE,
}

#[cfg(target_os = "windows")]
impl Drop for WlanClient {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe {
                let _ = WlanCloseHandle(self.handle, ptr::null());
            }
        }
    }
}

#[cfg(target_os = "windows")]
struct WlanMemory(*mut core::ffi::c_void);

#[cfg(target_os = "windows")]
impl Drop for WlanMemory {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                WlanFreeMemory(self.0.cast_const());
            }
        }
    }
}

#[cfg(target_os = "windows")]
fn wlan_interface_name(iface: &WLAN_INTERFACE_INFO) -> String {
    let len = iface
        .strInterfaceDescription
        .iter()
        .position(|ch| *ch == 0)
        .unwrap_or(iface.strInterfaceDescription.len());
    String::from_utf16_lossy(&iface.strInterfaceDescription[..len])
        .trim()
        .to_owned()
}

#[cfg(target_os = "windows")]
fn wlan_bss_entry_to_wifi_bss(entry: &WLAN_BSS_ENTRY, band: &BandConfig) -> Option<WifiBss> {
    let primary_freq_hz = entry.ulChCenterFrequency as f64 * 1.0e3;
    if !freq_in_band(primary_freq_hz, band) {
        return None;
    }

    let ies = unsafe { wlan_bss_ies(entry) };
    let (width_mhz, parsed_center_hz) = parse_wifi_operation_from_ies(&ies, band, primary_freq_hz);

    Some(WifiBss {
        ssid: ssid_to_string(&entry.dot11Ssid),
        bssid: format_bssid(entry.dot11Bssid),
        primary_freq_hz,
        center_freq_hz: parsed_center_hz.unwrap_or(primary_freq_hz),
        channel: freq_mhz_to_channel((primary_freq_hz / 1.0e6).round() as i32),
        width_mhz,
        rssi_dbm: entry.lRssi,
        phy: phy_type_label(entry.dot11BssPhyType).to_owned(),
        last_seen_ns: now_ns(),
    })
}

#[cfg(target_os = "linux")]
fn iw_interfaces() -> Result<Vec<String>, String> {
    let output = Command::new("iw").arg("dev").output().map_err(|err| {
        format!(
            "could not run `iw dev`: {}",
            command_start_error("iw", &err)
        )
    })?;
    if !output.status.success() {
        return Err(format!(
            "`iw dev` exited with {}{}",
            output.status,
            command_stderr_suffix(&output.stderr)
        ));
    }

    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            line.strip_prefix("Interface ")
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(ToOwned::to_owned)
        })
        .collect())
}

fn selected_scan_targets(
    interfaces: &[String],
    selected_interface: Option<&str>,
) -> Result<Vec<String>, String> {
    let selected = selected_interface.unwrap_or_default().trim();
    if selected.is_empty() {
        return interfaces
            .first()
            .cloned()
            .map(|iface| vec![iface])
            .ok_or_else(|| "Wi-Fi scan failed: no wireless interfaces are available.".to_owned());
    }

    if interfaces.iter().any(|iface| iface == selected) {
        Ok(vec![selected.to_owned()])
    } else {
        Err(format!(
            "Wi-Fi scan failed: selected interface `{selected}` is not available. Available interfaces: {}.",
            if interfaces.is_empty() {
                "none".to_owned()
            } else {
                interfaces.join(", ")
            }
        ))
    }
}

fn scan_complete_status(source: &str, count: usize, band: &BandConfig) -> String {
    if count == 0 {
        format!(
            "Wi-Fi scan completed with {source}, but found no APs in selected band {}.",
            band_mhz_range(band)
        )
    } else {
        format!(
            "Wi-Fi scan found {count} AP(s) in selected band {} using {source}.",
            band_mhz_range(band)
        )
    }
}

fn band_mhz_range(band: &BandConfig) -> String {
    format!(
        "{:.1}-{:.1} MHz",
        band.start_hz / 1.0e6,
        band.stop_hz / 1.0e6
    )
}

#[cfg(target_os = "windows")]
fn windows_wlan_error(operation: &str, code: u32) -> String {
    format!("{operation} returned error code {code}")
}

fn join_diagnostics(messages: &[String]) -> String {
    if messages.is_empty() {
        return "No diagnostic details were reported.".to_owned();
    }

    let mut out = messages
        .iter()
        .take(3)
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("; ");
    if messages.len() > 3 {
        out.push_str(&format!("; ... {} more", messages.len() - 3));
    }
    out
}

#[cfg(target_os = "linux")]
fn command_start_error(command: &str, err: &std::io::Error) -> String {
    if err.kind() == ErrorKind::NotFound {
        format!(
            "`{command}` was not found in PATH. Install the `iw` package or launch the app with `iw` in PATH."
        )
    } else {
        err.to_string()
    }
}

#[cfg(target_os = "linux")]
fn command_stderr_suffix(stderr: &[u8]) -> String {
    let stderr = compact_command_output(stderr);
    if stderr.is_empty() {
        String::new()
    } else {
        format!(": {stderr}")
    }
}

#[cfg(target_os = "linux")]
fn compact_command_output(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out = String::new();
    for ch in compact.chars().take(240) {
        out.push(ch);
    }
    if compact.chars().count() > 240 {
        out.push_str("...");
    }
    out
}

#[cfg(target_os = "linux")]
#[derive(Default)]
struct IwBss {
    ssid: String,
    bssid: String,
    freq_mhz: Option<i32>,
    center_freq_hz: Option<f64>,
    rssi_dbm: Option<i32>,
    width_mhz: i32,
    phy: String,
}

#[cfg(target_os = "linux")]
fn parse_iw_scan(text: &str, band: &BandConfig) -> Vec<WifiBss> {
    let mut out = Vec::new();
    let mut current: Option<IwBss> = None;
    let mut in_ht = false;
    let mut in_vht = false;
    let mut in_he = false;

    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.starts_with("BSS ") {
            push_iw_bss(&mut out, current.take(), band);
            in_ht = false;
            in_vht = false;
            in_he = false;
            let bssid = line
                .strip_prefix("BSS ")
                .unwrap_or_default()
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .split('(')
                .next()
                .unwrap_or_default()
                .to_owned();
            current = Some(IwBss {
                bssid,
                width_mhz: 20,
                phy: "802.11".to_owned(),
                ..Default::default()
            });
            continue;
        }

        let Some(bss) = current.as_mut() else {
            continue;
        };

        if let Some(rest) = line.strip_prefix("freq:") {
            bss.freq_mhz = first_i32(rest);
        } else if let Some(rest) = line.strip_prefix("signal:") {
            bss.rssi_dbm = first_f32(rest).map(|v| v.round() as i32);
        } else if let Some(rest) = line.strip_prefix("SSID:") {
            bss.ssid = decode_iw_ssid(rest);
        } else if line.starts_with("HT operation:") {
            in_ht = true;
            in_vht = false;
            in_he = false;
            bss.phy = "802.11n".to_owned();
        } else if line.starts_with("VHT operation:") {
            in_vht = true;
            in_ht = false;
            in_he = false;
            bss.phy = "802.11ac/ax".to_owned();
        } else if line.starts_with("HE operation:") || line.starts_with("HE 6 GHz operation:") {
            in_he = true;
            in_ht = false;
            in_vht = false;
            bss.phy = "802.11ax".to_owned();
        } else if in_ht && line.contains("secondary channel offset:") {
            if line.contains("above") {
                bss.width_mhz = bss.width_mhz.max(40);
                if let Some(freq_mhz) = bss.freq_mhz {
                    bss.center_freq_hz = Some((freq_mhz as f64 + 10.0) * 1.0e6);
                }
            } else if line.contains("below") {
                bss.width_mhz = bss.width_mhz.max(40);
                if let Some(freq_mhz) = bss.freq_mhz {
                    bss.center_freq_hz = Some((freq_mhz as f64 - 10.0) * 1.0e6);
                }
            }
        } else if (in_vht || in_he) && line.contains("channel width:") {
            if let Some(width_mhz) = parse_iw_channel_width_mhz(line, in_he) {
                bss.width_mhz = bss.width_mhz.max(width_mhz);
            }
        } else if (in_vht || in_he)
            && line.contains("center freq segment 1:")
            && let Some(center_channel) = first_i32(line)
        {
            bss.center_freq_hz =
                channel_to_freq_hz_for_band(center_channel, band).or(bss.center_freq_hz);
        }
    }

    push_iw_bss(&mut out, current, band);
    dedup_bss(out)
}

#[cfg(target_os = "linux")]
fn push_iw_bss(out: &mut Vec<WifiBss>, current: Option<IwBss>, band: &BandConfig) {
    let Some(bss) = current else {
        return;
    };
    let Some(freq_mhz) = bss.freq_mhz else {
        return;
    };
    let freq_hz = freq_mhz as f64 * 1.0e6;
    if !freq_in_band(freq_hz, band) {
        return;
    }

    out.push(WifiBss {
        ssid: clean_ssid(&bss.ssid),
        bssid: bss.bssid,
        primary_freq_hz: freq_hz,
        center_freq_hz: bss.center_freq_hz.unwrap_or(freq_hz),
        channel: freq_mhz_to_channel(freq_mhz),
        width_mhz: bss.width_mhz,
        rssi_dbm: bss.rssi_dbm.unwrap_or(-100),
        phy: bss.phy,
        last_seen_ns: now_ns(),
    });
}

fn freq_in_band(freq_hz: f64, band: &BandConfig) -> bool {
    freq_hz >= band.start_hz && freq_hz <= band.stop_hz
}

#[cfg(target_os = "windows")]
unsafe fn wlan_bss_ies(entry: &WLAN_BSS_ENTRY) -> Vec<u8> {
    if entry.ulIeOffset == 0 || entry.ulIeSize == 0 || entry.ulIeSize > 4096 {
        return Vec::new();
    }

    let base = entry as *const WLAN_BSS_ENTRY as *const u8;
    let ptr = unsafe { base.add(entry.ulIeOffset as usize) };
    unsafe { slice::from_raw_parts(ptr, entry.ulIeSize as usize) }.to_vec()
}

#[cfg(target_os = "windows")]
fn ssid_to_string(ssid: &DOT11_SSID) -> String {
    let len = (ssid.uSSIDLength as usize).min(ssid.ucSSID.len());
    decode_ssid_bytes(&ssid.ucSSID[..len])
}

#[cfg(target_os = "windows")]
fn format_bssid(bssid: [u8; 6]) -> String {
    format!(
        "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        bssid[0], bssid[1], bssid[2], bssid[3], bssid[4], bssid[5]
    )
}

#[cfg(target_os = "windows")]
fn phy_type_label(phy: DOT11_PHY_TYPE) -> &'static str {
    if phy == dot11_phy_type_fhss {
        "802.11 FHSS"
    } else if phy == dot11_phy_type_dsss {
        "802.11 DSSS"
    } else if phy == dot11_phy_type_ofdm {
        "802.11a/g"
    } else if phy == dot11_phy_type_hrdsss {
        "802.11b"
    } else if phy == dot11_phy_type_erp {
        "802.11g"
    } else if phy == dot11_phy_type_ht {
        "802.11n"
    } else if phy == dot11_phy_type_vht {
        "802.11ac"
    } else if phy == dot11_phy_type_dmg {
        "802.11ad"
    } else if phy == dot11_phy_type_he {
        "802.11ax"
    } else if phy == dot11_phy_type_eht {
        "802.11be"
    } else {
        "802.11"
    }
}

#[cfg(target_os = "windows")]
fn parse_wifi_operation_from_ies(
    ies: &[u8],
    band: &BandConfig,
    primary_freq_hz: f64,
) -> (i32, Option<f64>) {
    let mut width_mhz = 20;
    let mut center_hz = None;
    let mut ht_center_hz = None;
    let mut idx = 0usize;

    while idx + 2 <= ies.len() {
        let id = ies[idx];
        let len = ies[idx + 1] as usize;
        idx += 2;
        if idx + len > ies.len() {
            break;
        }
        let body = &ies[idx..idx + len];

        match id {
            // HT Operation
            61 if body.len() >= 2 => {
                let secondary_offset = body[1] & 0b11;
                match secondary_offset {
                    // Secondary channel above primary: 40 MHz center is primary + 10 MHz.
                    1 => {
                        width_mhz = width_mhz.max(40);
                        ht_center_hz = Some(primary_freq_hz + 10.0e6);
                    }
                    // Secondary channel below primary: 40 MHz center is primary - 10 MHz.
                    3 => {
                        width_mhz = width_mhz.max(40);
                        ht_center_hz = Some(primary_freq_hz - 10.0e6);
                    }
                    _ => {}
                }
            }
            // VHT Operation
            192 if body.len() >= 3 => {
                match body[0] {
                    1 => width_mhz = width_mhz.max(80),
                    2 | 3 => width_mhz = width_mhz.max(160),
                    _ => {}
                }
                if body[1] != 0 {
                    center_hz = channel_to_freq_hz_for_band(body[1] as i32, band).or(center_hz);
                }
            }
            // Extension IE. HE/EHT operation details are variable; keep PHY/RSSI/freq exact and
            // only use conservative width unless HT/VHT operation above proves a wider channel.
            255 => {}
            _ => {}
        }

        idx += len;
    }

    (width_mhz, center_hz.or(ht_center_hz))
}

fn channel_to_freq_hz_for_band(channel: i32, band: &BandConfig) -> Option<f64> {
    channel_candidate_freqs_hz(channel)
        .into_iter()
        .flatten()
        .find(|freq| freq_in_band(*freq, band))
}

fn channel_candidate_freqs_hz(channel: i32) -> [Option<f64>; 3] {
    let ghz2 = if (1..=13).contains(&channel) {
        Some((2407 + channel * 5) as f64 * 1.0e6)
    } else if channel == 14 {
        Some(2484.0e6)
    } else {
        None
    };

    let ghz5 = if (1..=200).contains(&channel) {
        Some((5000 + channel * 5) as f64 * 1.0e6)
    } else {
        None
    };

    let ghz6 = if (1..=233).contains(&channel) {
        Some((5950 + channel * 5) as f64 * 1.0e6)
    } else {
        None
    };

    [ghz2, ghz5, ghz6]
}

fn freq_mhz_to_channel(freq_mhz: i32) -> i32 {
    if freq_mhz == 2484 {
        14
    } else if (2412..=2472).contains(&freq_mhz) {
        (freq_mhz - 2407) / 5
    } else if (5955..=7115).contains(&freq_mhz) {
        (freq_mhz - 5950) / 5
    } else {
        (freq_mhz - 5000) / 5
    }
}

fn decode_ssid_bytes(bytes: &[u8]) -> String {
    let bytes = trim_trailing_nul(bytes);
    if bytes.is_empty() {
        return String::new();
    }

    if let Ok(value) = std::str::from_utf8(bytes) {
        return clean_ssid(value);
    }

    for encoding in [encoding_rs::GBK, encoding_rs::BIG5] {
        let (value, _, had_errors) = encoding.decode(bytes);
        let value = clean_ssid(&value);
        if !had_errors && !value.is_empty() {
            return value;
        }
    }

    clean_ssid(&String::from_utf8_lossy(bytes))
}

fn trim_trailing_nul(mut bytes: &[u8]) -> &[u8] {
    while bytes.last() == Some(&0) {
        bytes = &bytes[..bytes.len() - 1];
    }
    bytes
}

#[cfg(target_os = "linux")]
fn decode_iw_ssid(value: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        return String::new();
    }

    let bytes = unescape_iw_ssid_bytes(value);
    decode_ssid_bytes(&bytes)
}

#[cfg(target_os = "linux")]
fn unescape_iw_ssid_bytes(value: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(value.len());
    let mut chars = value.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\\' && chars.peek() == Some(&'x') {
            let _ = chars.next();
            let hi = chars.next().and_then(|c| c.to_digit(16));
            let lo = chars.next().and_then(|c| c.to_digit(16));
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push(((hi << 4) | lo) as u8);
                continue;
            }
            out.push(b'\\');
            out.push(b'x');
            if let Some(hi) = hi {
                out.push(hex_digit_to_ascii(hi as u8));
            }
            if let Some(lo) = lo {
                out.push(hex_digit_to_ascii(lo as u8));
            }
            continue;
        }

        let mut buf = [0u8; 4];
        out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
    }

    out
}

#[cfg(target_os = "linux")]
fn hex_digit_to_ascii(value: u8) -> u8 {
    match value {
        0..=9 => b'0' + value,
        10..=15 => b'a' + (value - 10),
        _ => b'?',
    }
}

fn clean_ssid(value: &str) -> String {
    value.trim_matches('\0').trim().to_owned()
}

#[cfg(target_os = "linux")]
fn parse_iw_channel_width_mhz(line: &str, he_or_6ghz: bool) -> Option<i32> {
    let lower = line.to_ascii_lowercase();
    if lower.contains("160 mhz") {
        return Some(160);
    }
    if lower.contains("80 mhz") {
        return Some(80);
    }
    if lower.contains("40 mhz") {
        return Some(40);
    }
    if lower.contains("20 mhz") {
        return Some(20);
    }

    let code = first_i32(line)?;
    if he_or_6ghz {
        match code {
            0 => Some(20),
            1 => Some(40),
            2 => Some(80),
            3 => Some(160),
            _ => None,
        }
    } else {
        match code {
            0 => Some(40),
            1 => Some(80),
            2 | 3 => Some(160),
            _ => None,
        }
    }
}

#[cfg(target_os = "linux")]
fn first_i32(text: &str) -> Option<i32> {
    first_number_str(text).and_then(|s| s.parse::<i32>().ok())
}

#[cfg(target_os = "linux")]
fn first_f32(text: &str) -> Option<f32> {
    first_number_str(text).and_then(|s| s.parse::<f32>().ok())
}

#[cfg(target_os = "linux")]
fn first_number_str(text: &str) -> Option<&str> {
    let mut start = None;
    let mut end = 0usize;
    for (idx, ch) in text.char_indices() {
        if start.is_none() {
            if ch.is_ascii_digit() || ch == '-' || ch == '+' {
                start = Some(idx);
                end = idx + ch.len_utf8();
            }
        } else if ch.is_ascii_digit() || ch == '.' {
            end = idx + ch.len_utf8();
        } else {
            break;
        }
    }
    start.map(|s| &text[s..end])
}

fn dedup_bss(mut aps: Vec<WifiBss>) -> Vec<WifiBss> {
    aps.sort_by(|a, b| {
        a.bssid
            .cmp(&b.bssid)
            .then_with(|| b.rssi_dbm.cmp(&a.rssi_dbm))
    });
    aps.dedup_by(|a, b| a.bssid.eq_ignore_ascii_case(&b.bssid));
    aps
}
