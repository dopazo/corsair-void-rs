#[cfg(windows)]
mod boost;
#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(windows)]
pub mod windows;

#[derive(Debug)]
pub enum AudioError {
    DeviceNotFound,
    ApiError(String),
}

impl std::fmt::Display for AudioError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DeviceNotFound => write!(f, "Corsair Void capture device not found"),
            Self::ApiError(msg) => write!(f, "Audio API error: {}", msg),
        }
    }
}

impl std::error::Error for AudioError {}

pub trait AudioController: Send {
    fn find_device(&mut self) -> Result<bool, AudioError>;
    /// Apply a dB boost to the capture device. 0 = normal, 5 = +5 dB, 10 = +10 dB.
    fn set_boost_db(&self, db: u8) -> Result<(), AudioError>;
    /// Get the current dB boost level.
    fn get_boost_db(&self) -> Result<u8, AudioError>;
    /// Whether boost is available on this platform.
    fn boost_available(&self) -> bool;
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

/// Supported microphone boost levels in dB. The CLI enforces these via clap, but
/// IPC messages and the on-disk config are external inputs that must be snapped to
/// match (see [`normalize_boost_db`]) so state, config, and the tray menu agree.
pub const BOOST_LEVELS: [u8; 3] = [0, 5, 10];

/// Snap an arbitrary dB value to the nearest supported [`BOOST_LEVELS`] entry.
pub fn normalize_boost_db(db: u8) -> u8 {
    BOOST_LEVELS
        .into_iter()
        .min_by_key(|&level| level.abs_diff(db))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_passes_through_valid_levels() {
        assert_eq!(normalize_boost_db(0), 0);
        assert_eq!(normalize_boost_db(5), 5);
        assert_eq!(normalize_boost_db(10), 10);
    }

    #[test]
    fn normalize_snaps_to_nearest_level() {
        assert_eq!(normalize_boost_db(2), 0);
        assert_eq!(normalize_boost_db(7), 5);
        assert_eq!(normalize_boost_db(8), 10);
        assert_eq!(normalize_boost_db(200), 10);
    }
}
