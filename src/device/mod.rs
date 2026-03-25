pub mod protocol;
pub mod hid;
#[cfg(target_os = "linux")]
pub mod sysfs;

use protocol::HeadsetStatus;

#[derive(Debug, Clone)]
pub enum DeviceEvent {
    StatusUpdate(HeadsetStatus),
    Connected,
    Disconnected,
}

#[derive(Debug)]
pub enum DeviceError {
    NotFound,
    Communication(String),
}

impl std::fmt::Display for DeviceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => write!(f, "Headset not found"),
            Self::Communication(msg) => write!(f, "Communication error: {}", msg),
        }
    }
}

impl std::error::Error for DeviceError {}

impl From<hidapi::HidError> for DeviceError {
    fn from(e: hidapi::HidError) -> Self {
        Self::Communication(e.to_string())
    }
}
