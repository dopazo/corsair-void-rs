use log::{info, warn};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[derive(Default)]
pub struct Config {
    #[serde(default)]
    pub sound: SoundConfig,
    #[serde(default)]
    pub general: GeneralConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SoundConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_volume")]
    pub volume: f32,
    #[serde(default = "default_freq_high")]
    pub freq_high_hz: u32,
    #[serde(default = "default_freq_low")]
    pub freq_low_hz: u32,
    #[serde(default = "default_duration")]
    pub duration_ms: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[derive(Default)]
pub struct GeneralConfig {
    #[serde(default)]
    pub auto_start: bool,
    /// Microphone dB boost (0, 5, 10)
    #[serde(default)]
    pub mic_boost_db: u8,
}

fn default_true() -> bool {
    true
}
fn default_volume() -> f32 {
    0.5
}
fn default_freq_high() -> u32 {
    1000
}
fn default_freq_low() -> u32 {
    700
}
fn default_duration() -> u32 {
    150
}


impl Default for SoundConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            volume: 0.5,
            freq_high_hz: 1000,
            freq_low_hz: 700,
            duration_ms: 150,
        }
    }
}


impl Config {
    /// Return the config file path.
    pub fn path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("corsair-void")
            .join("config.toml")
    }

    /// Load config from disk. Returns defaults if the file doesn't exist.
    pub fn load() -> Self {
        let path = Self::path();
        if path.exists() {
            match std::fs::read_to_string(&path) {
                Ok(contents) => match toml::from_str(&contents) {
                    Ok(config) => {
                        info!("Loaded config from {}", path.display());
                        return config;
                    }
                    Err(e) => warn!("Failed to parse config: {}. Using defaults.", e),
                },
                Err(e) => warn!("Failed to read config: {}. Using defaults.", e),
            }
        }
        Self::default()
    }

    /// Save config to disk, creating directories if needed.
    pub fn save(&self) -> Result<(), Box<dyn std::error::Error>> {
        let path = Self::path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let contents = toml::to_string_pretty(self)?;
        std::fs::write(&path, contents)?;
        info!("Saved config to {}", path.display());
        Ok(())
    }
}
