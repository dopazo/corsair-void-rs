use std::path::PathBuf;
use std::time::Duration;

use super::protocol::*;
use super::{DeviceBackend, DeviceError};

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
    /// Last status returned, so the polling loop only sees changes (mirrors the
    /// HID backend, which returns `Some` only when the dongle pushes an update).
    last: Option<HeadsetStatus>,
}

impl SysfsBackend {
    /// Find the sysfs device path for the Corsair Void headset.
    pub fn open() -> Result<Self, DeviceError> {
        let driver_dir = std::fs::read_dir("/sys/bus/hid/drivers/corsair-void")
            .map_err(|e| DeviceError::Communication(e.to_string()))?;

        for entry in driver_dir.filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.join("microphone_up").exists() {
                return Ok(Self {
                    device_path: path,
                    last: None,
                });
            }
        }

        Err(DeviceError::NotFound)
    }

    /// Read the headset's current state from the sysfs attributes.
    fn read_current(&self) -> Result<HeadsetStatus, DeviceError> {
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

        Ok(HeadsetStatus {
            mic_up,
            battery_percent: battery_percent.min(100),
            battery_status,
            connection: ConnectionStatus::WirelessConnected,
        })
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

impl DeviceBackend for SysfsBackend {
    fn request_status(&self) -> Result<(), DeviceError> {
        // sysfs is always up to date, no request needed.
        Ok(())
    }

    fn request_notifications(&self) -> Result<(), DeviceError> {
        // No notification subscription concept for sysfs; reads are always live.
        Ok(())
    }

    fn read_status(&mut self, timeout_ms: i32) -> Result<Option<HeadsetStatus>, DeviceError> {
        // sysfs reads return instantly; sleep to match the HID polling cadence so
        // the polling loop doesn't busy-spin, then report only on change.
        if timeout_ms > 0 {
            std::thread::sleep(Duration::from_millis(timeout_ms as u64));
        }

        let status = self.read_current()?;
        if self.last.as_ref() == Some(&status) {
            Ok(None)
        } else {
            self.last = Some(status.clone());
            Ok(Some(status))
        }
    }

    fn needs_health_check(&self) -> bool {
        // sysfs reads reflect live kernel state and never go stale, so the
        // push-based health-check handshake doesn't apply (and would false-fire
        // on an idle headset, which reports no changes for long stretches).
        false
    }
}
