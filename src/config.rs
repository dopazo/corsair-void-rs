use log::{info, warn};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[derive(Default)]
pub struct Config {
    #[serde(default)]
    pub general: GeneralConfig,
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
