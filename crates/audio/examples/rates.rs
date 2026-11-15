//! 列出所有音訊裝置的預設取樣率與聲道數（排查「裝置打不開」用）。
//!
//! cargo run -p audio --example rates --release

use cpal::traits::{DeviceTrait, HostTrait};

fn main() {
    let host = cpal::default_host();
    for (label, input) in [("輸入", true), ("輸出", false)] {
        println!("== {label} ==");
        let devices = if input { host.input_devices() } else { host.output_devices() };
        for d in devices.into_iter().flatten() {
            let cfg = if input { d.default_input_config() } else { d.default_output_config() };
            match cfg {
                Ok(c) => println!("  {} Hz  {} ch  {:?}  {}", c.sample_rate(), c.channels(), c.sample_format(), audio::device::device_name(&d)),
                Err(e) => println!("  （無法取得：{e}）  {}", audio::device::device_name(&d)),
            }
        }
    }
}
