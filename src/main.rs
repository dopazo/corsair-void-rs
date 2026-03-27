// Hide console window when launched from Start Menu / autostart.
// CLI subcommands still work when run from an existing terminal.
#![windows_subsystem = "windows"]

mod audio;
mod config;
mod device;
mod ipc;
mod sound;
mod tray;
pub mod autostart;

use clap::{Parser, Subcommand};
use log::{error, info, warn};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use config::Config;
use device::hid::HidBackend;
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
    let val: u8 = s.parse().map_err(|_| format!("'{}' is not a valid number", s))?;
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
    if sound_player.is_none() {
        warn!("Sound player could not be initialized");
    }

    // Channels
    let (device_tx, device_rx) = mpsc::channel::<DeviceEvent>();
    let (ipc_tx, ipc_rx) = mpsc::channel::<IpcCommand>();

    // Spawn HID polling thread
    thread::spawn(move || {
        hid_polling_loop(device_tx);
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

fn hid_polling_loop(tx: mpsc::Sender<DeviceEvent>) {
    loop {
        match HidBackend::open() {
            Ok(device) => {
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

                    // Periodically re-send notification request to keep the dongle reporting
                    if last_notif_request.elapsed() >= Duration::from_millis(NOTIF_REFRESH_INTERVAL_MS) {
                        if let Err(e) = device.request_notifications() {
                            warn!("Notification request failed: {}", e);
                            consecutive_errors += 1;
                        } else {
                            last_notif_request = std::time::Instant::now();
                        }
                    }

                    // Health check: detect stale handles after sleep/hibernation.
                    // If no data for a while, send a status request — the dongle should
                    // always respond. If it doesn't within 5s, the handle is dead.
                    if health_check_at.is_none()
                        && last_data_received.elapsed() >= Duration::from_millis(NO_DATA_TIMEOUT_MS)
                    {
                        info!("No HID data for {}s, sending health check", NO_DATA_TIMEOUT_MS / 1000);
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
                        warn!("Too many consecutive HID errors ({}), considering device disconnected", consecutive_errors);
                        break;
                    }

                    if consecutive_errors > 0 {
                        thread::sleep(Duration::from_millis(500));
                    }
                }

                let _ = tx.send(DeviceEvent::Disconnected);
            }
            Err(_) => {
                // Device not found, wait and retry
            }
        }

        thread::sleep(Duration::from_millis(RECONNECT_INTERVAL_MS));
    }
}

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
                let _ = tx.send(IpcCommand {
                    message,
                    responder,
                });
                // Wait briefly for the main thread to process and respond
                thread::sleep(Duration::from_millis(100));
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
            match HidBackend::open() {
                Ok(device) => {
                    if let Err(e) = device.request_status() {
                        error!("Failed to request status: {}", e);
                        std::process::exit(1);
                    }
                    match device.read_status(1000) {
                        Ok(Some(status)) => {
                            println!(
                                "Mic: {}",
                                if status.mic_up { "Muted (UP)" } else { "Active (DOWN)" }
                            );
                            println!("Battery: {}% ({})", status.battery_percent, status.battery_status);
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
                if *mic_up { "Muted (UP)" } else { "Active (DOWN)" }
            );
            println!("Battery: {}% ({})", battery_percent, battery_status);
            println!("Boost: +{} dB", boost_db);
            println!(
                "Status: {}",
                if *connected { "Connected" } else { "Disconnected" }
            );
        }
        IpcResponse::Ok => {
            match command {
                Command::Boost { db } => println!("Boost set to +{} dB", db),
                Command::Stop => println!("Instance stopped"),
                _ => println!("OK"),
            }
        }
        IpcResponse::Error(msg) => {
            eprintln!("Error: {}", msg);
            std::process::exit(1);
        }
    }
}
