use hidapi::{HidApi, HidDevice};
use log::{debug, info, warn};

use super::DeviceError;
use super::protocol::*;

pub struct HidBackend {
    device: HidDevice,
}

impl HidBackend {
    /// Open the first matching Corsair Void wireless dongle.
    pub fn open() -> Result<Self, DeviceError> {
        let api = HidApi::new().map_err(|e| DeviceError::Communication(e.to_string()))?;

        for info in api.device_list() {
            if info.vendor_id() == VENDOR_ID
                && WIRELESS_PRODUCT_IDS.contains(&info.product_id())
                && info.usage_page() == USAGE_PAGE
            {
                info!(
                    "Found Corsair Void dongle: PID={:#06x}, usage_page={:#06x}, path={:?}",
                    info.product_id(),
                    info.usage_page(),
                    info.path()
                );
                let device = api.open_path(info.path())?;
                return Ok(Self { device });
            }
        }

        Err(DeviceError::NotFound)
    }

    /// Send a one-shot status request to the dongle.
    pub fn request_status(&self) -> Result<(), DeviceError> {
        self.send_command(STATUS_REQUEST_CMD, "status request")
    }

    /// Ask the dongle to send notifications when status changes.
    pub fn request_notifications(&self) -> Result<(), DeviceError> {
        self.send_command(NOTIF_REQUEST_CMD, "notification request")
    }

    fn send_command(&self, cmd: u8, label: &str) -> Result<(), DeviceError> {
        let mut buf = [0u8; REPORT_SIZE];
        buf[0] = cmd;
        buf[1] = STATUS_REPORT_ID;
        self.device.write(&buf)?;
        debug!("Sent {}", label);
        Ok(())
    }

    /// Read a status report with timeout. Returns None on timeout.
    pub fn read_status(&self, timeout_ms: i32) -> Result<Option<HeadsetStatus>, DeviceError> {
        let mut buf = [0u8; REPORT_SIZE];
        let bytes_read = self.device.read_timeout(&mut buf, timeout_ms)?;

        if bytes_read == 0 {
            return Ok(None);
        }

        debug!("Read {} bytes: {:?}", bytes_read, &buf[..bytes_read]);

        match HeadsetStatus::from_report(&buf[..bytes_read]) {
            Some(status) => {
                debug!("Parsed status: {}", status);
                Ok(Some(status))
            }
            None => {
                warn!("Could not parse report: {:?}", &buf[..bytes_read]);
                Ok(None)
            }
        }
    }
}
