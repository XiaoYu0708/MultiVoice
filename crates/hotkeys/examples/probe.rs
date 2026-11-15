//! 手動測試全域按鍵：先錄一個按鍵，之後持續顯示它是否按住。
//! 切到遊戲（或其他視窗）按那個鍵，這裡應該要顯示「按住」。
//!
//! cargo run -p hotkeys --example probe --release

use std::time::Duration;

fn main() {
    if let Err(e) = hotkeys::start() {
        eprintln!("啟動失敗：{e}");
        return;
    }
    println!("監聽已啟動（{}）", if hotkeys::is_admin() { "系統管理員" } else { "一般使用者" });
    println!("請按下要測試的按鍵（可帶 Ctrl/Alt/Shift，可用滑鼠中鍵／側鍵；Esc 結束）");

    hotkeys::start_recording();
    let hk = loop {
        std::thread::sleep(Duration::from_millis(10));
        match hotkeys::take_recorded() {
            Some(hotkeys::Recorded::Key(hk)) => break hk,
            Some(hotkeys::Recorded::Cancelled) => return,
            None => {}
        }
    };
    println!("已設定：{hk}");
    println!("現在切到其他視窗或遊戲，按住／放開 {hk} 試試（按 Ctrl+C 或關閉視窗結束）");

    let mut last = None;
    let mut last_events = hotkeys::event_count();
    let mut quiet = 0u32;
    loop {
        std::thread::sleep(Duration::from_millis(10));
        let held = hk.held();
        if last != Some(held) {
            println!("{}", if held { ">>> 按住（說話中）" } else { "    放開" });
            last = Some(held);
        }
        // 5 秒都沒收到任何按鍵事件：提醒可能是權限問題
        let ev = hotkeys::event_count();
        if ev == last_events {
            quiet += 1;
            if quiet == 500 && !hotkeys::is_admin() {
                println!("（5 秒沒收到任何按鍵；若遊戲以系統管理員執行，這個程式也要以管理員身分執行才收得到）");
            }
        } else {
            quiet = 0;
            last_events = ev;
        }
    }
}
