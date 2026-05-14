//! USB hotplug notifications.
//!
//! On Windows, a hidden message-only window listens for `WM_DEVICECHANGE`
//! events and signals an mpsc channel each time. The HID polling loop uses
//! this to wake up immediately when the dongle is (un)plugged instead of
//! waiting on a fixed polling interval.
//!
//! On other platforms this is a no-op; the polling fallback (via the same
//! `recv_timeout` timeout in the caller) preserves the previous behaviour.

use std::sync::mpsc::Sender;

#[cfg(windows)]
pub fn spawn(tx: Sender<()>) {
    platform::spawn(tx);
}

#[cfg(not(windows))]
pub fn spawn(_tx: Sender<()>) {
    // No native hotplug listener on this platform; the caller's polling
    // timeout drives reconnection attempts.
}

#[cfg(windows)]
mod platform {
    use super::Sender;
    use log::{info, warn};
    use std::ffi::c_void;
    use std::mem::size_of;
    use std::sync::{Mutex, OnceLock};

    use windows::core::w;
    use windows::Win32::Devices::HumanInterfaceDevice::GUID_DEVINTERFACE_HID;
    use windows::Win32::Foundation::{HANDLE, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, HMENU, RegisterClassW,
        RegisterDeviceNotificationW, TranslateMessage, DBT_DEVTYP_DEVICEINTERFACE,
        DEV_BROADCAST_DEVICEINTERFACE_W, DEVICE_NOTIFY_WINDOW_HANDLE, HWND_MESSAGE, MSG,
        WINDOW_EX_STYLE, WINDOW_STYLE, WM_DEVICECHANGE, WNDCLASSW,
    };

    /// Set once when the watcher thread starts. The wndproc reads from it.
    static HOTPLUG_TX: OnceLock<Mutex<Sender<()>>> = OnceLock::new();

    pub fn spawn(tx: Sender<()>) {
        if HOTPLUG_TX.set(Mutex::new(tx)).is_err() {
            warn!("Hotplug watcher already initialised");
            return;
        }
        let _ = std::thread::Builder::new()
            .name("hotplug-watcher".into())
            .spawn(run_watcher);
    }

    unsafe extern "system" fn wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        if msg == WM_DEVICECHANGE {
            if let Some(tx_mutex) = HOTPLUG_TX.get() {
                if let Ok(tx) = tx_mutex.lock() {
                    // Channel may be closed if the main process is tearing down;
                    // ignore the error in that case.
                    let _ = tx.send(());
                }
            }
            return LRESULT(1);
        }
        DefWindowProcW(hwnd, msg, wparam, lparam)
    }

    fn run_watcher() {
        unsafe {
            let hinstance = match GetModuleHandleW(None) {
                Ok(h) => h,
                Err(e) => {
                    warn!("Hotplug: GetModuleHandleW failed: {}", e);
                    return;
                }
            };

            let class_name = w!("CorsairVoidHotplugWnd");
            let wc = WNDCLASSW {
                lpfnWndProc: Some(wnd_proc),
                hInstance: hinstance.into(),
                lpszClassName: class_name,
                ..Default::default()
            };
            if RegisterClassW(&wc) == 0 {
                warn!(
                    "Hotplug: RegisterClassW failed: {}",
                    std::io::Error::last_os_error()
                );
                return;
            }

            let hwnd = match CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                class_name,
                w!(""),
                WINDOW_STYLE::default(),
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                HMENU(std::ptr::null_mut()),
                HINSTANCE::from(hinstance),
                None,
            ) {
                Ok(h) => h,
                Err(e) => {
                    warn!("Hotplug: CreateWindowExW failed: {}", e);
                    return;
                }
            };

            let mut filter = DEV_BROADCAST_DEVICEINTERFACE_W {
                dbcc_size: size_of::<DEV_BROADCAST_DEVICEINTERFACE_W>() as u32,
                dbcc_devicetype: DBT_DEVTYP_DEVICEINTERFACE.0,
                dbcc_reserved: 0,
                dbcc_classguid: GUID_DEVINTERFACE_HID,
                dbcc_name: [0; 1],
            };

            match RegisterDeviceNotificationW(
                HANDLE(hwnd.0),
                &mut filter as *mut _ as *mut c_void,
                DEVICE_NOTIFY_WINDOW_HANDLE,
            ) {
                Ok(_) => info!("Hotplug watcher listening for HID device changes"),
                Err(e) => warn!("Hotplug: RegisterDeviceNotificationW failed: {}", e),
            }

            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
}
