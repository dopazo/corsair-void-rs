use std::path::PathBuf;

use super::DeviceError;
use super::protocol::*;

/// Check if the hid-corsair-void kernel driver exposes sysfs attributes.
pub fn sysfs_available() -> bool {
    std::fs::read_dir("/sys/bus/hid/drivers/corsair-void")
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .any(|e| e.path().join("microphone_up").exists())
        })
        .unwrap_or(false)
}

pub struct SysfsBackend {
    device_path: PathBuf,
}

impl SysfsBackend {
    /// Find the sysfs device path for the Corsair Void headset.
    pub fn open() -> Result<Self, DeviceError> {
        let driver_dir = std::fs::read_dir("/sys/bus/hid/drivers/corsair-void")
            .map_err(|e| DeviceError::Communication(e.to_string()))?;

        for entry in driver_dir.filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.join("microphone_up").exists() {
                return Ok(Self { device_path: path });
            }
        }

        Err(DeviceError::NotFound)
    }

    pub fn request_status(&self) -> Result<(), DeviceError> {
        // sysfs is always up to date, no request needed
        Ok(())
    }

    pub fn read_status(&self) -> Result<Option<HeadsetStatus>, DeviceError> {
        let mic_up = self.read_attr_bool("microphone_up")?;
        let battery_percent = self.read_attr_u8("battery_capacity")?;
        let charging = self.read_attr_bool("battery_charging")?;

        let battery_status = if charging {
            BatteryStatus::Charging
        } else if battery_percent <= LOW_BATTERY_THRESHOLD {
            BatteryStatus::Low
        } else {
            BatteryStatus::Normal
        };

        Ok(Some(HeadsetStatus {
            mic_up,
            battery_percent: battery_percent.min(100),
            battery_status,
            connection: ConnectionStatus::WirelessConnected,
        }))
    }

    fn read_attr_bool(&self, name: &str) -> Result<bool, DeviceError> {
        let val = self.read_attr_string(name)?;
        Ok(val.trim() == "1")
    }

    fn read_attr_u8(&self, name: &str) -> Result<u8, DeviceError> {
        let val = self.read_attr_string(name)?;
        val.trim()
            .parse()
            .map_err(|e: std::num::ParseIntError| DeviceError::Communication(e.to_string()))
    }

    fn read_attr_string(&self, name: &str) -> Result<String, DeviceError> {
        std::fs::read_to_string(self.device_path.join(name))
            .map_err(|e| DeviceError::Communication(e.to_string()))
    }
}
