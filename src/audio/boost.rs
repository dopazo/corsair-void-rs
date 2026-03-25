use log::{debug, error, info, warn};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};

use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Media::Audio::{
    eRender, IAudioCaptureClient, IAudioClient, IAudioRenderClient, IMMDevice,
    IMMDeviceEnumerator, MMDeviceEnumerator, AUDCLNT_BUFFERFLAGS_SILENT,
    AUDCLNT_SHAREMODE_SHARED, DEVICE_STATE_ACTIVE, WAVEFORMATEX,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED, STGM_READ,
};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;

use super::AudioError;

enum BoostCommand {
    SetGain(f32),
}

/// WASAPI resources initialized on the main thread, sent to the passthrough thread.
struct WasapiResources {
    capture_client: IAudioClient,
    capture_capture: IAudioCaptureClient,
    capture_event: HANDLE,
    render_client: IAudioClient,
    render_render: IAudioRenderClient,
    render_buffer_size: u32,
    capture_channels: usize,
    render_channels: usize,
    need_resample: bool,
    sample_rate_ratio: f64,
}

// SAFETY: COM interfaces are initialized on the main thread but used exclusively
// on the passthrough thread after being sent. No concurrent access.
unsafe impl Send for WasapiResources {}

struct BoostEngineInner {
    cmd_tx: Option<mpsc::Sender<BoostCommand>>,
    thread_handle: Option<JoinHandle<()>>,
    current_db: u8,
    gain: Arc<AtomicU32>,
    stop: Arc<AtomicBool>,
    render_device_name: Option<String>,
    capture_device: Option<IMMDevice>,
    render_device: Option<IMMDevice>,
}

pub struct BoostEngine {
    inner: Mutex<BoostEngineInner>,
}

// SAFETY: COM objects stored in inner are only accessed under the Mutex,
// from the main thread (which called CoInitializeEx).
unsafe impl Send for BoostEngine {}
unsafe impl Sync for BoostEngine {}

fn db_to_linear(db: u8) -> f32 {
    10.0_f32.powf(db as f32 / 20.0)
}

impl BoostEngine {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(BoostEngineInner {
                cmd_tx: None,
                thread_handle: None,
                current_db: 0,
                gain: Arc::new(AtomicU32::new(f32::to_bits(1.0))),
                stop: Arc::new(AtomicBool::new(false)),
                render_device_name: None,
                capture_device: None,
                render_device: None,
            }),
        }
    }

    pub fn detect_virtual_cable(&self) {
        let inner = &mut *self.inner.lock().unwrap();
        inner.render_device_name = None;
        inner.render_device = None;

        match find_virtual_cable_device() {
            Ok(Some((device, name))) => {
                info!("Virtual audio cable detected: {}", name);
                inner.render_device_name = Some(name);
                inner.render_device = Some(device);
            }
            Ok(None) => {
                debug!("No virtual audio cable detected");
            }
            Err(e) => {
                warn!("Error detecting virtual cable: {}", e);
            }
        }
    }

    pub fn set_capture_device(&self, device: IMMDevice) {
        self.inner.lock().unwrap().capture_device = Some(device);
    }

    pub fn virtual_cable_available(&self) -> bool {
        self.inner.lock().unwrap().render_device.is_some()
    }

    pub fn set_boost_db(&self, db: u8) -> Result<(), AudioError> {
        let inner = &mut *self.inner.lock().unwrap();

        if db == 0 {
            if inner.cmd_tx.is_some() {
                info!("Stopping boost engine");
                inner.stop.store(true, Ordering::SeqCst);
                inner.cmd_tx = None;
                if let Some(handle) = inner.thread_handle.take() {
                    let _ = handle.join();
                }
                info!("Boost engine stopped");
            }
            inner.current_db = 0;
            inner.gain.store(f32::to_bits(1.0), Ordering::SeqCst);
            return Ok(());
        }

        let render_device = inner
            .render_device
            .clone()
            .ok_or_else(|| AudioError::ApiError(
                "No virtual audio cable detected. Install VB-CABLE (https://vb-audio.com/Cable/)".into(),
            ))?;

        let capture_device = inner
            .capture_device
            .clone()
            .ok_or(AudioError::DeviceNotFound)?;

        let gain_factor = db_to_linear(db);
        inner.gain.store(f32::to_bits(gain_factor), Ordering::SeqCst);
        inner.current_db = db;

        // If thread is already running, just update the gain
        if inner.cmd_tx.is_some() {
            debug!("Updating boost gain to +{} dB (factor: {:.3})", db, gain_factor);
            if let Some(tx) = &inner.cmd_tx {
                let _ = tx.send(BoostCommand::SetGain(gain_factor));
            }
            return Ok(());
        }

        // Initialize WASAPI on the main thread, then send resources to the new thread
        let resources = init_wasapi(&capture_device, &render_device)?;

        info!("Starting boost engine: +{} dB (factor: {:.3})", db, gain_factor);
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let gain = inner.gain.clone();
        let stop = inner.stop.clone();
        stop.store(false, Ordering::SeqCst);

        let handle = thread::spawn(move || {
            passthrough_loop(resources, gain, stop, cmd_rx);
        });

        inner.cmd_tx = Some(cmd_tx);
        inner.thread_handle = Some(handle);

        Ok(())
    }

    pub fn get_boost_db(&self) -> u8 {
        self.inner.lock().unwrap().current_db
    }
}

impl Drop for BoostEngine {
    fn drop(&mut self) {
        let _ = self.set_boost_db(0);
    }
}

fn find_virtual_cable_device() -> Result<Option<(IMMDevice, String)>, AudioError> {
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
                    return Ok(Some((device, name)));
                }
            }
        }

        Ok(None)
    }
}

/// Initialize WASAPI capture and render clients on the current thread.
fn init_wasapi(
    capture_device: &IMMDevice,
    render_device: &IMMDevice,
) -> Result<WasapiResources, AudioError> {
    unsafe {
        // Capture side
        let capture_client: IAudioClient = capture_device
            .Activate::<IAudioClient>(CLSCTX_ALL, None)
            .map_err(|e| AudioError::ApiError(format!("Activate capture IAudioClient: {}", e)))?;

        let capture_format_ptr = capture_client
            .GetMixFormat()
            .map_err(|e| AudioError::ApiError(format!("GetMixFormat capture: {}", e)))?;
        let capture_format = *capture_format_ptr;

        // Copy packed struct fields to locals for safe logging
        let cap_rate = capture_format.nSamplesPerSec;
        let cap_ch = capture_format.nChannels;
        let cap_bits = capture_format.wBitsPerSample;
        let cap_align = capture_format.nBlockAlign;
        info!("Capture format: {} Hz, {} ch, {} bits, {} block align", cap_rate, cap_ch, cap_bits, cap_align);

        let capture_event: HANDLE = CreateEventW(None, false, false, None)
            .map_err(|e| AudioError::ApiError(format!("CreateEventW: {}", e)))?;

        capture_client
            .Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                windows::Win32::Media::Audio::AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
                0,
                0,
                &capture_format as *const WAVEFORMATEX,
                None,
            )
            .map_err(|e| AudioError::ApiError(format!("Initialize capture: {}", e)))?;

        capture_client
            .SetEventHandle(capture_event)
            .map_err(|e| AudioError::ApiError(format!("SetEventHandle: {}", e)))?;

        let capture_capture: IAudioCaptureClient = capture_client
            .GetService::<IAudioCaptureClient>()
            .map_err(|e| AudioError::ApiError(format!("GetService capture: {}", e)))?;

        // Render side
        let render_client: IAudioClient = render_device
            .Activate::<IAudioClient>(CLSCTX_ALL, None)
            .map_err(|e| AudioError::ApiError(format!("Activate render IAudioClient: {}", e)))?;

        let render_format_ptr = render_client
            .GetMixFormat()
            .map_err(|e| AudioError::ApiError(format!("GetMixFormat render: {}", e)))?;
        let render_format = *render_format_ptr;

        let ren_rate = render_format.nSamplesPerSec;
        let ren_ch = render_format.nChannels;
        let ren_bits = render_format.wBitsPerSample;
        let ren_align = render_format.nBlockAlign;
        info!("Render format: {} Hz, {} ch, {} bits, {} block align", ren_rate, ren_ch, ren_bits, ren_align);

        render_client
            .Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                windows::Win32::Media::Audio::AUDCLNT_STREAMFLAGS_NOPERSIST,
                0,
                0,
                &render_format as *const WAVEFORMATEX,
                None,
            )
            .map_err(|e| AudioError::ApiError(format!("Initialize render: {}", e)))?;

        let render_buffer_size = render_client
            .GetBufferSize()
            .map_err(|e| AudioError::ApiError(format!("GetBufferSize render: {}", e)))?;

        let render_render: IAudioRenderClient = render_client
            .GetService::<IAudioRenderClient>()
            .map_err(|e| AudioError::ApiError(format!("GetService render: {}", e)))?;

        let need_resample = cap_rate != ren_rate;
        let capture_channels = cap_ch as usize;
        let render_channels = ren_ch as usize;

        if need_resample {
            warn!("Sample rate mismatch: capture={} Hz, render={} Hz. Resampling enabled.", cap_rate, ren_rate);
        }

        let sample_rate_ratio = if need_resample {
            ren_rate as f64 / cap_rate as f64
        } else {
            1.0
        };

        Ok(WasapiResources {
            capture_client,
            capture_capture,
            capture_event,
            render_client,
            render_render,
            render_buffer_size,
            capture_channels,
            render_channels,
            need_resample,
            sample_rate_ratio,
        })
    }
}

fn passthrough_loop(
    res: WasapiResources,
    gain: Arc<AtomicU32>,
    stop: Arc<AtomicBool>,
    cmd_rx: mpsc::Receiver<BoostCommand>,
) {
    // COM init for this thread
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }

    if let Err(e) = passthrough_loop_inner(res, gain, stop, cmd_rx) {
        error!("Boost passthrough loop error: {}", e);
    }

    info!("Boost passthrough thread exiting");
}

fn passthrough_loop_inner(
    res: WasapiResources,
    gain: Arc<AtomicU32>,
    stop: Arc<AtomicBool>,
    cmd_rx: mpsc::Receiver<BoostCommand>,
) -> Result<(), AudioError> {
    unsafe {
        // Start streams
        res.capture_client
            .Start()
            .map_err(|e| AudioError::ApiError(format!("Start capture: {}", e)))?;
        res.render_client
            .Start()
            .map_err(|e| AudioError::ApiError(format!("Start render: {}", e)))?;

        info!("Boost passthrough loop running");

        loop {
            if stop.load(Ordering::SeqCst) {
                break;
            }

            // Process commands
            while let Ok(cmd) = cmd_rx.try_recv() {
                match cmd {
                    BoostCommand::SetGain(g) => {
                        debug!("Boost gain updated to {:.3}", g);
                        gain.store(f32::to_bits(g), Ordering::SeqCst);
                    }
                }
            }
            if stop.load(Ordering::SeqCst) {
                break;
            }

            // Wait for capture data (50ms timeout to check stop flag)
            WaitForSingleObject(res.capture_event, 50);

            // Read all available packets
            let mut packet_size = res.capture_capture
                .GetNextPacketSize()
                .map_err(|e| AudioError::ApiError(format!("GetNextPacketSize: {}", e)))?;

            while packet_size > 0 {
                let mut buffer_ptr: *mut u8 = std::ptr::null_mut();
                let mut num_frames = 0u32;
                let mut flags = 0u32;

                res.capture_capture
                    .GetBuffer(
                        &mut buffer_ptr,
                        &mut num_frames,
                        &mut flags,
                        None,
                        None,
                    )
                    .map_err(|e| AudioError::ApiError(format!("GetBuffer capture: {}", e)))?;

                if num_frames > 0 {
                    let gain_factor = f32::from_bits(gain.load(Ordering::Relaxed));
                    let is_silent = (flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0;

                    let capture_samples = if is_silent {
                        vec![0.0f32; num_frames as usize * res.capture_channels]
                    } else {
                        let sample_count = num_frames as usize * res.capture_channels;
                        let f32_ptr = buffer_ptr as *const f32;
                        std::slice::from_raw_parts(f32_ptr, sample_count).to_vec()
                    };

                    res.capture_capture
                        .ReleaseBuffer(num_frames)
                        .map_err(|e| AudioError::ApiError(format!("ReleaseBuffer capture: {}", e)))?;

                    let output_samples = process_audio(
                        &capture_samples,
                        res.capture_channels,
                        res.render_channels,
                        gain_factor,
                        res.need_resample,
                        res.sample_rate_ratio,
                    );

                    let output_frames = output_samples.len() / res.render_channels;

                    let padding = res.render_client
                        .GetCurrentPadding()
                        .map_err(|e| AudioError::ApiError(format!("GetCurrentPadding: {}", e)))?;
                    let available = (res.render_buffer_size - padding) as usize;
                    let frames_to_write = output_frames.min(available);

                    if frames_to_write > 0 {
                        let render_buf = res.render_render
                            .GetBuffer(frames_to_write as u32)
                            .map_err(|e| AudioError::ApiError(format!("GetBuffer render: {}", e)))?;

                        let dest = std::slice::from_raw_parts_mut(
                            render_buf as *mut f32,
                            frames_to_write * res.render_channels,
                        );
                        let src = &output_samples[..frames_to_write * res.render_channels];
                        dest.copy_from_slice(src);

                        res.render_render
                            .ReleaseBuffer(frames_to_write as u32, 0)
                            .map_err(|e| AudioError::ApiError(format!("ReleaseBuffer render: {}", e)))?;
                    }
                } else {
                    res.capture_capture
                        .ReleaseBuffer(num_frames)
                        .map_err(|e| AudioError::ApiError(format!("ReleaseBuffer capture (empty): {}", e)))?;
                }

                packet_size = res.capture_capture
                    .GetNextPacketSize()
                    .map_err(|e| AudioError::ApiError(format!("GetNextPacketSize (loop): {}", e)))?;
            }
        }

        let _ = res.capture_client.Stop();
        let _ = res.render_client.Stop();
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
    // Step 1: Apply gain and clamp
    let amplified: Vec<f32> = input
        .iter()
        .map(|&s| (s * gain).clamp(-1.0, 1.0))
        .collect();

    // Step 2: Channel conversion
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

    // Step 3: Resample if needed
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
