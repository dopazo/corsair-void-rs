# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Cross-platform (Windows & Linux) Rust CLI + system tray application to control the Corsair Void Elite RGB Wireless headset, as a lightweight alternative to iCUE. The full design specification lives in `corsair-void-rs.md`.

## Build & Run Commands

```bash
cargo build                    # Debug build
cargo build --release          # Release build
cargo run                      # Run (starts system tray by default)
cargo run -- status            # CLI: show headset status
cargo run -- gain 50           # CLI: set mic gain
cargo run -- stop              # CLI: stop running instance
cargo test                     # Run all tests
cargo test <test_name>         # Run a single test
cargo clippy                   # Lint
cargo fmt                      # Format code
RUST_LOG=debug cargo run       # Run with debug logging
```

## Architecture

**Threading model**: No async runtime. Uses `std::thread` + `std::sync::mpsc` channels.

- **Main thread**: OS event loop (Win32 message pump / GTK) for system tray via `tray-icon`/`muda`
- **HID polling thread**: Reads dongle every 100ms via `hidapi`, sends events over mpsc to main thread
- **IPC server thread**: Named pipe (Windows `\\.\pipe\corsair-void`) / Unix socket (Linux `$XDG_RUNTIME_DIR/corsair-void.sock`) for CLI-to-tray communication

**Single-instance model**: When tray is running, CLI commands go through IPC. When no instance is running, CLI opens HID directly for one-shot commands.

## Key Modules

| Module | Purpose |
|---|---|
| `device/protocol.rs` | HID report IDs, offsets, constants extracted from reference driver |
| `device/hid.rs` | HID communication via `hidapi` (Windows + Linux fallback) |
| `device/sysfs.rs` | Linux sysfs backend for kernel 6.13+ with `hid-corsair-void` driver |
| `audio/windows.rs` | Mute/gain via Windows Audio API (`windows` crate) |
| `audio/linux.rs` | Mute/gain via PulseAudio/PipeWire (`libpulse-binding`) |
| `tray/` | System tray icon with runtime-drawn overlays (mute, battery, warning) |
| `ipc/` | Named pipe / Unix socket IPC |
| `sound.rs` | Programmatic tone generation via `rodio` (no audio files) |
| `config.rs` | TOML config via `serde` + `toml`, paths via `dirs` crate |

## HID Protocol

Reference driver: [stuarthayhurst/corsair-void-driver](https://github.com/stuarthayhurst/corsair-void-driver) (`src/hid-corsair-void.c`).

- Vendor ID: `0x1b1c`. Target Product IDs: `0x0a0c`, `0x0a2b`, `0x1b23`, `0x0a14`, `0x0a16`, `0x0a51`, `0x0a55`
- Communication uses HID feature reports with 12-byte packets
- Report ID 100: mic position (byte 2, bit 7), battery % (byte 2, bits 0-6), battery status (byte 4)
- Commands: `0xC9` (status request), `0xCA` (notification request)
- Charging firmware reports ~54% higher than actual; clamp to `min(reported, 100)`

## Platform-Specific Behavior

- **Audio muting**: Targets only the Corsair Void capture device, not global input
- **Mute override**: If user unmutes from OS while mic is up, app waits for next mic movement to re-engage
- **Reconnection**: Passive wait on OS hotplug event (no polling)
- **Linux hybrid backend**: Uses sysfs when kernel driver is loaded, falls back to hidapi otherwise. Detection at startup by checking sysfs attribute existence
- **Auto-start**: Windows Registry (`HKCU\...\Run`) / Linux systemd user service

## Linux Build Dependencies

```bash
# Debian/Ubuntu
sudo apt install libudev-dev libusb-1.0-0-dev libhidapi-dev
# Arch
sudo pacman -S hidapi libusb
# Fedora
sudo dnf install hidapi-devel libudev-devel
```
