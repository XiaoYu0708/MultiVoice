//! M0：列出音訊裝置與預設格式。
//! cargo run -p audio --example devices

use cpal::traits::{DeviceTrait, HostTrait};

fn main() {
    let host = cpal::default_host();
    println!("Host: {}", host.id().name());
    let din = host.default_input_device().and_then(|d| d.id().ok()).map(|i| i.to_string());
    let dout = host.default_output_device().and_then(|d| d.id().ok()).map(|i| i.to_string());

    for dev in host.devices().expect("devices") {
        let id = dev.id().map(|i| i.to_string()).unwrap_or_default();
        let name = dev.description().map(|d| d.to_string()).unwrap_or_default();
        let mut tags = Vec::new();
        if Some(&id) == din.as_ref() { tags.push("預設輸入"); }
        if Some(&id) == dout.as_ref() { tags.push("預設輸出"); }
        println!("\n{name}  {}", if tags.is_empty() { String::new() } else { format!("[{}]", tags.join(", ")) });
        if let Ok(c) = dev.default_input_config() {
            println!("  輸入：{} ch, {} Hz, {:?}, buffer {:?}", c.channels(), c.sample_rate(), c.sample_format(), c.buffer_size());
        }
        if let Ok(c) = dev.default_output_config() {
            println!("  輸出：{} ch, {} Hz, {:?}, buffer {:?}", c.channels(), c.sample_rate(), c.sample_format(), c.buffer_size());
        }
    }
}
