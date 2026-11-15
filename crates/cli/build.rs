//! 把圖示與版本資訊編進 multivoice-cli.exe（共用桌面版的 winres.rs）

#[path = "../desktop/src/icon_shape.rs"]
#[allow(dead_code)]
mod icon_shape;

include!("../desktop/winres.rs");

fn main() {
    println!("cargo:rerun-if-changed=../desktop/src/icon_shape.rs");
    println!("cargo:rerun-if-changed=../desktop/winres.rs");
    embed_windows_resources("multivoice-cli.exe", "MultiVoice 命令列版（除錯、自動測試用）");
}
