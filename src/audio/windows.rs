use log::{debug, info, warn};

use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Media::Audio::{
    eCapture, IMMDevice, IMMDeviceCollection, IMMDeviceEnumerator, MMDeviceEnumerator,
    DEVICE_STATE_ACTIVE,
};
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED, STGM_READ,
};
use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;

use super::{AudioController, AudioError};

pub struct WindowsAudioController {
    endpoint_volume: Option<IAudioEndpointVolume>,
}

// SAFETY: IAudioEndpointVolume is COM and we only use it from the thread that
// called CoInitializeEx. We mark Send so it can be stored in Box<dyn AudioController + Send>
// and used from the main thread only.
unsafe impl Send for WindowsAudioController {}

impl WindowsAudioController {
    pub fn new() -> Self {
        Self {
            endpoint_volume: None,
        }
    }

    fn ensure_com() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
    }

    fn find_corsair_device() -> Result<IMMDevice, AudioError> {
        unsafe {
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                    .map_err(|e| AudioError::ApiError(format!("CoCreateInstance: {}", e)))?;

            let collection: IMMDeviceCollection = enumerator
                .EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE)
                .map_err(|e| AudioError::ApiError(format!("EnumAudioEndpoints: {}", e)))?;

            let count = collection
                .GetCount()
                .map_err(|e| AudioError::ApiError(format!("GetCount: {}", e)))?;

            debug!("Found {} active capture devices", count);

            for i in 0..count {
                let device: IMMDevice = collection
                    .Item(i)
                    .map_err(|e| AudioError::ApiError(format!("Item: {}", e)))?;

                let store: IPropertyStore = device
                    .OpenPropertyStore(STGM_READ)
                    .map_err(|e| AudioError::ApiError(format!("OpenPropertyStore: {}", e)))?;

                let name_prop = store
                    .GetValue(&PKEY_Device_FriendlyName)
                    .map_err(|e| AudioError::ApiError(format!("GetValue: {}", e)))?;

                let name = name_prop.to_string();
                debug!("Capture device {}: {}", i, name);

                if name.to_lowercase().contains("corsair") {
                    info!("Found Corsair capture device: {}", name);
                    return Ok(device);
                }
            }

            warn!("No Corsair capture device found among {} devices", count);
            Err(AudioError::DeviceNotFound)
        }
    }

    fn activate_volume(device: &IMMDevice) -> Result<IAudioEndpointVolume, AudioError> {
        unsafe {
            device
                .Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None)
                .map_err(|e| AudioError::ApiError(format!("Activate: {}", e)))
        }
    }

    fn volume(&self) -> Result<&IAudioEndpointVolume, AudioError> {
        self.endpoint_volume.as_ref().ok_or(AudioError::DeviceNotFound)
    }
}

impl AudioController for WindowsAudioController {
    fn find_device(&mut self) -> Result<bool, AudioError> {
        Self::ensure_com();
        match Self::find_corsair_device() {
            Ok(device) => {
                self.endpoint_volume = Some(Self::activate_volume(&device)?);
                Ok(true)
            }
            Err(AudioError::DeviceNotFound) => Ok(false),
            Err(e) => Err(e),
        }
    }

    fn mute(&self) -> Result<(), AudioError> {
        unsafe {
            self.volume()?
                .SetMute(true, std::ptr::null())
                .map_err(|e| AudioError::ApiError(format!("SetMute: {}", e)))
        }
    }

    fn unmute(&self) -> Result<(), AudioError> {
        unsafe {
            self.volume()?
                .SetMute(false, std::ptr::null())
                .map_err(|e| AudioError::ApiError(format!("SetMute: {}", e)))
        }
    }

    fn is_muted(&self) -> Result<bool, AudioError> {
        unsafe {
            let muted = self
                .volume()?
                .GetMute()
                .map_err(|e| AudioError::ApiError(format!("GetMute: {}", e)))?;
            Ok(muted.as_bool())
        }
    }

    fn set_boost_db(&self, _db: u8) -> Result<(), AudioError> {
        // Windows endpoint volume for USB headsets maxes at 0 dB.
        // Real boost requires WASAPI capture + software amplification (TODO).
        Err(AudioError::NotSupported(
            "dB boost on Windows requires WASAPI capture (not yet implemented)".into(),
        ))
    }

    fn get_boost_db(&self) -> Result<u8, AudioError> {
        Err(AudioError::NotSupported(
            "dB boost on Windows requires WASAPI capture (not yet implemented)".into(),
        ))
    }
}
