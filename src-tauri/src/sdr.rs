use crate::model::SdrDeviceInfo;

pub struct SdrDeviceCatalog;

impl SdrDeviceCatalog {
    pub fn list_devices(&self) -> Vec<SdrDeviceInfo> {
        #[cfg(feature = "soapy")]
        {
            eprintln!("[sdrwispy] SoapySDR support enabled; calling SoapySDRDevice_enumerate");
            let devices = crate::soapy::enumerate_devices();
            eprintln!(
                "[sdrwispy] SoapySDR raw enumeration returned {} device(s)",
                devices.len()
            );
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
            eprintln!("[sdrwispy] SoapySDR support disabled at compile time");
            vec![SdrDeviceInfo {
                id: "soapy-disabled".to_owned(),
                label: "SoapySDR support is disabled in this build".to_owned(),
                driver: "soapy-info".to_owned(),
            }]
        }
    }
}
