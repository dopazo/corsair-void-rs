use log::{debug, info, warn};

use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Media::Audio::{
    eCapture, IMMDevice, IMMDeviceCollection, IMMDeviceEnumerator, MMDeviceEnumerator,
    DEVICE_STATE_ACTIVE,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED, STGM_READ,
};
use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;

use super::boost::BoostEngine;
use super::{AudioController, AudioError};

pub struct WindowsAudioController {
    boost_engine: BoostEngine,
}

// SAFETY: IAudioEndpointVolume is COM and we only use it from the thread that
// called CoInitializeEx. We mark Send so it can be stored in Box<dyn AudioController + Send>
// and used from the main thread only.
unsafe impl Send for WindowsAudioController {}

impl WindowsAudioController {
    pub fn new() -> Self {
        Self {
            boost_engine: BoostEngine::new(),
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
}

impl AudioController for WindowsAudioController {
    fn find_device(&mut self) -> Result<bool, AudioError> {
        Self::ensure_com();
        match Self::find_corsair_device() {
            Ok(device) => {
                self.boost_engine.set_capture_device(&device);
                self.boost_engine.detect_virtual_cable();
                Ok(true)
            }
            Err(AudioError::DeviceNotFound) => Ok(false),
            Err(e) => Err(e),
        }
    }

    fn set_boost_db(&self, db: u8) -> Result<(), AudioError> {
        self.boost_engine.set_boost_db(db)
    }

    fn get_boost_db(&self) -> Result<u8, AudioError> {
        Ok(self.boost_engine.get_boost_db())
    }

    fn boost_available(&self) -> bool {
        self.boost_engine.virtual_cable_available()
    }

    fn stop_boost(&self) {
        self.boost_engine.stop();
    }
}
