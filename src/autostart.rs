//! 开机自启 = 注册表 HKCU Run 键(免提权, 永不弹 UAC)
//! 本机限制: UAC 设为「从不通知」+ 任务计划创建被策略锁死(普通权限/提权均拒绝访问),
//! 而任务计划删除免权限 —— 任务计划方案不可用, 改走 HKCU\...\Run:
//! 登录时 Shell 启动, 带 --minimized 静默进托盘(启动比任务计划晚数秒, 无感知差异)。

use std::os::windows::process::CommandExt;
use std::process::Command;

pub const TASK_NAME: &str = "SrunAutoLogin";
const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
const NO_WINDOW: u32 = 0x0800_0000; // CREATE_NO_WINDOW

fn exe_path() -> String {
    std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default()
}

fn reg(args: &[&str]) -> bool {
    Command::new("reg")
        .args(args)
        .creation_flags(NO_WINDOW)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// 查询自启是否启用(免提权)
pub fn enabled() -> bool {
    reg(&["query", RUN_KEY, "/v", TASK_NAME])
}

pub fn set(enable: bool) {
    // 清理旧方案残留: 任务计划(本机删除免权限, 已实测可删)与旧 -Boot 任务
    run_schtasks(&["/Delete", "/TN", TASK_NAME, "/F"]);
    run_schtasks(&["/Delete", "/TN", "SrunAutoLogin-Boot", "/F"]);
    if enable {
        // ⚠ 值含空格与引号, 用参数数组直传(不经 shell 解析), reg 原样写入
        let value = format!("\"{}\" --minimized", exe_path());
        reg(&[
            "add", RUN_KEY, "/v", TASK_NAME, "/t", "REG_SZ", "/d", &value, "/f",
        ]);
    } else {
        reg(&["delete", RUN_KEY, "/v", TASK_NAME, "/f"]);
    }
}

/// 清理旧任务计划(仅删除, 免提权)
fn run_schtasks(args: &[&str]) -> bool {
    Command::new("schtasks")
        .args(args)
        .creation_flags(NO_WINDOW)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub fn handle(install: bool) {
    println!(
        "已{}「登录时」自启 (注册表 Run 键, 免提权)",
        if install { "启用" } else { "移除" }
    );
    set(install);
    println!("当前自启状态: {}", if enabled() { "已启用" } else { "未启用" });
}
