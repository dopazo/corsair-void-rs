use log::{debug, info};

#[derive(Debug, Clone)]
pub enum IpcMessage {
    Status,
    Boost(u8),
    Stop,
}

#[derive(Debug, Clone)]
pub enum IpcResponse {
    Status {
        mic_up: bool,
        battery_percent: u8,
        battery_status: String,
        boost_db: u8,
        connected: bool,
    },
    Ok,
    Error(String),
}

impl IpcMessage {
    pub fn serialize(&self) -> String {
        match self {
            Self::Status => "STATUS\n".to_string(),
            Self::Boost(db) => format!("BOOST {}\n", db),
            Self::Stop => "STOP\n".to_string(),
        }
    }

    pub fn parse(line: &str) -> Option<Self> {
        let trimmed = line.trim();
        if trimmed.eq_ignore_ascii_case("STATUS") {
            Some(Self::Status)
        } else if trimmed.eq_ignore_ascii_case("STOP") {
            Some(Self::Stop)
        } else if let Some(rest) = trimmed.strip_prefix("BOOST ") {
            // Snap to a supported level: IPC is an external boundary that, unlike the
            // clap-gated CLI, can carry any u8. Normalizing here keeps the persisted
            // config and tray menu consistent with the {0,5,10} contract.
            rest.trim()
                .parse::<u8>()
                .ok()
                .map(|db| Self::Boost(crate::audio::normalize_boost_db(db)))
        } else {
            None
        }
    }
}

impl IpcResponse {
    pub fn serialize(&self) -> String {
        match self {
            Self::Status {
                mic_up,
                battery_percent,
                battery_status,
                boost_db,
                connected,
            } => format!(
                "OK:mic={},battery={},battery_status={},boost_db={},connected={}\n",
                if *mic_up { "muted" } else { "active" },
                battery_percent,
                battery_status,
                boost_db,
                connected,
            ),
            Self::Ok => "OK\n".to_string(),
            Self::Error(msg) => format!("ERR:{}\n", msg),
        }
    }

    pub fn parse(line: &str) -> Self {
        let trimmed = line.trim();
        if trimmed == "OK" {
            return Self::Ok;
        }
        if let Some(fields) = trimmed.strip_prefix("OK:") {
            let mut mic_up = false;
            let mut battery_percent = 0u8;
            let mut battery_status = String::new();
            let mut boost_db = 0u8;
            let mut connected = false;
            for field in fields.split(',') {
                if let Some((key, val)) = field.split_once('=') {
                    match key {
                        "mic" => mic_up = val == "muted",
                        "battery" => battery_percent = val.parse().unwrap_or(0),
                        "battery_status" => battery_status = val.to_string(),
                        "boost_db" => boost_db = val.parse().unwrap_or(0),
                        "connected" => connected = val == "true",
                        _ => {}
                    }
                }
            }
            return Self::Status {
                mic_up,
                battery_percent,
                battery_status,
                boost_db,
                connected,
            };
        }
        if let Some(msg) = trimmed.strip_prefix("ERR:") {
            return Self::Error(msg.to_string());
        }
        Self::Error(format!("Unknown response: {}", trimmed))
    }
}

// ─── Platform-specific IPC ───

#[cfg(windows)]
mod platform {
    use super::*;

    use windows::core::HSTRING;
    use windows::Win32::Foundation::{
        CloseHandle, ERROR_NO_DATA, ERROR_PIPE_CONNECTED, HANDLE, INVALID_HANDLE_VALUE,
    };
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_SHARE_NONE,
        OPEN_EXISTING, PIPE_ACCESS_DUPLEX,
    };
    use windows::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, WaitNamedPipeW,
        PIPE_READMODE_BYTE, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
    };

    const PIPE_NAME: &str = r"\\.\pipe\corsair-void";
    const PIPE_BUFFER_SIZE: u32 = 1024;

    pub struct IpcServer {
        handle: HANDLE,
    }

    impl IpcServer {
        pub fn bind() -> Result<Self, std::io::Error> {
            let pipe_name = HSTRING::from(PIPE_NAME);
            let handle = unsafe {
                CreateNamedPipeW(
                    &pipe_name,
                    PIPE_ACCESS_DUPLEX,
                    PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                    PIPE_UNLIMITED_INSTANCES,
                    PIPE_BUFFER_SIZE,
                    PIPE_BUFFER_SIZE,
                    0,
                    None,
                )
            };
            if handle == INVALID_HANDLE_VALUE {
                return Err(std::io::Error::last_os_error());
            }

            info!("IPC server listening on {}", PIPE_NAME);
            Ok(Self { handle })
        }

        /// Block until a client connects, read the message, and return it along with
        /// a responder that can send back a response.
        pub fn accept(&self) -> Result<(IpcMessage, IpcResponder), std::io::Error> {
            unsafe {
                if let Err(e) = ConnectNamedPipe(self.handle, None) {
                    let connected = windows::core::HRESULT::from_win32(ERROR_PIPE_CONNECTED.0);
                    let no_data = windows::core::HRESULT::from_win32(ERROR_NO_DATA.0);
                    // ERROR_PIPE_CONNECTED (535): a client connected in the gap before
                    // this call — Win32 documents it as a successful connect.
                    // ERROR_NO_DATA (232): connected-but-already-closing; still safe to
                    // attempt the read. Anything else is a genuine failure.
                    if e.code() != connected && e.code() != no_data {
                        return Err(std::io::Error::other(e.to_string()));
                    }
                }
            }

            let mut buf = [0u8; 1024];
            let total_read = unsafe {
                let mut bytes_read = 0u32;
                let _ = windows::Win32::Storage::FileSystem::ReadFile(
                    self.handle,
                    Some(&mut buf),
                    Some(&mut bytes_read),
                    None,
                );
                bytes_read as usize
            };

            let line = String::from_utf8_lossy(&buf[..total_read]);
            let msg = IpcMessage::parse(&line).ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, "Invalid IPC message")
            })?;

            debug!("IPC received: {:?}", msg);
            Ok((
                msg,
                IpcResponder {
                    handle: self.handle,
                },
            ))
        }

        /// Disconnect current client and prepare for the next one.
        pub fn disconnect_client(&self) {
            unsafe {
                let _ = DisconnectNamedPipe(self.handle);
            }
        }
    }

    impl Drop for IpcServer {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.handle);
            }
        }
    }

    pub struct IpcResponder {
        handle: HANDLE,
    }

    // SAFETY: The HANDLE is only used from one thread at a time (IPC server sends
    // it to main thread via mpsc, then main thread writes the response).
    unsafe impl Send for IpcResponder {}

    impl IpcResponder {
        pub fn send(&self, response: IpcResponse) -> Result<(), std::io::Error> {
            let data = response.serialize();
            let bytes = data.as_bytes();
            unsafe {
                let mut written = 0u32;
                windows::Win32::Storage::FileSystem::WriteFile(
                    self.handle,
                    Some(bytes),
                    Some(&mut written),
                    None,
                )
                .map_err(|e| std::io::Error::other(e.to_string()))?;
            }
            Ok(())
        }

        /// Block until the client has drained the pipe, so a response written just
        /// before the daemon exits (e.g. the `Stop` ack) isn't lost to the close.
        pub fn flush(&self) {
            // FlushFileBuffers on the server end does not return until the client
            // has read all buffered data. Our CLI reads immediately, so this returns
            // promptly; we ignore the result since a failed flush only means the
            // client already went away.
            unsafe {
                let _ = windows::Win32::Storage::FileSystem::FlushFileBuffers(self.handle);
            }
        }
    }

    pub struct IpcClient;

    impl IpcClient {
        /// Check if a tray instance is running by probing the named pipe.
        pub fn is_running() -> bool {
            let pipe_name = HSTRING::from(PIPE_NAME);
            // WaitNamedPipeW checks if the pipe exists without consuming a connection.
            // Timeout of 0 (or minimal) just tests existence.
            unsafe { WaitNamedPipeW(&pipe_name, 100).as_bool() }
        }

        /// Connect to the running instance and send a message, returning the response.
        pub fn send(msg: IpcMessage) -> Result<IpcResponse, std::io::Error> {
            let pipe_name = HSTRING::from(PIPE_NAME);
            let handle = unsafe {
                CreateFileW(
                    &pipe_name,
                    (FILE_GENERIC_READ | FILE_GENERIC_WRITE).0,
                    FILE_SHARE_NONE,
                    None,
                    OPEN_EXISTING,
                    FILE_ATTRIBUTE_NORMAL,
                    None,
                )
            }
            .map_err(|e| {
                std::io::Error::new(std::io::ErrorKind::ConnectionRefused, e.to_string())
            })?;

            // Write the message
            let data = msg.serialize();
            unsafe {
                let mut written = 0u32;
                windows::Win32::Storage::FileSystem::WriteFile(
                    handle,
                    Some(data.as_bytes()),
                    Some(&mut written),
                    None,
                )
                .map_err(|e| std::io::Error::other(e.to_string()))?;
            }

            // Read response
            let mut buf = [0u8; 1024];
            let bytes_read;
            unsafe {
                let mut read = 0u32;
                let _ = windows::Win32::Storage::FileSystem::ReadFile(
                    handle,
                    Some(&mut buf),
                    Some(&mut read),
                    None,
                );
                bytes_read = read as usize;
                let _ = CloseHandle(handle);
            }

            let line = String::from_utf8_lossy(&buf[..bytes_read]);
            Ok(IpcResponse::parse(&line))
        }
    }
}

#[cfg(unix)]
mod platform {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::PathBuf;
    use std::time::Duration;

    /// Drop a client that stops sending mid-message rather than hanging the single
    /// serial accept loop forever.
    const IPC_READ_TIMEOUT: Duration = Duration::from_secs(2);
    /// Cap the bytes read from a single connection so a newline-free flood can't
    /// grow the buffer until OOM.
    const MAX_IPC_LINE: u64 = 1024;

    fn socket_path() -> PathBuf {
        if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
            PathBuf::from(runtime_dir).join("corsair-void.sock")
        } else {
            PathBuf::from("/tmp/corsair-void.sock")
        }
    }

    pub struct IpcServer {
        listener: UnixListener,
    }

    impl IpcServer {
        pub fn bind() -> Result<Self, std::io::Error> {
            let path = socket_path();
            // Remove stale socket file
            let _ = std::fs::remove_file(&path);
            let listener = UnixListener::bind(&path)?;
            info!("IPC server listening on {}", path.display());
            Ok(Self { listener })
        }

        pub fn accept(&self) -> Result<(IpcMessage, IpcResponder), std::io::Error> {
            let (stream, _) = self.listener.accept()?;
            // Bound the read: a stalled client surfaces as WouldBlock/TimedOut (the
            // caller disconnects and continues), and the take() cap stops a
            // newline-free flood before it can exhaust memory.
            stream.set_read_timeout(Some(IPC_READ_TIMEOUT))?;
            let mut reader = BufReader::new(stream.try_clone()?).take(MAX_IPC_LINE);
            let mut line = String::new();
            reader.read_line(&mut line)?;

            let msg = IpcMessage::parse(&line).ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, "Invalid IPC message")
            })?;

            debug!("IPC received: {:?}", msg);
            Ok((msg, IpcResponder { stream }))
        }

        pub fn disconnect_client(&self) {
            // Unix sockets don't need explicit disconnect
        }
    }

    impl Drop for IpcServer {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(socket_path());
        }
    }

    pub struct IpcResponder {
        stream: UnixStream,
    }

    impl IpcResponder {
        pub fn send(&self, response: IpcResponse) -> Result<(), std::io::Error> {
            (&self.stream).write_all(response.serialize().as_bytes())
        }

        /// No-op on Unix: `write_all` already handed the bytes to the kernel socket
        /// buffer, which survives our exit until the client reads them. Mirrors the
        /// Windows responder's `flush()` so the caller is platform-agnostic.
        pub fn flush(&self) {}
    }

    pub struct IpcClient;

    impl IpcClient {
        pub fn is_running() -> bool {
            UnixStream::connect(socket_path()).is_ok()
        }

        pub fn send(msg: IpcMessage) -> Result<IpcResponse, std::io::Error> {
            let mut stream = UnixStream::connect(socket_path())?;
            stream.write_all(msg.serialize().as_bytes())?;

            // Mirror the server bounds so the CLI can't hang or over-read if the
            // daemon stalls mid-response.
            stream.set_read_timeout(Some(IPC_READ_TIMEOUT))?;
            let mut reader = BufReader::new(stream).take(MAX_IPC_LINE);
            let mut line = String::new();
            reader.read_line(&mut line)?;
            Ok(IpcResponse::parse(&line))
        }
    }
}

pub use platform::{IpcClient, IpcResponder, IpcServer};
