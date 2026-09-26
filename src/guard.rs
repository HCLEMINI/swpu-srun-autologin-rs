//! 进程守护 —— 同一个 exe 以 `--watchdog <pid>` 化身为看门狗子进程。
//! 主进程异常退出(退出码 ≠ 0: 崩溃/panic abort/被强杀)时 3 秒后自动重启;
//! 正常退出(托盘退出/点X/关机注销, 退出码 0)则守护结束、看门狗随之退出。
//! 重启的实例带 --watched 标记, 不再孵化新看门狗 —— 一个会话始终只有一个守护进程。
//! 防死循环: 连续 5 次存活 <30s 的异常退出后守护放弃。

use std::os::windows::process::CommandExt;
use std::process::{Child, Command};
use std::sync::{Mutex, OnceLock};

use windows_sys::Win32::Foundation::FILETIME;
use windows_sys::Win32::System::SystemInformation::GetSystemTimeAsFileTime;
use windows_sys::Win32::System::Threading::{
    GetCurrentProcessId, GetExitCodeProcess, GetProcessTimes, OpenProcess, WaitForSingleObject,
    PROCESS_QUERY_LIMITED_INFORMATION,
};

const NO_WINDOW: u32 = 0x0800_0000; // CREATE_NO_WINDOW(与 autostart.rs 同款)
/// SYNCHRONIZE 访问权(windows-sys 把它放在 Storage::FileSystem, 不值得为此启 feature, 用字面量)
const PROC_SYNCHRONIZE: u32 = 0x0010_0000;

/// 看门狗子进程句柄(用户取消勾选时 kill 用)
static WATCHDOG: OnceLock<Mutex<Option<Child>>> = OnceLock::new();

/// 日志: 走 gui::push_log —— GUI 进程里进面板+文件; 看门狗进程/无 GUI 时仅落文件
fn glog(msg: &str) {
    crate::gui::push_log(msg);
}

/// 当前进程是否由看门狗重启而来(带 --watched 标记则不再孵化新看门狗)
pub fn is_watched() -> bool {
    std::env::args().any(|a| a == "--watched")
}

/// 孵化看门狗守护当前进程(幂等: 已有则跳过)。
/// restart_headless: 被守护进程异常退出后以何种形态复活(true=headless 服务, false=GUI 静默进托盘)
pub fn spawn(restart_headless: bool) {
    let cell = WATCHDOG.get_or_init(|| Mutex::new(None));
    let mut g = match cell.lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    if g.is_some() {
        return;
    }
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(_) => return,
    };
    let mut cmd = Command::new(exe);
    cmd.arg("--watchdog")
        .arg(unsafe { GetCurrentProcessId() }.to_string());
    if restart_headless {
        cmd.arg("--headless");
    }
    match cmd.creation_flags(NO_WINDOW).spawn() {
        Ok(c) => {
            let pid = c.id();
            *g = Some(c);
            glog(&format!("🛡 进程守护已启动 (watchdog pid={pid})"));
        }
        Err(e) => glog(&format!("⚠ 进程守护启动失败: {}", e)),
    }
}

/// 停止看门狗(用户取消勾选时立即生效)
pub fn stop() {
    if let Some(cell) = WATCHDOG.get() {
        if let Ok(mut g) = cell.lock() {
            if let Some(mut c) = g.take() {
                let _ = c.kill();
                let _ = c.wait();
                glog("🛡 进程守护已停止");
            }
        }
    }
}

/// FILETIME(100ns, 自 1601 纪元) → 秒
fn ft_secs(ft: &FILETIME) -> u64 {
    (((ft.dwHighDateTime as u64) << 32) | ft.dwLowDateTime as u64) / 10_000_000
}

fn now_secs() -> u64 {
    let mut ft: FILETIME = unsafe { std::mem::zeroed() };
    unsafe { GetSystemTimeAsFileTime(&mut ft) };
    ft_secs(&ft)
}

/// 看门狗主循环(--watchdog 模式入口)
pub fn run_watchdog(pid: u32, restart_headless: bool) -> ! {
    let mut pid = pid;
    let mut fast_crash: u32 = 0;
    loop {
        // 打开目标进程(刚 spawn 时可能未就绪, 重试 ~3s)
        let mut handle = std::ptr::null_mut();
        for _ in 0..30 {
            handle = unsafe {
                OpenProcess(PROC_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION, 0, pid)
            };
            if !handle.is_null() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        if handle.is_null() {
            glog(&format!("🛡 看门狗: 无法附加到进程 {pid}, 退出"));
            std::process::exit(0);
        }
        // 记录创建时刻 → 死亡时算存活时长(短命 = 启动即崩)
        let mut born_ft: FILETIME = unsafe { std::mem::zeroed() };
        let mut tmp: FILETIME = unsafe { std::mem::zeroed() };
        let born = unsafe {
            if GetProcessTimes(handle, &mut born_ft, &mut tmp, &mut tmp, &mut tmp) != 0 {
                ft_secs(&born_ft)
            } else {
                0
            }
        };
        // 阻塞等待终止; 关机时看门狗随会话被杀, 不会在断电后复活任何东西
        unsafe { WaitForSingleObject(handle, u32::MAX) };
        let mut code: u32 = 0;
        unsafe { GetExitCodeProcess(handle, &mut code) };
        if code == 0 {
            glog("🛡 主进程正常退出, 守护结束");
            std::process::exit(0);
        }
        let uptime = if born > 0 { now_secs().saturating_sub(born) } else { 60 };
        fast_crash = if uptime < 30 { fast_crash + 1 } else { 0 };
        if fast_crash >= 5 {
            glog(&format!(
                "🛡 连续快速异常退出 {fast_crash} 次(疑似启动即崩), 守护放弃, 详查 srun_service.log"
            ));
            std::process::exit(0);
        }
        glog(&format!("🛡 检测到异常退出 (code={code}), 3 秒后自动重启"));
        std::thread::sleep(std::time::Duration::from_secs(3));
        // 重启主进程: GUI 静默进托盘 / headless 继续 headless; 失败重试 3 次
        let exe = match std::env::current_exe() {
            Ok(p) => p,
            Err(_) => {
                glog("🛡 重启失败: 取不到自身路径, 守护退出");
                std::process::exit(1);
            }
        };
        let mut child: Option<Child> = None;
        for _ in 0..3 {
            let mut cmd = Command::new(&exe);
            cmd.arg("--watched");
            if restart_headless {
                cmd.arg("--headless");
            } else {
                cmd.arg("--minimized");
            }
            match cmd.creation_flags(NO_WINDOW).spawn() {
                Ok(c) => {
                    child = Some(c);
                    break;
                }
                Err(_) => std::thread::sleep(std::time::Duration::from_secs(2)),
            }
        }
        match child {
            Some(c) => {
                pid = c.id();
                glog(&format!("🛡 已重启主进程 (pid={pid})"));
            }
            None => {
                glog("🛡 重启连续失败, 守护退出");
                std::process::exit(1);
            }
        }
    }
}
