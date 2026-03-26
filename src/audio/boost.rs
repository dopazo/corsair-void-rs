use log::{debug, error, info, warn};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use windows::core::PCWSTR;
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Media::Audio::{
    eRender, IAudioCaptureClient, IAudioClient, IAudioRenderClient,
    IMMDeviceEnumerator, MMDeviceEnumerator, AUDCLNT_BUFFERFLAGS_SILENT,
    AUDCLNT_SHAREMODE_SHARED, DEVICE_STATE_ACTIVE,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED, STGM_READ,
};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;

use super::AudioError;

struct BoostEngineInner {
    thread_handle: Option<JoinHandle<()>>,
    current_db: u8,
    gain: Arc<AtomicU32>,
    stop: Arc<AtomicBool>,
    capture_device_id: Option<String>,
    render_device_id: Option<String>,
    render_device_name: Option<String>,
}

pub struct BoostEngine {
    inner: Mutex<BoostEngineInner>,
}

fn db_to_linear(db: u8) -> f32 {
    10.0_f32.powf(db as f32 / 20.0)
}

/// Get the device ID string from an IMMDevice.
fn get_device_id(device: &windows::Win32::Media::Audio::IMMDevice) -> Result<String, AudioError> {
    unsafe {
        let id_pwstr = device
            .GetId()
            .map_err(|e| AudioError::ApiError(format!("GetId: {}", e)))?;
        let id = id_pwstr.to_string().map_err(|e| AudioError::ApiError(format!("PWSTR to string: {}", e)))?;
        windows::Win32::System::Com::CoTaskMemFree(Some(id_pwstr.0 as *const _));
        Ok(id)
    }
}

impl BoostEngine {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(BoostEngineInner {
                thread_handle: None,
                current_db: 0,
                gain: Arc::new(AtomicU32::new(f32::to_bits(1.0))),
                stop: Arc::new(AtomicBool::new(false)),
                capture_device_id: None,
                render_device_id: None,
                render_device_name: None,
            }),
        }
    }

    pub fn detect_virtual_cable(&self) {
        let inner = &mut *self.inner.lock().unwrap();
        inner.render_device_id = None;
        inner.render_device_name = None;

        match find_virtual_cable_device() {
            Ok(Some((id, name))) => {
                info!("Virtual audio cable detected: {} (ID: {})", name, id);
                inner.render_device_id = Some(id);
                inner.render_device_name = Some(name);
            }
            Ok(None) => {
                debug!("No virtual audio cable detected");
            }
            Err(e) => {
                warn!("Error detecting virtual cable: {}", e);
            }
        }
    }

    pub fn set_capture_device(&self, device: &windows::Win32::Media::Audio::IMMDevice) {
        match get_device_id(device) {
            Ok(id) => {
                debug!("Capture device ID: {}", id);
                self.inner.lock().unwrap().capture_device_id = Some(id);
            }
            Err(e) => warn!("Failed to get capture device ID: {}", e),
        }
    }

    pub fn virtual_cable_available(&self) -> bool {
        self.inner.lock().unwrap().render_device_id.is_some()
    }

    pub fn set_boost_db(&self, db: u8) -> Result<(), AudioError> {
        let inner = &mut *self.inner.lock().unwrap();

        let gain_factor = db_to_linear(db);
        inner.gain.store(f32::to_bits(gain_factor), Ordering::SeqCst);
        inner.current_db = db;

        // If thread is already running, the atomic update is sufficient
        if inner.thread_handle.is_some() {
            debug!("Updating boost gain to +{} dB (factor: {:.3})", db, gain_factor);
            return Ok(());
        }

        // Need VB-CABLE to start passthrough
        let render_id = inner
            .render_device_id
            .clone()
            .ok_or_else(|| AudioError::ApiError(
                "No virtual audio cable detected. Install VB-CABLE (https://vb-audio.com/Cable/)".into(),
            ))?;

        let capture_id = inner
            .capture_device_id
            .clone()
            .ok_or(AudioError::DeviceNotFound)?;

        // Spawn the boost thread — it does ALL WASAPI init in its own MTA apartment
        info!("Starting boost engine: +{} dB (factor: {:.3})", db, gain_factor);
        let gain = inner.gain.clone();
        let stop = inner.stop.clone();
        stop.store(false, Ordering::SeqCst);

        let handle = thread::spawn(move || {
            passthrough_thread(capture_id, render_id, gain, stop);
        });

        inner.thread_handle = Some(handle);

        Ok(())
    }

    /// Stop the passthrough thread entirely (used on disconnect/exit).
    pub fn stop(&self) {
        let inner = &mut *self.inner.lock().unwrap();
        if inner.thread_handle.is_some() {
            info!("Stopping boost engine");
            inner.stop.store(true, Ordering::SeqCst);
            if let Some(handle) = inner.thread_handle.take() {
                let _ = handle.join();
            }
            info!("Boost engine stopped");
        }
    }

    pub fn get_boost_db(&self) -> u8 {
        self.inner.lock().unwrap().current_db
    }
}

impl Drop for BoostEngine {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Find a virtual audio cable render device. Returns (device_id, friendly_name).
fn find_virtual_cable_device() -> Result<Option<(String, String)>, AudioError> {
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                .map_err(|e| AudioError::ApiError(format!("CoCreateInstance: {}", e)))?;

        let collection = enumerator
            .EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)
            .map_err(|e| AudioError::ApiError(format!("EnumAudioEndpoints: {}", e)))?;

        let count = collection
            .GetCount()
            .map_err(|e| AudioError::ApiError(format!("GetCount: {}", e)))?;

        let cable_keywords = ["cable input", "voicemeeter", "virtual cable"];

        for i in 0..count {
            let device = collection
                .Item(i)
                .map_err(|e| AudioError::ApiError(format!("Item: {}", e)))?;

            let store: IPropertyStore = device
                .OpenPropertyStore(STGM_READ)
                .map_err(|e| AudioError::ApiError(format!("OpenPropertyStore: {}", e)))?;

            let name = store
                .GetValue(&PKEY_Device_FriendlyName)
                .map_err(|e| AudioError::ApiError(format!("GetValue: {}", e)))?
                .to_string();

            let name_lower = name.to_lowercase();
            debug!("Render device {}: {}", i, name);

            for keyword in &cable_keywords {
                if name_lower.contains(keyword) {
                    let id = get_device_id(&device)?;
                    return Ok(Some((id, name)));
                }
            }
        }

        Ok(None)
    }
}

/// The boost thread entry point. Initializes COM + WASAPI entirely on this thread.
fn passthrough_thread(
    capture_id: String,
    render_id: String,
    gain: Arc<AtomicU32>,
    stop: Arc<AtomicBool>,
) {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }

    if let Err(e) = passthrough_thread_inner(&capture_id, &render_id, gain, stop) {
        error!("Boost passthrough error: {}", e);
    }

    info!("Boost passthrough thread exiting");
}

fn passthrough_thread_inner(
    capture_id: &str,
    render_id: &str,
    gain: Arc<AtomicU32>,
    stop: Arc<AtomicBool>,
) -> Result<(), AudioError> {
    unsafe {
        // Open devices by ID on this thread
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                .map_err(|e| AudioError::ApiError(format!("CoCreateInstance: {}", e)))?;

        let capture_id_wide: Vec<u16> = capture_id.encode_utf16().chain(std::iter::once(0)).collect();
        let capture_device = enumerator
            .GetDevice(PCWSTR(capture_id_wide.as_ptr()))
            .map_err(|e| AudioError::ApiError(format!("GetDevice capture: {}", e)))?;

        let render_id_wide: Vec<u16> = render_id.encode_utf16().chain(std::iter::once(0)).collect();
        let render_device = enumerator
            .GetDevice(PCWSTR(render_id_wide.as_ptr()))
            .map_err(|e| AudioError::ApiError(format!("GetDevice render: {}", e)))?;

        // Init capture WASAPI
        let capture_client: IAudioClient = capture_device
            .Activate::<IAudioClient>(CLSCTX_ALL, None)
            .map_err(|e| AudioError::ApiError(format!("Activate capture: {}", e)))?;

        let capture_format_ptr = capture_client
            .GetMixFormat()
            .map_err(|e| AudioError::ApiError(format!("GetMixFormat capture: {}", e)))?;

        let cap_rate = (*capture_format_ptr).nSamplesPerSec;
        let cap_ch = (*capture_format_ptr).nChannels;
        info!("Capture: {} Hz, {} ch", cap_rate, cap_ch);

        let capture_event: HANDLE = CreateEventW(None, false, false, None)
            .map_err(|e| AudioError::ApiError(format!("CreateEventW: {}", e)))?;

        capture_client
            .Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                windows::Win32::Media::Audio::AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
                0, 0,
                capture_format_ptr,
                None,
            )
            .map_err(|e| AudioError::ApiError(format!("Initialize capture: {}", e)))?;

        capture_client
            .SetEventHandle(capture_event)
            .map_err(|e| AudioError::ApiError(format!("SetEventHandle: {}", e)))?;

        let capture_capture: IAudioCaptureClient = capture_client
            .GetService::<IAudioCaptureClient>()
            .map_err(|e| AudioError::ApiError(format!("GetService capture: {}", e)))?;

        // Init render WASAPI
        let render_client: IAudioClient = render_device
            .Activate::<IAudioClient>(CLSCTX_ALL, None)
            .map_err(|e| AudioError::ApiError(format!("Activate render: {}", e)))?;

        let render_format_ptr = render_client
            .GetMixFormat()
            .map_err(|e| AudioError::ApiError(format!("GetMixFormat render: {}", e)))?;

        let ren_rate = (*render_format_ptr).nSamplesPerSec;
        let ren_ch = (*render_format_ptr).nChannels;
        info!("Render: {} Hz, {} ch", ren_rate, ren_ch);

        render_client
            .Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                windows::Win32::Media::Audio::AUDCLNT_STREAMFLAGS_NOPERSIST,
                0, 0,
                render_format_ptr,
                None,
            )
            .map_err(|e| AudioError::ApiError(format!("Initialize render: {}", e)))?;

        let render_buffer_size = render_client
            .GetBufferSize()
            .map_err(|e| AudioError::ApiError(format!("GetBufferSize: {}", e)))?;

        let render_render: IAudioRenderClient = render_client
            .GetService::<IAudioRenderClient>()
            .map_err(|e| AudioError::ApiError(format!("GetService render: {}", e)))?;

        let capture_channels = cap_ch as usize;
        let render_channels = ren_ch as usize;
        let need_resample = cap_rate != ren_rate;
        let sample_rate_ratio = if need_resample {
            ren_rate as f64 / cap_rate as f64
        } else {
            1.0
        };

        if need_resample {
            warn!("Sample rate mismatch: {}→{} Hz", cap_rate, ren_rate);
        }

        // Start streams
        capture_client
            .Start()
            .map_err(|e| AudioError::ApiError(format!("Start capture: {}", e)))?;
        render_client
            .Start()
            .map_err(|e| AudioError::ApiError(format!("Start render: {}", e)))?;

        info!("Boost passthrough loop running");

        // Main loop
        loop {
            if stop.load(Ordering::SeqCst) {
                break;
            }

            WaitForSingleObject(capture_event, 50);

            let mut packet_size = capture_capture
                .GetNextPacketSize()
                .map_err(|e| AudioError::ApiError(format!("GetNextPacketSize: {}", e)))?;

            while packet_size > 0 {
                let mut buffer_ptr: *mut u8 = std::ptr::null_mut();
                let mut num_frames = 0u32;
                let mut flags = 0u32;

                capture_capture
                    .GetBuffer(&mut buffer_ptr, &mut num_frames, &mut flags, None, None)
                    .map_err(|e| AudioError::ApiError(format!("GetBuffer capture: {}", e)))?;

                if num_frames > 0 {
                    let gain_factor = f32::from_bits(gain.load(Ordering::Relaxed));
                    let is_silent = (flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0;

                    let capture_samples = if is_silent {
                        vec![0.0f32; num_frames as usize * capture_channels]
                    } else {
                        let sample_count = num_frames as usize * capture_channels;
                        let f32_ptr = buffer_ptr as *const f32;
                        std::slice::from_raw_parts(f32_ptr, sample_count).to_vec()
                    };

                    capture_capture
                        .ReleaseBuffer(num_frames)
                        .map_err(|e| AudioError::ApiError(format!("ReleaseBuffer capture: {}", e)))?;

                    let output_samples = process_audio(
                        &capture_samples,
                        capture_channels,
                        render_channels,
                        gain_factor,
                        need_resample,
                        sample_rate_ratio,
                    );

                    let output_frames = output_samples.len() / render_channels;
                    let padding = render_client
                        .GetCurrentPadding()
                        .map_err(|e| AudioError::ApiError(format!("GetCurrentPadding: {}", e)))?;
                    let available = (render_buffer_size - padding) as usize;
                    let frames_to_write = output_frames.min(available);

                    if frames_to_write > 0 {
                        let render_buf = render_render
                            .GetBuffer(frames_to_write as u32)
                            .map_err(|e| AudioError::ApiError(format!("GetBuffer render: {}", e)))?;

                        let dest = std::slice::from_raw_parts_mut(
                            render_buf as *mut f32,
                            frames_to_write * render_channels,
                        );
                        dest.copy_from_slice(&output_samples[..frames_to_write * render_channels]);

                        render_render
                            .ReleaseBuffer(frames_to_write as u32, 0)
                            .map_err(|e| AudioError::ApiError(format!("ReleaseBuffer render: {}", e)))?;
                    }
                } else {
                    capture_capture
                        .ReleaseBuffer(num_frames)
                        .map_err(|e| AudioError::ApiError(format!("ReleaseBuffer: {}", e)))?;
                }

                packet_size = capture_capture
                    .GetNextPacketSize()
                    .map_err(|e| AudioError::ApiError(format!("GetNextPacketSize: {}", e)))?;
            }
        }

        let _ = capture_client.Stop();
        let _ = render_client.Stop();
    }

    Ok(())
}

fn process_audio(
    input: &[f32],
    in_channels: usize,
    out_channels: usize,
    gain: f32,
    need_resample: bool,
    sample_rate_ratio: f64,
) -> Vec<f32> {
    let amplified: Vec<f32> = input
        .iter()
        .map(|&s| (s * gain).clamp(-1.0, 1.0))
        .collect();

    let channel_converted = if in_channels == out_channels {
        amplified
    } else if in_channels == 1 && out_channels == 2 {
        amplified.iter().flat_map(|&s| [s, s]).collect()
    } else if in_channels == 2 && out_channels == 1 {
        amplified
            .chunks(2)
            .map(|pair| {
                if pair.len() == 2 {
                    (pair[0] + pair[1]) * 0.5
                } else {
                    pair[0]
                }
            })
            .collect()
    } else {
        let in_frames = amplified.len() / in_channels;
        let mut out = Vec::with_capacity(in_frames * out_channels);
        for frame in 0..in_frames {
            for ch in 0..out_channels {
                if ch < in_channels {
                    out.push(amplified[frame * in_channels + ch]);
                } else {
                    out.push(0.0);
                }
            }
        }
        out
    };

    if !need_resample {
        return channel_converted;
    }

    let in_frames = channel_converted.len() / out_channels;
    let out_frames = (in_frames as f64 * sample_rate_ratio).ceil() as usize;
    let mut resampled = Vec::with_capacity(out_frames * out_channels);

    for i in 0..out_frames {
        let src_pos = i as f64 / sample_rate_ratio;
        let src_idx = src_pos.floor() as usize;
        let frac = src_pos - src_idx as f64;

        for ch in 0..out_channels {
            let s0 = if src_idx < in_frames {
                channel_converted[src_idx * out_channels + ch]
            } else {
                0.0
            };
            let s1 = if src_idx + 1 < in_frames {
                channel_converted[(src_idx + 1) * out_channels + ch]
            } else {
                s0
            };
            resampled.push(s0 + (s1 - s0) * frac as f32);
        }
    }

    resampled
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_db_to_linear() {
        assert!((db_to_linear(0) - 1.0).abs() < 0.001);
        assert!((db_to_linear(5) - 1.778).abs() < 0.01);
        assert!((db_to_linear(10) - 3.162).abs() < 0.01);
    }

    #[test]
    fn test_process_audio_gain() {
        let input = vec![0.5, -0.3];
        let out = process_audio(&input, 1, 1, 2.0, false, 1.0);
        assert!((out[0] - 1.0).abs() < 0.001);
        assert!((out[1] - (-0.6)).abs() < 0.001);
    }

    #[test]
    fn test_process_audio_clamp() {
        let input = vec![0.8, -0.9];
        let out = process_audio(&input, 1, 1, 3.0, false, 1.0);
        assert!((out[0] - 1.0).abs() < 0.001);
        assert!((out[1] - (-1.0)).abs() < 0.001);
    }

    #[test]
    fn test_mono_to_stereo() {
        let input = vec![0.5];
        let out = process_audio(&input, 1, 2, 1.0, false, 1.0);
        assert_eq!(out.len(), 2);
        assert!((out[0] - 0.5).abs() < 0.001);
        assert!((out[1] - 0.5).abs() < 0.001);
    }

    #[test]
    fn test_stereo_to_mono() {
        let input = vec![0.4, 0.6];
        let out = process_audio(&input, 2, 1, 1.0, false, 1.0);
        assert_eq!(out.len(), 1);
        assert!((out[0] - 0.5).abs() < 0.001);
    }
}
