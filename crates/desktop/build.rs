//! 把圖示與版本資訊編進 multivoice.exe（見 winres.rs）

#[path = "src/icon_shape.rs"]
#[allow(dead_code)]
mod icon_shape;

include!("winres.rs");

fn main() {
    println!("cargo:rerun-if-changed=src/icon_shape.rs");
    println!("cargo:rerun-if-changed=winres.rs");
    embed_windows_resources("multivoice.exe", "MultiVoice 遊戲多頻道語音");
}
