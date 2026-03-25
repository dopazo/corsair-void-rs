use hidapi::HidApi;

fn main() {
    let api = HidApi::new().expect("Failed to init hidapi");

    println!("=== All Corsair HID devices (VID 0x1b1c) ===\n");

    let mut found = false;
    for info in api.device_list() {
        if info.vendor_id() == 0x1b1c {
            found = true;
            println!("  PID: {:#06x}", info.product_id());
            println!("  Path: {:?}", info.path());
            println!("  Product: {:?}", info.product_string());
            println!("  Manufacturer: {:?}", info.manufacturer_string());
            println!("  Interface: {}", info.interface_number());
            println!("  Usage Page: {:#06x}", info.usage_page());
            println!("  Usage: {:#06x}", info.usage());
            println!();
        }
    }

    if !found {
        println!("No Corsair HID devices found!");
        println!("\nAll devices:");
        for info in api.device_list() {
            println!(
                "  VID={:#06x} PID={:#06x} {:?}",
                info.vendor_id(),
                info.product_id(),
                info.product_string()
            );
        }
    }
}
