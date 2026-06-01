// Hide console window when launched from Start Menu / autostart.
// CLI subcommands still work when run from an existing terminal.
#![windows_subsystem = "windows"]

mod audio;
pub mod autostart;
mod config;
mod device;
mod ipc;
mod sound;
mod tray;

use clap::{Parser, Subcommand};
use log::{error, info, warn};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use config::Config;
use device::protocol::*;
use device::DeviceEvent;
use ipc::{IpcClient, IpcMessage, IpcResponse, IpcServer};
use sound::SoundPlayer;
use tray::IpcCommand;

#[derive(Parser)]
#[command(name = "corsair-void", about = "Corsair Void headset controller")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Show headset status
    Status,
    /// Set microphone dB boost (0, 5, or 10)
    Boost {
        #[arg(value_parser = parse_boost_db)]
        db: u8,
    },
    /// Stop the running instance
    Stop,
}

fn parse_boost_db(s: &str) -> Result<u8, String> {
    let val: u8 = s
        .parse()
        .map_err(|_| format!("'{}' is not a valid number", s))?;
    match val {
        0 | 5 | 10 => Ok(val),
        _ => Err("boost must be 0, 5, or 10 dB".to_string()),
    }
}

fn main() {
    env_logger::init();
    let cli = Cli::parse();

    match cli.command {
        None => run_tray_mode(),
        Some(cmd) => run_cli(cmd),
    }
}

fn run_tray_mode() {
    info!("Starting Corsair Void in tray mode");

    let config = Config::load();

    // Initialize audio controller
    let mut audio_ctrl = audio::create_audio_controller();
    match audio_ctrl.find_device() {
        Ok(true) => {
            info!("Corsair audio capture device found");
            // Apply saved boost level
            if config.general.mic_boost_db > 0 {
                if let Err(e) = audio_ctrl.set_boost_db(config.general.mic_boost_db) {
                    warn!("Failed to apply saved boost: {}", e);
                }
            }
        }
        Ok(false) => warn!("Corsair audio capture device not found — mute/boost won't work until headset is detected"),
        Err(e) => warn!("Audio init error: {}", e),
    }

    // Initialize sound player
    let sound_player = SoundPlayer::new(&config.sound);

    // Channels
    let (device_tx, device_rx) = mpsc::channel::<DeviceEvent>();
    let (ipc_tx, ipc_rx) = mpsc::channel::<IpcCommand>();
    let (hotplug_tx, hotplug_rx) = mpsc::channel::<()>();

    // OS hotplug watcher (Windows: WM_DEVICECHANGE; no-op elsewhere)
    device::hotplug::spawn(hotplug_tx);

    // Spawn HID polling thread
    thread::spawn(move || {
        hid_polling_loop(device_tx, hotplug_rx);
    });

    // Spawn IPC server thread
    thread::spawn(move || {
        ipc_server_loop(ipc_tx);
    });

    // Run tray on main thread (blocks)
    tray::run_tray(device_rx, ipc_rx, audio_ctrl, sound_player, config);
}

const MAX_CONSECUTIVE_ERRORS: u32 = 10;
const NOTIF_REFRESH_INTERVAL_MS: u64 = 5000;
const NO_DATA_TIMEOUT_MS: u64 = 30_000;
const HEALTH_CHECK_TIMEOUT_MS: u64 = 5_000;
/// Fallback wait while the dongle is missing. Normally we are woken up sooner
/// by an OS hotplug event; this is just a safety net in case the event misses.
const HOTPLUG_FALLBACK_TIMEOUT_MS: u64 = 5_000;

fn hid_polling_loop(tx: mpsc::Sender<DeviceEvent>, hotplug_rx: mpsc::Receiver<()>) {
    loop {
        // Drain any stale hotplug events buffered while we were busy reading the
        // device; we only care about events that arrive while we're waiting.
        while hotplug_rx.try_recv().is_ok() {}

        match device::open_device_backend() {
            Ok(mut device) => {
                info!("HID device opened");
                let _ = tx.send(DeviceEvent::Connected);

                // Get initial status, then switch to notification mode
                if let Err(e) = device.request_status() {
                    warn!("Failed initial status request: {}", e);
                }
                if let Err(e) = device.request_notifications() {
                    warn!("Failed initial notification request: {}", e);
                }

                let mut consecutive_errors = 0u32;
                let mut last_notif_request = std::time::Instant::now();
                let mut last_data_received = std::time::Instant::now();
                let mut health_check_at: Option<std::time::Instant> = None;

                loop {
                    match device.read_status(POLL_INTERVAL_MS as i32) {
                        Ok(Some(status)) => {
                            consecutive_errors = 0;
                            last_data_received = std::time::Instant::now();
                            health_check_at = None;
                            let _ = tx.send(DeviceEvent::StatusUpdate(status));
                        }
                        Ok(None) => {
                            // Timeout — no change reported. Don't reset consecutive_errors:
                            // a stale handle after sleep/hibernation returns timeouts instead
                            // of errors, which would mask write failures forever.
                        }
                        Err(e) => {
                            warn!("HID read error: {}", e);
                            consecutive_errors += 1;
                        }
                    }

                    // Periodically re-send notification request to keep the dongle reporting.
                    // Always advance the timestamp so a failing write doesn't busy-loop;
                    // consecutive_errors will trip the disconnect threshold if it keeps failing.
                    if last_notif_request.elapsed()
                        >= Duration::from_millis(NOTIF_REFRESH_INTERVAL_MS)
                    {
                        last_notif_request = std::time::Instant::now();
                        if let Err(e) = device.request_notifications() {
                            warn!("Notification request failed: {}", e);
                            consecutive_errors += 1;
                        }
                    }

                    // Health check: detect stale handles after sleep/hibernation.
                    // If no data for a while, send a status request — the dongle should
                    // always respond. If it doesn't within 5s, the handle is dead.
                    // Only push-based backends (HID) need this; on-demand backends
                    // (sysfs) never go stale and would false-trigger it while idle.
                    if device.needs_health_check()
                        && health_check_at.is_none()
                        && last_data_received.elapsed() >= Duration::from_millis(NO_DATA_TIMEOUT_MS)
                    {
                        info!(
                            "No HID data for {}s, sending health check",
                            NO_DATA_TIMEOUT_MS / 1000
                        );
                        if let Err(e) = device.request_status() {
                            warn!("Health check write failed: {}", e);
                            break;
                        }
                        health_check_at = Some(std::time::Instant::now());
                    }
                    if let Some(hc) = health_check_at {
                        if hc.elapsed() >= Duration::from_millis(HEALTH_CHECK_TIMEOUT_MS) {
                            warn!("No response to health check — device handle is stale (sleep/hibernate?)");
                            break;
                        }
                    }

                    if consecutive_errors >= MAX_CONSECUTIVE_ERRORS {
                        warn!(
                            "Too many consecutive HID errors ({}), considering device disconnected",
                            consecutive_errors
                        );
                        break;
                    }

                    if consecutive_errors > 0 {
                        thread::sleep(Duration::from_millis(500));
                    }
                }

                let _ = tx.send(DeviceEvent::Disconnected);
            }
            Err(_) => {
                // Device not found; fall through to the hotplug wait below.
            }
        }

        // Wait for either an OS hotplug event or the fallback timeout.
        // recv_timeout returns Ok(()) on a hotplug signal, Err(Timeout) on the
        // fallback, or Err(Disconnected) if the watcher channel was closed —
        // in any case we just loop and try opening again.
        let _ = hotplug_rx.recv_timeout(Duration::from_millis(HOTPLUG_FALLBACK_TIMEOUT_MS));
    }
}

const IPC_RESPONSE_TIMEOUT_MS: u64 = 2000;

fn ipc_server_loop(tx: mpsc::Sender<IpcCommand>) {
    let server = match IpcServer::bind() {
        Ok(s) => s,
        Err(e) => {
            error!("Failed to start IPC server: {}", e);
            return;
        }
    };

    loop {
        match server.accept() {
            Ok((message, responder)) => {
                info!("IPC command received: {:?}", message);
                let (done_tx, done_rx) = mpsc::sync_channel::<()>(1);
                let _ = tx.send(IpcCommand {
                    message,
                    responder,
                    done: done_tx,
                });
                // Wait until the main thread signals it has written the response,
                // or bail out after a generous timeout if it never does.
                let _ = done_rx.recv_timeout(Duration::from_millis(IPC_RESPONSE_TIMEOUT_MS));
                server.disconnect_client();
            }
            Err(e) => {
                warn!("IPC accept error: {}", e);
                server.disconnect_client();
            }
        }
    }
}

fn run_cli(command: Command) {
    // Try IPC first (tray instance may be running)
    if IpcClient::is_running() {
        let msg = match &command {
            Command::Status => IpcMessage::Status,
            Command::Boost { db } => IpcMessage::Boost(*db),
            Command::Stop => IpcMessage::Stop,
        };

        match IpcClient::send(msg) {
            Ok(response) => {
                print_response(&command, &response);
                return;
            }
            Err(e) => {
                warn!("IPC failed: {}. Falling back to direct HID.", e);
            }
        }
    }

    // No tray running — direct mode
    match command {
        Command::Status => {
            match device::open_device_backend() {
                Ok(mut device) => {
                    if let Err(e) = device.request_status() {
                        error!("Failed to request status: {}", e);
                        std::process::exit(1);
                    }
                    match device.read_status(1000) {
                        Ok(Some(status)) => {
                            println!(
                                "Mic: {}",
                                if status.mic_up {
                                    "Muted (UP)"
                                } else {
                                    "Active (DOWN)"
                                }
                            );
                            println!(
                                "Battery: {}% ({})",
                                status.battery_percent, status.battery_status
                            );
                            println!("Connection: {}", status.connection);
                            // Try to get boost from audio controller
                            let mut audio = audio::create_audio_controller();
                            if audio.find_device().unwrap_or(false) {
                                match audio.get_boost_db() {
                                    Ok(db) => println!("Boost: +{} dB", db),
                                    Err(_) => println!("Boost: N/A"),
                                }
                            }
                        }
                        Ok(None) => {
                            error!("No response from headset");
                            std::process::exit(1);
                        }
                        Err(e) => {
                            error!("Failed to read status: {}", e);
                            std::process::exit(1);
                        }
                    }
                }
                Err(e) => {
                    error!("{}", e);
                    std::process::exit(1);
                }
            }
        }
        Command::Boost { db } => {
            let mut audio = audio::create_audio_controller();
            match audio.find_device() {
                Ok(true) => {
                    if let Err(e) = audio.set_boost_db(db) {
                        error!("Failed to set boost: {}", e);
                        std::process::exit(1);
                    }
                    println!("Boost set to +{} dB", db);
                }
                Ok(false) => {
                    error!("Corsair audio device not found");
                    std::process::exit(1);
                }
                Err(e) => {
                    error!("{}", e);
                    std::process::exit(1);
                }
            }
        }
        Command::Stop => {
            eprintln!("No running instance found");
            std::process::exit(1);
        }
    }
}

fn print_response(command: &Command, response: &IpcResponse) {
    match response {
        IpcResponse::Status {
            mic_up,
            battery_percent,
            battery_status,
            boost_db,
            connected,
        } => {
            println!(
                "Mic: {}",
                if *mic_up {
                    "Muted (UP)"
                } else {
                    "Active (DOWN)"
                }
            );
            println!("Battery: {}% ({})", battery_percent, battery_status);
            println!("Boost: +{} dB", boost_db);
            println!(
                "Status: {}",
                if *connected {
                    "Connected"
                } else {
                    "Disconnected"
                }
            );
        }
        IpcResponse::Ok => match command {
            Command::Boost { db } => println!("Boost set to +{} dB", db),
            Command::Stop => println!("Instance stopped"),
            _ => println!("OK"),
        },
        IpcResponse::Error(msg) => {
            eprintln!("Error: {}", msg);
            std::process::exit(1);
        }
    }
}
