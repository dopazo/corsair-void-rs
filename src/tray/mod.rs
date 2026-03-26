use log::{debug, info, warn};
use std::sync::mpsc::Receiver;

use tray_icon::menu::{
    CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu,
};
use tray_icon::{Icon, TrayIconBuilder};

use crate::audio::AudioController;
use crate::config::Config;
use crate::device::protocol::{BatteryStatus, LOW_BATTERY_THRESHOLD};
use crate::device::DeviceEvent;
use crate::ipc::{IpcMessage, IpcResponse, IpcResponder};
use crate::sound::SoundPlayer;

const ICON_SIZE: u32 = 32;

/// Commands arriving from the IPC thread.
pub struct IpcCommand {
    pub message: IpcMessage,
    pub responder: IpcResponder,
}

/// Application state tracked by the tray event loop.
struct AppState {
    last_mic_up: Option<bool>,
    low_battery_alerted: bool,
    battery_percent: u8,
    battery_status: BatteryStatus,
    /// Whether the USB HID dongle is open
    device_open: bool,
    /// Whether the headset reports as wirelessly connected (can be off/out of range)
    headset_connected: bool,
    boost_db: u8,
}

/// Subset of AppState fields that affect the tray UI (icon, tooltip, menu text).
/// Used for change detection to avoid redundant UI updates.
#[derive(PartialEq)]
struct UiSnapshot {
    device_open: bool,
    headset_connected: bool,
    last_mic_up: Option<bool>,
    battery_percent: u8,
    battery_status: BatteryStatus,
}

impl AppState {
    fn ui_snapshot(&self) -> UiSnapshot {
        UiSnapshot {
            device_open: self.device_open,
            headset_connected: self.headset_connected,
            last_mic_up: self.last_mic_up,
            battery_percent: self.battery_percent,
            battery_status: self.battery_status,
        }
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            last_mic_up: None,
            low_battery_alerted: false,
            battery_percent: 0,
            battery_status: BatteryStatus::Disconnected,
            device_open: false,
            headset_connected: false,
            boost_db: 0,
        }
    }
}

struct MenuItems {
    battery_item: MenuItem,
    mic_item: MenuItem,
    boost_items: [CheckMenuItem; 3],
    auto_start_item: CheckMenuItem,
    quit_item: MenuItem,
}

const BOOST_LEVELS: [u8; 3] = [0, 5, 10];

fn build_menu(config: &Config, boost_available: bool) -> (Menu, MenuItems) {
    let title = MenuItem::new("Corsair Void", false, None);
    let battery_item = MenuItem::new("Battery: --", false, None);
    let mic_item = MenuItem::new("Mic: --", false, None);

    let boost_submenu = Submenu::new("Mic Boost", true);
    let boost_items: [CheckMenuItem; 3] = [
        CheckMenuItem::new("0 dB", true, config.general.mic_boost_db == 0, None),
        CheckMenuItem::new("+5 dB", true, config.general.mic_boost_db == 5, None),
        CheckMenuItem::new("+10 dB", true, config.general.mic_boost_db == 10, None),
    ];
    for item in &boost_items {
        let _ = boost_submenu.append(item);
    }
    if !boost_available {
        let _ = boost_submenu.append(&PredefinedMenuItem::separator());
        let _ = boost_submenu.append(&MenuItem::new("(Requires VB-CABLE)", false, None));
    }

    let auto_start_item =
        CheckMenuItem::new("Start on Login", true, config.general.auto_start, None);

    let quit_item = MenuItem::new("Quit", true, None);

    let menu = Menu::new();
    let _ = menu.append_items(&[
        &title,
        &battery_item,
        &mic_item,
        &PredefinedMenuItem::separator(),
        &boost_submenu,
        &auto_start_item,
        &PredefinedMenuItem::separator(),
        &quit_item,
    ]);

    let items = MenuItems {
        battery_item,
        mic_item,
        boost_items,
        auto_start_item,
        quit_item,
    };

    (menu, items)
}

/// Generate a placeholder tray icon with optional overlays.
fn generate_icon(state: &AppState) -> Icon {
    let mut pixels = vec![0u8; (ICON_SIZE * ICON_SIZE * 4) as usize];

    // Base: dark grey circle
    let center = ICON_SIZE as f32 / 2.0;
    let radius = center - 2.0;
    for y in 0..ICON_SIZE {
        for x in 0..ICON_SIZE {
            let dx = x as f32 - center;
            let dy = y as f32 - center;
            let dist = (dx * dx + dy * dy).sqrt();
            let idx = ((y * ICON_SIZE + x) * 4) as usize;

            if dist <= radius {
                let (r, g, b) = if state.device_open && state.headset_connected {
                    (0x00, 0xB4, 0xD8) // Teal: headset connected
                } else if state.device_open {
                    (0xE0, 0xA0, 0x20) // Orange: dongle connected, headset off/out of range
                } else {
                    (0x80, 0x80, 0x80) // Grey: dongle disconnected
                };
                pixels[idx] = r;
                pixels[idx + 1] = g;
                pixels[idx + 2] = b;
                pixels[idx + 3] = 0xFF;
            }
        }
    }

    // Mute overlay: red dot in top-right
    if state.last_mic_up == Some(true) {
        let dot_cx = ICON_SIZE - 6;
        let dot_cy = 6u32;
        let dot_r = 4.0f32;
        let r_ceil = dot_r.ceil() as u32;
        let y_start = dot_cy.saturating_sub(r_ceil);
        let y_end = (dot_cy + r_ceil + 1).min(ICON_SIZE);
        let x_start = dot_cx.saturating_sub(r_ceil);
        let x_end = (dot_cx + r_ceil + 1).min(ICON_SIZE);
        for y in y_start..y_end {
            for x in x_start..x_end {
                let dx = x as f32 - dot_cx as f32;
                let dy = y as f32 - dot_cy as f32;
                if (dx * dx + dy * dy).sqrt() <= dot_r {
                    let idx = ((y * ICON_SIZE + x) * 4) as usize;
                    pixels[idx] = 0xFF;     // R
                    pixels[idx + 1] = 0x20; // G
                    pixels[idx + 2] = 0x20; // B
                    pixels[idx + 3] = 0xFF; // A
                }
            }
        }
    }

    // Battery bar at bottom (only when headset is wirelessly connected)
    if state.device_open && state.headset_connected {
        let bar_height = 4u32;
        let bar_y = ICON_SIZE - bar_height - 2;
        let bar_width = ((state.battery_percent as f32 / 100.0) * (ICON_SIZE - 4) as f32) as u32;
        let (r, g, b) = if state.battery_percent > 50 {
            (0x40u8, 0xC0u8, 0x40u8) // green
        } else if state.battery_percent > LOW_BATTERY_THRESHOLD {
            (0xE0, 0xC0, 0x20) // yellow
        } else {
            (0xE0, 0x30, 0x30) // red
        };
        for y in bar_y..(bar_y + bar_height) {
            for x in 2..(2 + bar_width) {
                let idx = ((y * ICON_SIZE + x) * 4) as usize;
                if idx + 3 < pixels.len() {
                    pixels[idx] = r;
                    pixels[idx + 1] = g;
                    pixels[idx + 2] = b;
                    pixels[idx + 3] = 0xFF;
                }
            }
        }
    }

    Icon::from_rgba(pixels, ICON_SIZE, ICON_SIZE).expect("Failed to create icon")
}

fn update_menu_text(items: &MenuItems, state: &AppState) {
    if state.device_open && state.headset_connected {
        let status_suffix = match state.battery_status {
            BatteryStatus::Charging => " (Charging)",
            BatteryStatus::Low => " (Low)",
            BatteryStatus::Critical => " (Critical!)",
            BatteryStatus::Full => " (Full)",
            _ => "",
        };
        items
            .battery_item
            .set_text(format!("Battery: {}%{}", state.battery_percent, status_suffix));
        items.mic_item.set_text(format!(
            "Mic: {}",
            if state.last_mic_up == Some(true) {
                "Muted"
            } else {
                "Active"
            }
        ));
    } else {
        items.battery_item.set_text(if state.device_open {
            "Waiting for headset..."
        } else {
            "Dongle not connected"
        });
        items.mic_item.set_text("Mic: --");
    }
}

/// Run the tray event loop on the main thread.
pub fn run_tray(
    device_rx: Receiver<DeviceEvent>,
    ipc_rx: Receiver<IpcCommand>,
    mut audio: Box<dyn AudioController>,
    sound_player: Option<SoundPlayer>,
    mut config: Config,
) {
    let (menu, items) = build_menu(&config, audio.boost_available());
    let icon = generate_icon(&AppState::default());

    let tray_icon = TrayIconBuilder::new()
        .with_tooltip("Corsair Void")
        .with_icon(icon)
        .with_menu(Box::new(menu))
        .build()
        .expect("Failed to create tray icon");

    let menu_rx = MenuEvent::receiver();
    let mut state = AppState {
        boost_db: config.general.mic_boost_db,
        ..AppState::default()
    };

    info!("Tray started");

    loop {
        // 1. Pump OS messages (required for tray icon on Windows)
        #[cfg(windows)]
        pump_win32_messages();

        // 2. Process device events — only update state, defer UI update
        let snapshot_before = state.ui_snapshot();
        while let Ok(event) = device_rx.try_recv() {
            match event {
                DeviceEvent::StatusUpdate(status) => {
                    state.headset_connected = status.is_connected();
                    state.last_mic_up = Some(status.mic_up);
                    state.battery_percent = status.battery_percent;
                    state.battery_status = status.battery_status;
                    handle_low_battery(&mut state, &sound_player);
                }
                DeviceEvent::Connected => {
                    state.device_open = true;
                    info!("Device connected event");
                    // Re-find audio device and restart boost if needed
                    match audio.find_device() {
                        Ok(true) => {
                            if state.boost_db > 0 {
                                if let Err(e) = audio.set_boost_db(state.boost_db) {
                                    warn!("Failed to restart boost after reconnect: {}", e);
                                }
                            }
                        }
                        Ok(false) => warn!("Audio device not found after reconnect"),
                        Err(e) => warn!("Audio find_device error after reconnect: {}", e),
                    }
                }
                DeviceEvent::Disconnected => {
                    state.device_open = false;
                    state.headset_connected = false;
                    state.last_mic_up = None;
                    info!("Device disconnected");
                    // Stop boost passthrough (don't reset boost_db so it restarts on reconnect)
                    audio.stop_boost();
                }
            }
        }
        // Update UI once if state changed
        let snapshot_after = state.ui_snapshot();
        if snapshot_before != snapshot_after {
            update_menu_text(&items, &state);
            let new_icon = generate_icon(&state);
            let _ = tray_icon.set_icon(Some(new_icon));
            let tooltip = if state.device_open && state.headset_connected {
                format!("Corsair Void - Battery: {}%", state.battery_percent)
            } else if state.device_open {
                "Corsair Void - Waiting for headset".to_string()
            } else {
                "Corsair Void - Disconnected".to_string()
            };
            let _ = tray_icon.set_tooltip(Some(tooltip));
        }

        // 3. Process IPC commands
        while let Ok(cmd) = ipc_rx.try_recv() {
            let is_mutation = !matches!(cmd.message, IpcMessage::Status);
            let response = handle_ipc_command(&cmd.message, &mut state, &mut *audio, &mut config);
            if is_mutation {
                update_menu_text(&items, &state);
                for (i, item) in items.boost_items.iter().enumerate() {
                    item.set_checked(BOOST_LEVELS[i] == state.boost_db);
                }
            }
            let _ = cmd.responder.send(response);
        }

        // 4. Process menu events
        while let Ok(event) = menu_rx.try_recv() {
            if event.id == items.quit_item.id() {
                info!("Quit requested from tray menu");
                return;
            }

            // Check boost items
            for (i, boost_item) in items.boost_items.iter().enumerate() {
                if event.id == boost_item.id() {
                    let db = BOOST_LEVELS[i];
                    debug!("Boost set to +{} dB", db);
                    if let Err(e) = audio.set_boost_db(db) {
                        warn!("Failed to set boost: {}", e);
                    }
                    state.boost_db = db;
                    config.general.mic_boost_db = db;
                    let _ = config.save();
                    for (j, item) in items.boost_items.iter().enumerate() {
                        item.set_checked(j == i);
                    }
                    break;
                }
            }

            // Auto-start toggle
            if event.id == items.auto_start_item.id() {
                let enabled = items.auto_start_item.is_checked();
                debug!("Auto-start toggled: {}", enabled);
                if let Err(e) = crate::autostart::set_auto_start(enabled) {
                    warn!("Failed to set auto-start: {}", e);
                    // Revert checkbox
                    items.auto_start_item.set_checked(!enabled);
                } else {
                    config.general.auto_start = enabled;
                    let _ = config.save();
                }
            }
        }

        std::thread::sleep(std::time::Duration::from_millis(16));
    }
}

fn handle_low_battery(state: &mut AppState, sound_player: &Option<SoundPlayer>) {
    if state.battery_percent <= LOW_BATTERY_THRESHOLD
        && state.battery_status != BatteryStatus::Charging
        && !state.low_battery_alerted
        && state.device_open
    {
        info!(
            "Low battery alert: {}%",
            state.battery_percent
        );
        if let Some(player) = sound_player {
            player.play_low_battery();
        }
        state.low_battery_alerted = true;
    } else if state.battery_percent > LOW_BATTERY_THRESHOLD
        || state.battery_status == BatteryStatus::Charging
    {
        state.low_battery_alerted = false;
    }
}

fn handle_ipc_command(
    msg: &IpcMessage,
    state: &mut AppState,
    audio: &mut dyn AudioController,
    config: &mut Config,
) -> IpcResponse {
    match msg {
        IpcMessage::Status => IpcResponse::Status {
            mic_up: state.last_mic_up.unwrap_or(false),
            battery_percent: state.battery_percent,
            battery_status: state.battery_status.to_string(),
            boost_db: state.boost_db,
            connected: state.device_open,
        },
        IpcMessage::Boost(db) => {
            let db = *db;
            if let Err(e) = audio.set_boost_db(db) {
                return IpcResponse::Error(format!("Failed to set boost: {}", e));
            }
            state.boost_db = db;
            config.general.mic_boost_db = db;
            let _ = config.save();
            IpcResponse::Ok
        }
        IpcMessage::Stop => {
            info!("Stop requested via IPC");
            std::process::exit(0);
        }
    }
}

#[cfg(windows)]
fn pump_win32_messages() {
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE,
    };
    unsafe {
        let mut msg = MSG::default();
        while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
