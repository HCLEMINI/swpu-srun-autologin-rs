//! 开机自启 = 任务计划(用户登录时 /SC ONLOGON, 当前用户, --minimized 进托盘)
//! 与 Python 版行为一致: 任务名 SrunAutoLogin, 创建/删除经 schtasks(需一次 UAC)

use std::os::windows::process::CommandExt;
use std::process::Command;

pub const TASK_NAME: &str = "SrunAutoLogin";
const NO_WINDOW: u32 = 0x0800_0000; // CREATE_NO_WINDOW

fn exe_path() -> String {
    std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default()
}

fn run_schtasks(args: &[&str]) -> bool {
    Command::new("schtasks")
        .args(args)
        .creation_flags(NO_WINDOW)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub fn enabled() -> bool {
    run_schtasks(&["/Query", "/TN", TASK_NAME])
}

pub fn set(enable: bool) {
    // 清理旧注册表 Run 项(旧版 Python 曾用)与旧任务名(曾叫 -Boot)
    remove_legacy_run_key();
    run_schtasks(&["/Delete", "/TN", "SrunAutoLogin-Boot", "/F"]);
    if enable {
        let tr = format!("\"{}\" --minimized", exe_path());
        run_schtasks(&["/Create", "/TN", TASK_NAME, "/TR", &tr, "/SC", "ONLOGON", "/F"]);
    } else {
        run_schtasks(&["/Delete", "/TN", TASK_NAME, "/F"]);
    }
}

/// 清理旧版注册表 HKCU\...\Run 自启项(免提权, 仅 HKCU)
fn remove_legacy_run_key() {
    let _ = Command::new("reg")
        .args([
            "delete",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
            "/v",
            "SrunAutoLogin",
            "/f",
        ])
        .creation_flags(NO_WINDOW)
        .status();
}

pub fn handle(install: bool) {
    println!(
        "已发起{}「登录时」自启任务 ({}), 如弹 UAC 请点【是】",
        if install { "创建" } else { "移除" },
        TASK_NAME
    );
    set(install);
    std::thread::sleep(std::time::Duration::from_millis(1500));
    println!("当前自启状态: {}", if enabled() { "已启用" } else { "未启用" });
}
