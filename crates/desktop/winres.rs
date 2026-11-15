// Windows 資源（桌面版與 CLI 的 build.rs 用 include! 共用）：圖示、版本資訊。
// 應用程式資訊清單不用另外放：GNU 工具鏈已經會連結預設的（asInvoker、支援的作業系統），再放一份會衝突。
//
// 有版本資訊（產品名稱、說明、版本、來源網址）與圖示的執行檔，看起來比較像正常的軟體，
// 一般認為比較不會被防毒軟體的機器學習模型誤判（例如 Windows Defender 的 `Trojan:Win32/Bearfoos.A!ml`）。
// 圖示用 icon_shape.rs 畫（跟視窗、系統匣同一份），不需要圖檔。
// 需要 windres（GNU 工具鏈附的；這台電腦在 WinLibs 裡）。呼叫端要先宣告 `mod icon_shape`。

#[allow(dead_code)]
fn embed_windows_resources(file_name: &str, description: &str) {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let version = std::env::var("CARGO_PKG_VERSION").unwrap();
    let nums: Vec<u32> = version.split(['.', '-', '+']).take(3).map(|p| p.parse().unwrap_or(0)).collect();
    let (a, b, c) = (nums.first().copied().unwrap_or(0), nums.get(1).copied().unwrap_or(0), nums.get(2).copied().unwrap_or(0));

    // 圖示：多個尺寸的 32 位元 BMP（不需要 PNG 編碼器）
    let ico = out.join("app.ico");
    std::fs::write(&ico, make_ico(&[16, 20, 24, 32, 40, 48, 64, 256])).unwrap();

    let path = |p: &std::path::Path| p.display().to_string().replace('\\', "/");
    let rc = out.join("app.rc");
    std::fs::write(
        &rc,
        format!(
            r#"#pragma code_page(65001)
1 ICON "{ico}"
1 VERSIONINFO
FILEVERSION {a},{b},{c},0
PRODUCTVERSION {a},{b},{c},0
FILEFLAGSMASK 0x3fL
FILEFLAGS 0x0L
FILEOS 0x40004L
FILETYPE 0x1L
FILESUBTYPE 0x0L
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040404b0"
    BEGIN
      VALUE "CompanyName", "XiaoYu0708"
      VALUE "FileDescription", "{description}"
      VALUE "FileVersion", "{version}"
      VALUE "InternalName", "{stem}"
      VALUE "LegalCopyright", "XiaoYu0708"
      VALUE "OriginalFilename", "{file_name}"
      VALUE "ProductName", "MultiVoice"
      VALUE "ProductVersion", "{version}"
      VALUE "Comments", "https://github.com/XiaoYu0708/MultiVoice"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x0404, 1200
  END
END
"#,
            ico = path(&ico),
            stem = file_name.trim_end_matches(".exe"),
        ),
    )
    .unwrap();
    embed_resource::compile(&rc, embed_resource::NONE).manifest_required().unwrap();
}

/// 多尺寸 .ico（每個尺寸一張 32 位元 BGRA 的 DIB，加上全 0 的 AND 遮罩）
fn make_ico(sizes: &[usize]) -> Vec<u8> {
    let images: Vec<Vec<u8>> = sizes
        .iter()
        .map(|&n| {
            let rgba = icon_shape::icon_rgba(n, [66, 120, 220], false);
            let mask_row = n.div_ceil(32) * 4;
            let mut d = Vec::new();
            for v in [40u32, n as u32, (n * 2) as u32] {
                d.extend_from_slice(&v.to_le_bytes());
            }
            d.extend_from_slice(&1u16.to_le_bytes()); // planes
            d.extend_from_slice(&32u16.to_le_bytes()); // bit count
            d.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
            d.extend_from_slice(&((n * n * 4 + mask_row * n) as u32).to_le_bytes());
            d.extend_from_slice(&[0u8; 16]); // 解析度、調色盤
            // 由下往上
            for y in (0..n).rev() {
                for x in 0..n {
                    let p = &rgba[(y * n + x) * 4..][..4];
                    d.extend_from_slice(&[p[2], p[1], p[0], p[3]]);
                }
            }
            d.extend(std::iter::repeat_n(0u8, mask_row * n));
            d
        })
        .collect();
    let mut ico = Vec::new();
    ico.extend_from_slice(&[0, 0, 1, 0]);
    ico.extend_from_slice(&(sizes.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * sizes.len();
    for (&n, img) in sizes.iter().zip(&images) {
        let b = if n >= 256 { 0 } else { n as u8 };
        ico.extend_from_slice(&[b, b, 0, 0]);
        ico.extend_from_slice(&1u16.to_le_bytes());
        ico.extend_from_slice(&32u16.to_le_bytes());
        ico.extend_from_slice(&(img.len() as u32).to_le_bytes());
        ico.extend_from_slice(&(offset as u32).to_le_bytes());
        offset += img.len();
    }
    for img in images {
        ico.extend_from_slice(&img);
    }
    ico
}
