# corsair-void-rs

Cross-platform (Windows & Linux) Rust CLI and system tray application to control the Corsair Void wireless headset, as a lightweight alternative to iCUE.

Based on the open-source Linux driver by Stuart Hayhurst: [stuarthayhurst/corsair-void-driver](https://github.com/stuarthayhurst/corsair-void-driver).

## Features

- System tray with real-time battery percentage, mic status, and connection state
- Three tray icon states: grey (dongle disconnected), orange (dongle connected, headset off), teal (headset connected)
- Microphone boost: 0, +5, or +10 dB, persisted in config
- Low battery alert at 20%
- CLI for quick commands
- Auto-start on login toggle from tray menu
- Single-instance model with IPC (CLI commands route to running tray instance)

## Supported Devices

Corsair Void wireless headsets (Vendor ID `0x1b1c`):

| Family | Product IDs |
|---|---|
| Void Wireless | `0x0a0c`, `0x0a2b`, `0x1b23` |
| Void Pro Wireless | `0x0a14`, `0x0a16`, `0x0a1a` |
| Void Elite Wireless | `0x0a51`, `0x0a55`, `0x0a75` |

## Build

### Windows

No external C libraries needed. Win32 APIs are accessed via the `windows` crate.

```bash
cargo build --release
```

The compiled binary is at `target/release/corsair-void.exe`.

### Linux

Install system dependencies first:

```bash
# Debian/Ubuntu
sudo apt install libudev-dev libusb-1.0-0-dev libhidapi-dev libpulse-dev pkg-config

# Arch
sudo pacman -S hidapi libusb libpulse

# Fedora
sudo dnf install hidapi-devel libudev-devel pulseaudio-libs-devel
```

Then build:

```bash
cargo build --release
```

The compiled binary is at `target/release/corsair-void` (no `.exe` extension on Linux).

## Usage

### Running the tray app

The app runs as a system tray icon. There are two ways to start it:

**Run the executable directly** (recommended for daily use): double-click `corsair-void.exe` (Windows) or `corsair-void` (Linux) from your file manager. The tray icon appears with no terminal window.

**Run from a terminal:**

```bash
# Windows
.\target\release\corsair-void.exe

# Linux
./target/release/corsair-void
```

When launched from a terminal, the app runs normally but the terminal window stays open for the lifetime of the tray. On Linux, you can detach it from the terminal:

```bash
# Run in background, detached from terminal
nohup ./target/release/corsair-void &disown
```

For persistent background use, enable **Auto-start** from the tray menu -- this registers the app to start on login without needing a terminal (via Windows Registry or systemd user service).

### CLI commands

```bash
corsair-void status       # Show headset status (mic, battery, boost, connection)
corsair-void boost 5      # Set mic boost to +5 dB (0, 5, or 10)
corsair-void stop         # Stop the running tray instance
```

When the tray is running, CLI commands route through IPC. When no instance is running, `status` and `boost` open the HID device directly.

## Platform Notes

### Windows

- **Mic boost** requires [VB-CABLE](https://vb-audio.com/Cable/) (free virtual audio device). The app captures mic audio via WASAPI, amplifies it in a passthrough thread, and sends the boosted signal to VB-CABLE Input. Set "CABLE Output" as your microphone in Discord/apps. Without VB-CABLE installed, boost is unavailable and the tray menu indicates this.
- **IPC**: Named pipe at `\\.\pipe\corsair-void`
- **Auto-start**: Registry key at `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`

### Linux

- **Mic boost** works natively via PulseAudio/PipeWire. No external software needed -- PulseAudio supports source volume above 100% out of the box.
- **IPC**: Unix domain socket at `$XDG_RUNTIME_DIR/corsair-void.sock`
- **Auto-start**: systemd user service
- **HID**: Uses `hidapi` by default. A sysfs backend for kernel 6.13+ with the `hid-corsair-void` driver is also available in the codebase.

### Both platforms

- **Mute/unmute** is handled by headset firmware (mic up = hardware mute). The app only displays mic status in the tray -- it does not perform software muting.

## Configuration

Config is stored as TOML:

- **Windows**: `%APPDATA%\corsair-void\config.toml`
- **Linux**: `~/.config/corsair-void/config.toml`

```toml
[general]
auto_start = false
mic_boost_db = 0
```

## Architecture

No async runtime. Uses `std::thread` and `std::sync::mpsc` channels.

| Thread | Role |
|---|---|
| Main | OS event loop (Win32 message pump / GTK) for system tray via `tray-icon` |
| HID polling | Reads dongle every 100 ms via `hidapi`, sends events to main thread |
| IPC server | Named pipe / Unix socket listener, forwards CLI commands to main thread |

## Project Structure

```
src/
  main.rs              Entry point, CLI parsing, thread orchestration
  config.rs            TOML config (auto_start, mic_boost_db)
  autostart.rs         Registry (Windows) / systemd (Linux) auto-start
  audio/
    mod.rs             AudioController trait + platform dispatch
    windows.rs         Device discovery via Windows Audio API
    boost.rs           WASAPI passthrough boost engine (Windows, requires VB-CABLE)
    linux.rs           Boost via PulseAudio (native, no external deps)
  device/
    mod.rs             DeviceEvent enum, DeviceError
    protocol.rs        HID report IDs, offsets, status parsing
    hid.rs             hidapi backend (Windows + Linux)
    sysfs.rs           Linux sysfs backend for kernel 6.13+
  tray/
    mod.rs             System tray icon, menu, runtime-drawn overlays
  ipc/
    mod.rs             IPC server/client over named pipe or Unix socket
  bin/
    hid_debug.rs       Debug: raw HID device enumeration
    audio_debug.rs     Debug: audio device enumeration
    boost_debug.rs     Debug: boost passthrough smoke test
```

## Development

```bash
cargo test                     # Run all tests
cargo clippy                   # Lint
cargo fmt                      # Format code
RUST_LOG=debug cargo run       # Run with debug logging
```
