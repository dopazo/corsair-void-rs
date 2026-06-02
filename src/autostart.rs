use log::info;
use std::sync::mpsc::Sender;

/// Result of an auto-start toggle, delivered to the tray event loop. On Linux the
/// `systemctl` invocation runs off the tray thread, so its success/failure arrives
/// asynchronously; the loop persists the config on success or reverts the menu
/// checkbox on failure.
pub struct AutoStartOutcome {
    pub enabled: bool,
    pub result: Result<(), String>,
}

#[cfg(windows)]
pub fn set_auto_start(enabled: bool, tx: Sender<AutoStartOutcome>) {
    // Registry access is near-instant, so this stays inline; we still report via
    // the channel so the tray handles both platforms uniformly.
    let result = set_auto_start_windows(enabled).map_err(|e| e.to_string());
    let _ = tx.send(AutoStartOutcome { enabled, result });
}

#[cfg(windows)]
fn set_auto_start_windows(enabled: bool) -> Result<(), Box<dyn std::error::Error>> {
    use winreg::enums::*;
    use winreg::RegKey;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let run_key = hkcu.open_subkey_with_flags(
        r"Software\Microsoft\Windows\CurrentVersion\Run",
        KEY_SET_VALUE | KEY_QUERY_VALUE,
    )?;

    if enabled {
        let exe_path = std::env::current_exe()?;
        run_key.set_value("CorsairVoid", &exe_path.to_string_lossy().to_string())?;
        info!("Auto-start enabled: {}", exe_path.display());
    } else {
        let _ = run_key.delete_value("CorsairVoid");
        info!("Auto-start disabled");
    }
    Ok(())
}

#[cfg(target_os = "linux")]
pub fn set_auto_start(enabled: bool, tx: Sender<AutoStartOutcome>) {
    let service_path = match dirs::config_dir() {
        Some(dir) => dir.join("systemd/user/corsair-void.service"),
        None => {
            let _ = tx.send(AutoStartOutcome {
                enabled,
                result: Err("Could not determine config dir".into()),
            });
            return;
        }
    };

    if enabled {
        // Fast inline work: write the unit file. Report an immediate failure now so
        // the checkbox reverts without waiting on systemctl.
        let write_result = (|| -> std::io::Result<()> {
            let exe_path = std::env::current_exe()?;
            let service_content = format!(
                "[Unit]\n\
                 Description=Corsair Void controller\n\
                 \n\
                 [Service]\n\
                 ExecStart={}\n\
                 Restart=on-failure\n\
                 \n\
                 [Install]\n\
                 WantedBy=default.target\n",
                exe_path.display()
            );
            if let Some(parent) = service_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&service_path, service_content)
        })();
        if let Err(e) = write_result {
            let _ = tx.send(AutoStartOutcome {
                enabled,
                result: Err(e.to_string()),
            });
            return;
        }
        // `systemctl enable --now` talks to the systemd user manager over dbus and
        // can block for seconds — run it off the tray event loop and report back.
        std::thread::spawn(move || {
            let result = run_systemctl("enable");
            if result.is_ok() {
                info!("Auto-start enabled via systemd");
            }
            let _ = tx.send(AutoStartOutcome { enabled, result });
        });
    } else {
        // The unit file removal is fast and authoritative; a `systemctl disable`
        // failure is non-fatal, so we always report success and just run it
        // off-thread to avoid blocking the tray.
        let _ = std::fs::remove_file(&service_path);
        std::thread::spawn(move || {
            let _ = run_systemctl("disable");
            info!("Auto-start disabled");
            let _ = tx.send(AutoStartOutcome {
                enabled,
                result: Ok(()),
            });
        });
    }
}

#[cfg(target_os = "linux")]
fn run_systemctl(action: &str) -> Result<(), String> {
    match std::process::Command::new("systemctl")
        .args(["--user", action, "--now", "corsair-void"])
        .status()
    {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(format!("systemctl {} exited with {}", action, status)),
        Err(e) => Err(format!("failed to run systemctl: {}", e)),
    }
}
