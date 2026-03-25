use std::io::{self, Write};

use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Media::Audio::{
    eCapture, eRender, IAudioCaptureClient, IAudioClient, IAudioRenderClient,
    IMMDeviceEnumerator, MMDeviceEnumerator, AUDCLNT_BUFFERFLAGS_SILENT,
    AUDCLNT_SHAREMODE_SHARED, DEVICE_STATE_ACTIVE, WAVEFORMATEX,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED, STGM_READ,
};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;

fn main() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);

        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).expect("enumerator");

        // Find Corsair capture device
        println!("=== Looking for Corsair capture device ===");
        let capture_collection = enumerator
            .EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE)
            .expect("enum capture");
        let capture_count = capture_collection.GetCount().expect("count");

        let mut corsair_device = None;
        for i in 0..capture_count {
            let device = capture_collection.Item(i).expect("item");
            let store: IPropertyStore = device.OpenPropertyStore(STGM_READ).expect("store");
            let name = store.GetValue(&PKEY_Device_FriendlyName).expect("name").to_string();
            println!("  Capture {}: {}", i, name);
            if name.to_lowercase().contains("corsair") {
                println!("  >>> FOUND Corsair device");
                corsair_device = Some(device);
            }
        }
        let corsair_device = corsair_device.expect("Corsair mic not found!");

        // Find VB-CABLE render device
        println!("\n=== Looking for VB-CABLE render device ===");
        let render_collection = enumerator
            .EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)
            .expect("enum render");
        let render_count = render_collection.GetCount().expect("count");

        let mut cable_device = None;
        for i in 0..render_count {
            let device = render_collection.Item(i).expect("item");
            let store: IPropertyStore = device.OpenPropertyStore(STGM_READ).expect("store");
            let name = store.GetValue(&PKEY_Device_FriendlyName).expect("name").to_string();
            println!("  Render {}: {}", i, name);
            if name.to_lowercase().contains("cable input") {
                println!("  >>> FOUND VB-CABLE");
                cable_device = Some(device);
            }
        }
        let cable_device = cable_device.expect("VB-CABLE not found!");

        // Init capture — use the format pointer directly (WAVEFORMATEXTENSIBLE)
        println!("\n=== Initializing WASAPI capture ===");
        let capture_client: IAudioClient = corsair_device
            .Activate::<IAudioClient>(CLSCTX_ALL, None)
            .expect("activate capture");

        let capture_format_ptr: *mut WAVEFORMATEX = capture_client.GetMixFormat().expect("mix format capture");
        let cap_rate = (*capture_format_ptr).nSamplesPerSec;
        let cap_ch = (*capture_format_ptr).nChannels;
        let cap_bits = (*capture_format_ptr).wBitsPerSample;
        let cap_align = (*capture_format_ptr).nBlockAlign;
        let cap_tag = (*capture_format_ptr).wFormatTag;
        let cap_cb_size = (*capture_format_ptr).cbSize;
        println!("  Format: {} Hz, {} ch, {} bits, align={}, tag={}, cbSize={}",
            cap_rate, cap_ch, cap_bits, cap_align, cap_tag, cap_cb_size);

        let capture_event: HANDLE = CreateEventW(None, false, false, None).expect("event");

        // Pass the original pointer — preserves WAVEFORMATEXTENSIBLE data
        capture_client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            windows::Win32::Media::Audio::AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
            0, 0,
            capture_format_ptr,
            None,
        ).expect("init capture");
        println!("  Capture initialized OK");

        capture_client.SetEventHandle(capture_event).expect("set event");
        let capture_capture: IAudioCaptureClient = capture_client
            .GetService::<IAudioCaptureClient>()
            .expect("get capture service");

        let capture_buf_size = capture_client.GetBufferSize().expect("capture buf size");
        println!("  Capture buffer size: {} frames", capture_buf_size);

        // Init render
        println!("\n=== Initializing WASAPI render ===");
        let render_client: IAudioClient = cable_device
            .Activate::<IAudioClient>(CLSCTX_ALL, None)
            .expect("activate render");

        let render_format_ptr: *mut WAVEFORMATEX = render_client.GetMixFormat().expect("mix format render");
        let ren_rate = (*render_format_ptr).nSamplesPerSec;
        let ren_ch = (*render_format_ptr).nChannels;
        let ren_bits = (*render_format_ptr).wBitsPerSample;
        let ren_align = (*render_format_ptr).nBlockAlign;
        let ren_tag = (*render_format_ptr).wFormatTag;
        let ren_cb_size = (*render_format_ptr).cbSize;
        println!("  Format: {} Hz, {} ch, {} bits, align={}, tag={}, cbSize={}",
            ren_rate, ren_ch, ren_bits, ren_align, ren_tag, ren_cb_size);

        render_client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            windows::Win32::Media::Audio::AUDCLNT_STREAMFLAGS_NOPERSIST,
            0, 0,
            render_format_ptr,
            None,
        ).expect("init render");
        println!("  Render initialized OK");

        let render_buf_size = render_client.GetBufferSize().expect("render buf size");
        let render_render: IAudioRenderClient = render_client
            .GetService::<IAudioRenderClient>()
            .expect("get render service");
        println!("  Render buffer size: {} frames", render_buf_size);

        let cap_channels = cap_ch as usize;
        let ren_channels = ren_ch as usize;

        // Start
        println!("\n=== Starting passthrough (gain = +5 dB = x1.778) ===");
        println!("Speak into your Corsair mic. Discord should detect audio on CABLE Output.");
        println!("Running for 30 seconds...\n");

        capture_client.Start().expect("start capture");
        render_client.Start().expect("start render");

        let gain: f32 = 10.0; // +20 dB for testing
        let mut total_frames: u64 = 0;
        let mut total_packets: u64 = 0;
        let start_time = std::time::Instant::now();

        loop {
            WaitForSingleObject(capture_event, 100);

            let mut packet_size = capture_capture.GetNextPacketSize().expect("packet size");

            while packet_size > 0 {
                let mut buffer_ptr: *mut u8 = std::ptr::null_mut();
                let mut num_frames = 0u32;
                let mut flags = 0u32;

                capture_capture.GetBuffer(
                    &mut buffer_ptr, &mut num_frames, &mut flags, None, None,
                ).expect("get capture buffer");

                if num_frames > 0 {
                    let is_silent = (flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0;
                    let sample_count = num_frames as usize * cap_channels;

                    let samples: Vec<f32> = if is_silent {
                        vec![0.0; sample_count]
                    } else {
                        let f32_ptr = buffer_ptr as *const f32;
                        std::slice::from_raw_parts(f32_ptr, sample_count).to_vec()
                    };

                    capture_capture.ReleaseBuffer(num_frames).expect("release capture");

                    // Apply gain
                    let amplified: Vec<f32> = samples.iter()
                        .map(|&s| (s * gain).clamp(-1.0, 1.0))
                        .collect();

                    // Channel conversion
                    let output: Vec<f32> = if cap_channels == ren_channels {
                        amplified
                    } else if cap_channels == 1 && ren_channels == 2 {
                        amplified.iter().flat_map(|&s| [s, s]).collect()
                    } else if cap_channels == 2 && ren_channels == 1 {
                        amplified.chunks(2).map(|p| (p[0] + p[1]) * 0.5).collect()
                    } else {
                        amplified
                    };

                    let out_frames = output.len() / ren_channels;
                    let padding = render_client.GetCurrentPadding().expect("padding");
                    let available = (render_buf_size - padding) as usize;
                    let to_write = out_frames.min(available);

                    if to_write > 0 {
                        let render_buf = render_render.GetBuffer(to_write as u32).expect("get render buf");
                        let dest = std::slice::from_raw_parts_mut(render_buf as *mut f32, to_write * ren_channels);
                        dest.copy_from_slice(&output[..to_write * ren_channels]);
                        render_render.ReleaseBuffer(to_write as u32, 0).expect("release render");
                    }

                    total_frames += num_frames as u64;
                    total_packets += 1;

                    if total_packets % 50 == 0 {
                        let in_peak = samples.iter().map(|s| s.abs()).fold(0.0f32, f32::max);
                        let out_peak = output.iter().map(|s| s.abs()).fold(0.0f32, f32::max);
                        let elapsed = start_time.elapsed().as_secs_f32();
                        print!("\r  [{:.0}s] in_peak={:.4}, out_peak={:.4}, wrote={}    ",
                            elapsed, in_peak, out_peak, to_write);
                        io::stdout().flush().unwrap();
                    }
                } else {
                    capture_capture.ReleaseBuffer(0).expect("release empty");
                }

                packet_size = capture_capture.GetNextPacketSize().expect("next packet");
            }

            if start_time.elapsed().as_secs() > 30 {
                println!("\n\n30 seconds done.");
                break;
            }
        }

        let _ = capture_client.Stop();
        let _ = render_client.Stop();
        println!("Total: {} frames, {} packets", total_frames, total_packets);
    }
}
