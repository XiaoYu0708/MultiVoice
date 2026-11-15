//! 編成 wasm32 時，用 clang 把 libopus（C）一起編進來；原生建置（測試）則連結 opusic-sys。
//!
//! libopus 原始碼：環境變數 `OPUS_SRC`，沒設就用 cargo registry 裡 opusic-sys 附的那份
//! （桌面版的 `opus` crate 已經會下載它；全新 clone 只建網頁版時先跑 `cargo fetch`）。
//! wasm32-unknown-unknown 沒有 C 標準函式庫：`libc/` 放最小的標頭，malloc/free 等由 Rust 提供（src/libc.rs）。

use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=OPUS_SRC");
    println!("cargo:rerun-if-changed=libc");
    let target = std::env::var("TARGET").unwrap();
    if !target.starts_with("wasm32") {
        return;
    }
    let src = opus_src();
    let mut files = Vec::new();
    for (mk, vars) in [
        ("celt_sources.mk", &["CELT_SOURCES"][..]),
        ("silk_sources.mk", &["SILK_SOURCES", "SILK_SOURCES_FLOAT"][..]),
        ("opus_sources.mk", &["OPUS_SOURCES", "OPUS_SOURCES_FLOAT"][..]),
    ] {
        let text = std::fs::read_to_string(src.join(mk)).unwrap_or_else(|e| panic!("讀不到 {mk}：{e}"));
        for v in vars {
            files.extend(mk_list(&text, v).into_iter().map(|f| src.join(f)));
        }
    }
    let libc = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("libc");
    let mut b = cc::Build::new();
    if let Some(clang) = find_llvm("clang") {
        b.compiler(clang);
    }
    if let Some(ar) = find_llvm("llvm-ar") {
        b.archiver(ar);
    }
    b.files(&files)
        .include(&libc)
        .include(src.join("include"))
        .include(src.join("celt"))
        .include(src.join("silk"))
        .include(src.join("silk/float"))
        .include(src.join("src"))
        .define("OPUS_BUILD", None)
        .define("VAR_ARRAYS", None)
        .define("HAVE_LRINTF", None)
        .define("PACKAGE_VERSION", "\"1.6.1-wasm\"")
        .flag("-ffreestanding")
        .flag("-nostdlibinc")
        .flag("-fno-builtin-malloc")
        .flag("-Wno-everything")
        .opt_level(2)
        .debug(false);
    b.compile("opus");
}

/// Makefile 變數 `NAME = a.c \ b.c …` → 檔案清單
fn mk_list(text: &str, name: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut inside = false;
    for line in text.lines() {
        let l = line.trim();
        if !inside {
            if let Some(rest) = l.strip_prefix(name)
                && let Some(rest) = rest.trim_start().strip_prefix('=')
            {
                inside = true;
                out.extend(rest.split_whitespace().filter(|w| *w != "\\").map(String::from));
                if !l.ends_with('\\') {
                    break;
                }
            }
            continue;
        }
        out.extend(l.split_whitespace().filter(|w| *w != "\\").map(String::from));
        if !l.ends_with('\\') {
            break;
        }
    }
    assert!(!out.is_empty(), "找不到 {name}");
    out
}

fn opus_src() -> PathBuf {
    if let Ok(p) = std::env::var("OPUS_SRC") {
        return PathBuf::from(p);
    }
    let home = std::env::var("CARGO_HOME").map(PathBuf::from).unwrap_or_else(|_| {
        let h = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")).expect("找不到使用者資料夾");
        Path::new(&h).join(".cargo")
    });
    let reg = home.join("registry").join("src");
    let mut found: Vec<PathBuf> = std::fs::read_dir(&reg)
        .into_iter()
        .flatten()
        .flatten()
        .flat_map(|d| std::fs::read_dir(d.path()).into_iter().flatten().flatten())
        .map(|e| e.path())
        .filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("opusic-sys-")))
        .map(|p| p.join("opus"))
        .filter(|p| p.join("opus_sources.mk").exists())
        .collect();
    found.sort();
    found.pop().expect("找不到 libopus 原始碼：先 `cargo fetch`，或設定環境變數 OPUS_SRC")
}

/// clang／llvm-ar：PATH 上有就用，否則找 LLVM 的預設安裝位置（Windows）
fn find_llvm(tool: &str) -> Option<PathBuf> {
    let exe = if cfg!(windows) { format!("{tool}.exe") } else { tool.to_string() };
    if let Some(paths) = std::env::var_os("PATH") {
        for d in std::env::split_paths(&paths) {
            if d.join(&exe).exists() {
                return Some(d.join(&exe));
            }
        }
    }
    for d in [std::env::var("LLVM_PATH").ok().map(|p| PathBuf::from(p).join("bin")), Some(PathBuf::from(r"C:\Program Files\LLVM\bin"))]
        .into_iter()
        .flatten()
    {
        if d.join(&exe).exists() {
            return Some(d.join(&exe));
        }
    }
    None
}
