# corsair-void-rs

Cross-platform (Windows & Linux) Rust application to control the Corsair Void Elite RGB Wireless headset, as a lightweight alternative to iCUE.

---

## Primary reference

Based on the open-source Linux driver by Stuart Hayhurst:

> **[https://github.com/stuarthayhurst/corsair-void-driver](https://github.com/stuarthayhurst/corsair-void-driver)**

The driver has reverse-engineered the HID protocol of the Void Elite USB dongle. **Read `src/hid-corsair-void.c`** to extract:

- **Product IDs** for each headset model
- **HID report offsets and values** (mic position, battery, etc.)
- The communication protocol (feature reports, report IDs)

All proprietary protocol details are already solved there — do not duplicate that work.

---

## Target device

**Corsair Void Elite RGB Wireless only.** No support for wired or surround variants.

Wireless Product IDs (Vendor ID `0x1b1c`): `0x0a0c`, `0x0a2b`, `0x1b23`, `0x0a14`, `0x0a16`, `0x0a51`, `0x0a55`.

The specific Product ID for the Void Elite RGB Wireless must be identified from the driver source.

---

## Features

- Automatic mute when microphone is raised (OS-level mute via audio API)
- Microphone gain control (applied to the headset capture device only, not global input)
- Battery indicator with percentage display
- Low battery alert (audible 3-tone warning at 20%)
- CLI for quick commands
- System tray with dynamic icon (overlays for mute/battery state)
- Auto-start on login (toggle from tray menu)
- Reconnection handling (passive wait with notification on disconnect)

---

## Architecture decisions

### Threading model

No async runtime. The app uses **`std::thread`** and **`std::sync::mpsc`** channels:

- **Main thread**: Runs the OS event loop (Win32 message pump / GTK main loop) for the system tray
- **HID polling thread**: Reads the dongle every 100ms, sends events via mpsc channel to the main thread
- **IPC server thread**: Listens for CLI commands via named pipe (Windows) / Unix domain socket (Linux)

Rationale: `hidapi` is synchronous. `tray-icon`/`muda` require the main thread's OS event loop. Tokio would add complexity without benefit.

### IPC (CLI ↔ Tray)

Single-instance architecture. When the tray is running:

- CLI commands connect via **named pipe** (Windows: `\\.\pipe\corsair-void`) or **Unix domain socket** (Linux: `$XDG_RUNTIME_DIR/corsair-void.sock`)
- The CLI sends the command, receives the response, and exits
- If no instance is running, CLI opens the HID device directly for one-shot commands

### Mute behavior

- **OS-level mute**: When the mic is raised, the app mutes the Corsair Void capture device via the OS audio API (Windows Audio API / PulseAudio)
- **Respects manual override**: If the user unmutes manually from OS settings while mic is up, the app does NOT re-mute aggressively. It waits for the next mic movement (down → up) to re-engage
- Mute applies **only to the Corsair Void capture device**, not global input

### Reconnection

When the dongle is disconnected or the headset powers off:

1. The tray icon updates to show "disconnected" state
2. The app waits passively for an OS hotplug event (no polling)
3. On reconnection, resumes normal operation automatically

### Linux backend (hybrid)

On Linux with kernel 6.13+ (driver `hid-corsair-void` loaded):

| Feature | Backend |
|---|---|
| Microphone position (`microphone_up`) | sysfs (read-only) |
| Battery level | sysfs (read-only) |
| Mic gain | PulseAudio / PipeWire |

If the kernel driver is **not** loaded, fall back to `hidapi` for HID communication (same as Windows).

Detection: Check for the existence of sysfs attributes at startup.

### Configuration

- Resolved via the `dirs` crate (`dirs::config_dir()`)
  - Linux: `~/.config/corsair-void/config.toml`
  - Windows: `%APPDATA%\corsair-void\config.toml`
- **Defaults are hardcoded**. No config file is created on first run
- The file is only created/updated when the user changes a setting via CLI or tray menu
- Format: TOML via `serde` + `toml`

---

## Library stack

| Purpose | Rust crate |
|---|---|
| HID communication | `hidapi` |
| System tray | `tray-icon` |
| Tray menu | `muda` |
| Audio (Windows) | `windows` (Win32 Media Audio) |
| Audio (Linux) | `libpulse-binding` |
| Sound generation | `rodio` |
| CLI | `clap` |
| Configuration | `serde` + `toml` |
| Config paths | `dirs` |
| Logging | `env_logger` + `log` |

---

## Cargo.toml

```toml
[dependencies]
hidapi     = "2.6"
clap       = { version = "4", features = ["derive"] }
serde      = { version = "1", features = ["derive"] }
toml       = "0.8"
tray-icon  = "0.17"
muda       = "0.15"
rodio      = "0.17"
dirs       = "5"
env_logger = "0.11"
log        = "0.4"

[target.'cfg(windows)'.dependencies]
windows = { version = "0.58", features = ["Win32_Media_Audio"] }

[target.'cfg(unix)'.dependencies]
libpulse-binding = "2.28"
```

---

## Project structure

```
corsair-void-rs/
├── src/
│   ├── main.rs           # Entry point, CLI args, thread orchestration
│   ├── device/
│   │   ├── mod.rs        # Device abstraction (trait)
│   │   ├── hid.rs        # HID communication via hidapi (Windows + Linux fallback)
│   │   ├── sysfs.rs      # Linux sysfs backend (when kernel driver is loaded)
│   │   └── protocol.rs   # HID report IDs, offsets, constants (from driver source)
│   ├── audio/
│   │   ├── mod.rs        # Audio control trait
│   │   ├── windows.rs    # Mute/gain via Windows Audio API
│   │   └── linux.rs      # Mute/gain via PulseAudio/PipeWire
│   ├── tray/
│   │   └── mod.rs        # System tray, menu, icon overlays
│   ├── ipc/
│   │   └── mod.rs        # Named pipe (Windows) / Unix socket (Linux) IPC
│   ├── sound.rs          # Tone generation via rodio (mute/unmute/low battery)
│   └── config.rs         # Configuration (dirs + serde + toml)
├── assets/
│   ├── icon.png          # Base tray icon
│   └── icon.ico          # Windows icon format
├── Cargo.toml
└── README.md
```

---

## Execution architecture

```
┌─────────────────────────────────────┐
│           Main thread               │
│   CLI args → start tray / run cmd   │
└────────────────┬────────────────────┘
                 │
    ┌────────────┼────────────────┐
    │            │                │
    ▼            ▼                ▼
┌─────────┐ ┌──────────┐ ┌────────────┐
│ HID     │ │ IPC      │ │ System     │
│ polling │ │ server   │ │ Tray       │
│ thread  │ │ thread   │ │ (main)     │
│ (100ms) │ │ (socket) │ │            │
└────┬────┘ └────┬─────┘ └────────────┘
     │           │              ▲
     │    mpsc   │    mpsc      │
     └───────────┴──────────────┘
              events:
         mic_up changed → mute/unmute
         battery changed → update tray
         cli command → apply + respond
```

---

## HID protocol details

Extracted from `hid-corsair-void.c`:

| Field | Report ID | Byte | Details |
|---|---|---|---|
| Microphone position | 100 | Byte 2, Bit 7 | 1 = mic UP (muted), 0 = mic DOWN (active) |
| Battery percentage | 100 | Byte 2 | 0-100% (bits 0-6, after masking bit 7) |
| Battery status | 100 | Byte 4 | 1=Normal, 2=Low, 3=Critical, 4=Full, 5=Charging |
| Status request | — | — | Command `0xC9` |
| Notification request | — | — | Command `0xCA` |

**Note**: When charging, the firmware reports ~54% higher than actual capacity. The app clamps the displayed value to `min(reported, 100)` and appends the charging status.

Communication uses **HID feature reports** with 12-byte packets.

---

## Sound effects

Generated programmatically via `rodio` on **both platforms** (Windows and Linux). No external audio files. The binary is fully self-contained.

| Event | Pattern | Frequencies | Duration |
|---|---|---|---|
| Mic muted (raised) | high → low | 1000 Hz → 700 Hz | ~150ms per tone |
| Mic active (lowered) | low → high | 700 Hz → 1000 Hz | ~150ms per tone |
| Low battery (≤20%) | low → high → low | 700 Hz → 1000 Hz → 700 Hz | ~150ms per tone |

```rust
fn generate_tone(freq: f32, duration_ms: u32, sample_rate: u32) -> Vec<f32> {
    let samples = (sample_rate * duration_ms / 1000) as usize;
    (0..samples)
        .map(|i| {
            let t = i as f32 / sample_rate as f32;
            (2.0 * std::f32::consts::PI * freq * t).sin() * 0.5
        })
        .collect()
}
```

The low battery sound plays **once** when crossing the 20% threshold. It does not repeat until the headset is recharged and drops below 20% again.

### Configuration (config.toml)

```toml
[sound]
enabled      = true
volume       = 0.5      # 0.0 to 1.0
freq_high_hz = 1000
freq_low_hz  = 700
duration_ms  = 150
```

---

## System tray

### Icon

- **Base icon**: Pre-designed asset embedded in the binary via `include_bytes!()`
- **Overlays**: Drawn programmatically at runtime (e.g., red dot for muted, battery bar indicator, warning indicator for low battery)

### Menu (English UI)

```
+---------------------+
| Corsair Void        |
|---------------------|
| Battery: 78%        |
| Mic: Muted          |
|---------------------|
| Mic Gain        ▶  |
|   ├── 0%   (●)     |
|   ├── 25%          |
|   ├── 50%          |
|   ├── 75%          |
|   └── 100%         |
| Start on Login  [✓]|
|---------------------|
| Quit                |
+---------------------+
```

- **Battery**: Shows exact percentage + status (Charging, Low, Critical)
- **Mic**: Shows Muted/Active
- **Mic Gain**: Submenu with preset levels (0%, 25%, 50%, 75%, 100%). Default: 0%
- **Start on Login**: Checkbox toggle. Modifies Windows Registry (`HKCU\Software\Microsoft\Windows\CurrentVersion\Run`) or creates/removes systemd user service on Linux

---

## CLI usage

```bash
# Start in background with system tray
corsair-void

# Show current status
corsair-void status
> Mic: Muted (UP)
> Battery: 78%
> Gain: 0%

# Set microphone gain
corsair-void gain 50

# Stop the running instance
corsair-void stop
```

When the tray instance is already running, CLI commands are sent via IPC (named pipe / Unix socket). If no instance is running, the CLI opens the HID device directly.

---

## Auto-start

### Windows

The "Start on Login" toggle writes/removes a registry key:

```
HKCU\Software\Microsoft\Windows\CurrentVersion\Run
  "CorsairVoid" = "C:\path\to\corsair-void.exe"
```

### Linux

The toggle creates/removes a systemd user service:

```ini
# ~/.config/systemd/user/corsair-void.service
[Unit]
Description=Corsair Void controller

[Service]
ExecStart=/usr/local/bin/corsair-void
Restart=on-failure

[Install]
WantedBy=default.target
```

```bash
systemctl --user enable corsair-void
systemctl --user start corsair-void
```

---

## How to close the app

| Method | Windows | Linux |
|---|---|---|
| Right-click tray → Quit | Yes | Yes |
| `corsair-void stop` in terminal | Yes | Yes |
| `systemctl --user stop corsair-void` | No | Yes |
| Task Manager → end process | Yes | No |

---

## Cross-platform compatibility

| Feature | Windows | Linux |
|---|---|---|
| Auto mute on mic raise | Windows Audio API | PulseAudio/PipeWire |
| Mic gain control | Windows Audio API | PulseAudio/PipeWire |
| Battery indicator | hidapi | sysfs (driver) or hidapi |
| Mute/unmute sound | rodio | rodio |
| Low battery sound | rodio | rodio |
| System tray | Win32 | GTK |
| Auto-start | Registry | systemd user service |
| IPC | Named Pipe | Unix domain socket |
| HID communication | hidapi | sysfs (if driver loaded) or hidapi |

On Linux with kernel 6.13+, the `hid-corsair-void` driver is built-in. For older kernels, install manually from Stuart Hayhurst's repository.

---

## Build requirements

### Both platforms

| Tool | Installation |
|---|---|
| **Rust + Cargo** | [https://rustup.rs](https://rustup.rs) |
| **Git** | For cloning the repo and reference driver |

```bash
rustc --version
cargo --version
```

### Windows

| Requirement | Details |
|---|---|
| **Visual Studio C++ Build Tools** | MSVC linker required. Install from [visualstudio.microsoft.com](https://visualstudio.microsoft.com/visual-cpp-build-tools/) — only the "C++ Build Tools" component |
| **hidapi** | Compiled automatically as a dependency of `hidapi-rs` via Cargo |
| **Windows Audio API** | Included in Windows, accessed via the `windows` crate |

### Linux

| Requirement | Installation |
|---|---|
| **libhidapi** | Native library required by `hidapi-rs` |
| **libudev** | Required by hidapi on Linux |
| **libusb** | Alternative backend for hidapi |
| **PipeWire / PulseAudio** | For audio control (usually pre-installed) |
| **Kernel 6.13+** | Driver `hid-corsair-void` is built-in. For older kernels, install from reference repo |

```bash
# Debian / Ubuntu
sudo apt install libudev-dev libusb-1.0-0-dev libhidapi-dev

# Arch
sudo pacman -S hidapi libusb

# Fedora
sudo dnf install hidapi-devel libudev-devel
```

**udev rule** (required for non-root access to the dongle):

```bash
# /etc/udev/rules.d/99-corsair-void.rules
SUBSYSTEM=="hidraw", ATTRS{idVendor}=="1b1c", MODE="0666"

# Reload rules
sudo udevadm control --reload-rules
```

---

## Logging

Uses `env_logger`. Silent by default. Enable with:

```bash
RUST_LOG=debug corsair-void
RUST_LOG=corsair_void=trace corsair-void
```

No log files are written. All log output goes to stderr.

---

## Distribution

### Phase 1 (initial)

- **GitHub Releases**: GitHub Actions compiles for Windows (`.exe`) and Linux, uploads binaries to Releases
- README includes instructions for:
  - Where to place the binary
  - How to add to PATH
  - How to configure auto-start

### Phase 2 (future)

- Installer: NSIS (Windows), `.deb`/`.rpm` (Linux)

---

## Implementation steps

1. Read `src/hid-corsair-void.c` from the reference driver to extract the HID protocol
2. Identify the exact Product ID for the Void Elite RGB Wireless
3. Create `protocol.rs` with report IDs, offsets, and constants
4. Implement the HID polling loop (100ms) and verify events are received
5. Add automatic mute as the first working feature
6. Add battery reporting and tray icon with overlays
7. Add mic gain control (OS audio API)
8. Add sound effects (rodio)
9. Add IPC (named pipe / Unix socket) for CLI ↔ tray communication
10. Add auto-start toggle
11. Add Linux sysfs backend with auto-detection
12. Set up GitHub Actions for cross-platform builds and releases
