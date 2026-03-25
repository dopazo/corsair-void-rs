use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Media::Audio::{
    eCapture, IMMDeviceCollection, IMMDeviceEnumerator, MMDeviceEnumerator, DEVICE_STATE_ACTIVE,
};
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED, STGM_READ,
};
use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;

fn main() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);

        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                .expect("Failed to create device enumerator");

        let collection: IMMDeviceCollection = enumerator
            .EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE)
            .expect("Failed to enumerate");

        let count = collection.GetCount().expect("GetCount");

        println!("=== Capture devices: volume ranges ===\n");

        for i in 0..count {
            let device = collection.Item(i).expect("Item");

            let store: IPropertyStore = device
                .OpenPropertyStore(STGM_READ)
                .expect("OpenPropertyStore");
            let name = store
                .GetValue(&PKEY_Device_FriendlyName)
                .expect("GetValue")
                .to_string();

            let volume: IAudioEndpointVolume = device
                .Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None)
                .expect("Activate");

            let mut min_db: f32 = 0.0;
            let mut max_db: f32 = 0.0;
            let mut increment_db: f32 = 0.0;
            volume
                .GetVolumeRange(&mut min_db, &mut max_db, &mut increment_db)
                .expect("GetVolumeRange");

            let current_db = volume.GetMasterVolumeLevel().expect("GetMasterVolumeLevel");
            let current_scalar = volume
                .GetMasterVolumeLevelScalar()
                .expect("GetMasterVolumeLevelScalar");

            println!("Device {}: {}", i, name);
            println!("  Range: {:.1} dB  to  {:.1} dB  (increment: {:.1} dB)", min_db, max_db, increment_db);
            println!("  Current: {:.1} dB  ({:.0}%)", current_db, current_scalar * 100.0);

            // Try setting above 0 dB to see if it works
            if name.to_lowercase().contains("corsair") {
                println!("  >>> This is the Corsair device!");
                if max_db > 0.0 {
                    println!("  >>> max_db > 0 — boost IS possible via SetMasterVolumeLevel!");
                } else {
                    println!("  >>> max_db = {:.1} — no boost above 0 dB available", max_db);
                }
            }
            println!();
        }
    }
}
