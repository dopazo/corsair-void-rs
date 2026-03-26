use log::info;

#[cfg(windows)]
pub fn set_auto_start(enabled: bool) -> Result<(), Box<dyn std::error::Error>> {
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
pub fn set_auto_start(enabled: bool) -> Result<(), Box<dyn std::error::Error>> {
    let service_path = dirs::config_dir()
        .ok_or("Could not determine config dir")?
        .join("systemd/user/corsair-void.service");

    if enabled {
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
        std::fs::write(&service_path, service_content)?;
        std::process::Command::new("systemctl")
            .args(["--user", "enable", "--now", "corsair-void"])
            .status()?;
        info!("Auto-start enabled via systemd");
    } else {
        let _ = std::process::Command::new("systemctl")
            .args(["--user", "disable", "--now", "corsair-void"])
            .status();
        let _ = std::fs::remove_file(&service_path);
        info!("Auto-start disabled");
    }
    Ok(())
}

