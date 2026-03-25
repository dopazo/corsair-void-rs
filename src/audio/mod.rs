#[cfg(windows)]
pub mod windows;
#[cfg(windows)]
mod boost;
#[cfg(target_os = "linux")]
pub mod linux;

#[derive(Debug)]
pub enum AudioError {
    DeviceNotFound,
    ApiError(String),
    #[allow(dead_code)]
    NotSupported(String),
}

impl std::fmt::Display for AudioError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DeviceNotFound => write!(f, "Corsair Void capture device not found"),
            Self::ApiError(msg) => write!(f, "Audio API error: {}", msg),
            Self::NotSupported(msg) => write!(f, "Not supported: {}", msg),
        }
    }
}

impl std::error::Error for AudioError {}

pub trait AudioController: Send {
    fn find_device(&mut self) -> Result<bool, AudioError>;
    fn mute(&self) -> Result<(), AudioError>;
    fn unmute(&self) -> Result<(), AudioError>;
    fn is_muted(&self) -> Result<bool, AudioError>;
    /// Apply a dB boost to the capture device. 0 = normal, 5 = +5 dB, 10 = +10 dB.
    fn set_boost_db(&self, db: u8) -> Result<(), AudioError>;
    /// Get the current dB boost level.
    fn get_boost_db(&self) -> Result<u8, AudioError>;
    /// Whether a virtual audio cable is available for boost passthrough.
    fn virtual_cable_available(&self) -> bool {
        false
    }
    /// Stop the boost passthrough thread (on disconnect). Does not reset boost_db.
    fn stop_boost(&self) {}

}

pub fn create_audio_controller() -> Box<dyn AudioController> {
    #[cfg(windows)]
    {
        Box::new(windows::WindowsAudioController::new())
    }
    #[cfg(target_os = "linux")]
    {
        Box::new(linux::LinuxAudioController::new())
    }
}
