use crate::model::SdrDeviceInfo;

pub struct SdrDeviceCatalog;

impl SdrDeviceCatalog {
    pub fn list_devices(&self) -> Vec<SdrDeviceInfo> {
        #[cfg(feature = "soapy")]
        {
            let devices = crate::soapy::enumerate_devices();
            let hidden_audio_count = devices.iter().filter(|d| d.driver == "audio").count();
            let filtered: Vec<_> = devices
                .iter()
                .filter(|d| d.driver != "audio")
                .cloned()
                .collect();

            if !filtered.is_empty() {
                filtered
            } else if hidden_audio_count > 0 {
                vec![SdrDeviceInfo {
                    id: "no-rf-sdr".to_owned(),
                    label: format!(
                        "No RF SDR devices found (hidden {hidden_audio_count} Soapy audio device(s))"
                    ),
                    driver: "soapy-info".to_owned(),
                }]
            } else {
                devices
            }
        }

        #[cfg(not(feature = "soapy"))]
        {
            vec![SdrDeviceInfo {
                id: "soapy-disabled".to_owned(),
                label: "Build with --features soapy for SDR devices".to_owned(),
                driver: "soapy-info".to_owned(),
            }]
        }
    }
}
