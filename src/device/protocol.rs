use std::fmt;

// Vendor ID (Corsair)
pub const VENDOR_ID: u16 = 0x1b1c;

// Wireless Product IDs supported by the driver
pub const WIRELESS_PRODUCT_IDS: &[u16] = &[
    0x0a0c, 0x0a2b, 0x1b23, // Void Wireless
    0x0a14, 0x0a16, 0x0a1a, // Void Pro Wireless
    0x0a51, 0x0a55, 0x0a75, // Void Elite Wireless
];

// Report IDs
pub const STATUS_REPORT_ID: u8 = 0x64;
#[allow(dead_code)]
pub const FIRMWARE_REPORT_ID: u8 = 0x66;

// Command IDs (sent as first byte of output reports)
pub const STATUS_REQUEST_CMD: u8 = 0xC9;
pub const NOTIF_REQUEST_CMD: u8 = 0xCA;

// Status report byte offsets (0-indexed within the report data after report ID)
pub const STATUS_BYTE_BATTERY_MIC: usize = 2;
pub const STATUS_BYTE_CONNECTION: usize = 3;
pub const STATUS_BYTE_BATTERY_STATUS: usize = 4;

// Bitmasks for the battery/mic byte
pub const MIC_UP_MASK: u8 = 0x80;
pub const BATTERY_CAPACITY_MASK: u8 = 0x7F;

// Connection status raw values
pub const CONN_WIRED: u8 = 16;
pub const CONN_INITIALIZING: u8 = 38;
pub const CONN_LOST: u8 = 49;
pub const CONN_DISCONNECTED_SEARCHING: u8 = 51;
pub const CONN_DISCONNECTED_IDLE: u8 = 52;
pub const CONN_WIRELESS_CONNECTED: u8 = 177;

// Battery status raw values
pub const BAT_DISCONNECTED: u8 = 0;
pub const BAT_NORMAL: u8 = 1;
pub const BAT_LOW: u8 = 2;
pub const BAT_CRITICAL: u8 = 3;
pub const BAT_FULL: u8 = 4;
pub const BAT_CHARGING: u8 = 5;

// Thresholds
pub const LOW_BATTERY_THRESHOLD: u8 = 20;

// HID Usage Page for the vendor-specific interface we need
pub const USAGE_PAGE: u16 = 0xffc5;

// Polling interval in milliseconds
pub const POLL_INTERVAL_MS: u64 = 100;

// Reconnection retry interval in milliseconds
pub const RECONNECT_INTERVAL_MS: u64 = 2000;

// HID report packet size
pub const REPORT_SIZE: usize = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionStatus {
    Wired,
    Initializing,
    LostConnection,
    DisconnectedSearching,
    DisconnectedIdle,
    WirelessConnected,
    Unknown(u8),
}

impl ConnectionStatus {
    pub fn from_byte(b: u8) -> Self {
        match b {
            CONN_WIRED => Self::Wired,
            CONN_INITIALIZING => Self::Initializing,
            CONN_LOST => Self::LostConnection,
            CONN_DISCONNECTED_SEARCHING => Self::DisconnectedSearching,
            CONN_DISCONNECTED_IDLE => Self::DisconnectedIdle,
            CONN_WIRELESS_CONNECTED => Self::WirelessConnected,
            other => Self::Unknown(other),
        }
    }

    pub fn is_connected(&self) -> bool {
        matches!(self, Self::Wired | Self::WirelessConnected)
    }
}

impl fmt::Display for ConnectionStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Wired => write!(f, "Wired"),
            Self::Initializing => write!(f, "Initializing"),
            Self::LostConnection => write!(f, "Lost Connection"),
            Self::DisconnectedSearching => write!(f, "Disconnected (searching)"),
            Self::DisconnectedIdle => write!(f, "Disconnected"),
            Self::WirelessConnected => write!(f, "Connected"),
            Self::Unknown(v) => write!(f, "Unknown ({})", v),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatteryStatus {
    Disconnected,
    Normal,
    Low,
    Critical,
    Full,
    Charging,
    Unknown(u8),
}

impl BatteryStatus {
    pub fn from_byte(b: u8) -> Self {
        match b {
            BAT_DISCONNECTED => Self::Disconnected,
            BAT_NORMAL => Self::Normal,
            BAT_LOW => Self::Low,
            BAT_CRITICAL => Self::Critical,
            BAT_FULL => Self::Full,
            BAT_CHARGING => Self::Charging,
            other => Self::Unknown(other),
        }
    }
}

impl fmt::Display for BatteryStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Disconnected => write!(f, "Disconnected"),
            Self::Normal => write!(f, "Normal"),
            Self::Low => write!(f, "Low"),
            Self::Critical => write!(f, "Critical"),
            Self::Full => write!(f, "Full"),
            Self::Charging => write!(f, "Charging"),
            Self::Unknown(v) => write!(f, "Unknown ({})", v),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct HeadsetStatus {
    pub mic_up: bool,
    pub battery_percent: u8,
    pub battery_status: BatteryStatus,
    pub connection: ConnectionStatus,
}

impl HeadsetStatus {
    /// Parse a status report from raw HID data.
    /// `data` should include the report ID as the first byte.
    /// Returns None if the report ID doesn't match or data is too short.
    pub fn from_report(data: &[u8]) -> Option<Self> {
        if data.len() < 5 {
            return None;
        }

        // Byte 0 is the report ID
        if data[0] != STATUS_REPORT_ID {
            return None;
        }

        let battery_mic_byte = data[STATUS_BYTE_BATTERY_MIC];
        let mic_up = (battery_mic_byte & MIC_UP_MASK) != 0;
        let battery_raw = battery_mic_byte & BATTERY_CAPACITY_MASK;
        let battery_percent = battery_raw.min(100);

        let connection = ConnectionStatus::from_byte(data[STATUS_BYTE_CONNECTION]);
        let battery_status = BatteryStatus::from_byte(data[STATUS_BYTE_BATTERY_STATUS]);

        Some(Self {
            mic_up,
            battery_percent,
            battery_status,
            connection,
        })
    }

    pub fn is_connected(&self) -> bool {
        self.connection.is_connected()
    }
}

impl fmt::Display for HeadsetStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Mic: {} | Battery: {}% ({}) | {}",
            if self.mic_up { "Muted (UP)" } else { "Active (DOWN)" },
            self.battery_percent,
            self.battery_status,
            self.connection,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_status_report_connected() {
        // Report ID 0x64, power=0, battery=78 mic_down, conn=177, bat_status=1
        let data = [0x64, 0x00, 78, 177, 1, 0, 0, 0, 0, 0, 0, 0];
        let status = HeadsetStatus::from_report(&data).unwrap();
        assert!(!status.mic_up);
        assert_eq!(status.battery_percent, 78);
        assert_eq!(status.battery_status, BatteryStatus::Normal);
        assert_eq!(status.connection, ConnectionStatus::WirelessConnected);
        assert!(status.is_connected());
    }

    #[test]
    fn parse_status_report_mic_up() {
        // battery=50 with mic_up bit set: 50 | 0x80 = 0xB2 = 178
        let data = [0x64, 0x00, 0x80 | 50, 177, 1, 0, 0, 0, 0, 0, 0, 0];
        let status = HeadsetStatus::from_report(&data).unwrap();
        assert!(status.mic_up);
        assert_eq!(status.battery_percent, 50);
    }

    #[test]
    fn parse_status_report_charging_clamp() {
        // Charging can report >100, e.g. 127 (max 7 bits). Should clamp to 100.
        let data = [0x64, 0x00, 127, 177, 5, 0, 0, 0, 0, 0, 0, 0];
        let status = HeadsetStatus::from_report(&data).unwrap();
        assert_eq!(status.battery_percent, 100);
        assert_eq!(status.battery_status, BatteryStatus::Charging);
    }

    #[test]
    fn parse_status_report_disconnected() {
        let data = [0x64, 0x00, 0, 52, 0, 0, 0, 0, 0, 0, 0, 0];
        let status = HeadsetStatus::from_report(&data).unwrap();
        assert_eq!(status.connection, ConnectionStatus::DisconnectedIdle);
        assert!(!status.is_connected());
    }

    #[test]
    fn parse_wrong_report_id() {
        let data = [0x66, 0x00, 78, 177, 1, 0, 0, 0, 0, 0, 0, 0];
        assert!(HeadsetStatus::from_report(&data).is_none());
    }

    #[test]
    fn parse_too_short() {
        let data = [0x64, 0x00, 78];
        assert!(HeadsetStatus::from_report(&data).is_none());
    }
}
