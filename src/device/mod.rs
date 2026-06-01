pub mod hid;
pub mod hotplug;
pub mod protocol;
#[cfg(target_os = "linux")]
pub mod sysfs;

use protocol::HeadsetStatus;

#[derive(Debug, Clone)]
pub enum DeviceEvent {
    StatusUpdate(HeadsetStatus),
    Connected,
    Disconnected,
}

/// Abstraction over the ways we can talk to the headset. On Linux this lets the
/// polling loop and CLI use the `hid-corsair-void` kernel driver via sysfs when
/// it owns the HID interface, falling back to direct hidapi otherwise.
pub trait DeviceBackend: Send {
    fn request_status(&self) -> Result<(), DeviceError>;
    fn request_notifications(&self) -> Result<(), DeviceError>;
    /// Block up to `timeout_ms` and return the latest status, or `None` if
    /// nothing new arrived within the timeout.
    fn read_status(&mut self, timeout_ms: i32) -> Result<Option<HeadsetStatus>, DeviceError>;

    /// Whether this backend relies on push-based reports that can silently go
    /// stale (e.g. a HID dongle handle after sleep/hibernate), requiring the
    /// polling loop's request/response health-check handshake. Backends that
    /// read live state on demand (sysfs) never go stale and must opt out —
    /// otherwise an idle-but-healthy device would be declared disconnected,
    /// since their `read_status` legitimately returns `None` for long stretches
    /// and `request_status` elicits no response to reset the timer.
    fn needs_health_check(&self) -> bool {
        true
    }
}

/// Open the best available backend for this platform. On Linux, prefer the
/// sysfs backend when the kernel driver is bound (otherwise hidapi can't open
/// the busy interface); everywhere else, use hidapi directly.
#[cfg(target_os = "linux")]
pub fn open_device_backend() -> Result<Box<dyn DeviceBackend>, DeviceError> {
    if sysfs::sysfs_available() {
        match sysfs::SysfsBackend::open() {
            Ok(backend) => {
                log::info!("Using sysfs (hid-corsair-void) backend");
                return Ok(Box::new(backend));
            }
            Err(e) => log::warn!(
                "sysfs backend available but open failed ({}); falling back to hidapi",
                e
            ),
        }
    }
    Ok(Box::new(hid::HidBackend::open()?))
}

#[cfg(not(target_os = "linux"))]
pub fn open_device_backend() -> Result<Box<dyn DeviceBackend>, DeviceError> {
    Ok(Box::new(hid::HidBackend::open()?))
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
