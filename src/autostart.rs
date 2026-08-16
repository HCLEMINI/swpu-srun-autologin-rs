//! 开机自启 = 任务计划(用户登录时 /SC ONLOGON, 当前用户, --minimized 进托盘)
//! 任务名 SrunAutoLogin, 创建/删除经 schtasks。
//! ⚠ 本机根目录受保护, /Create /Delete 必须提权(runas, 一次 UAC); 查询 /Query 免提权。

use std::os::windows::process::CommandExt;
use std::process::Command;

pub const TASK_NAME: &str = "SrunAutoLogin";
const NO_WINDOW: u32 = 0x0800_0000; // CREATE_NO_WINDOW

fn exe_path() -> String {
    std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default()
}

fn w(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn run_schtasks(args: &[&str]) -> bool {
    Command::new("schtasks")
        .args(args)
        .creation_flags(NO_WINDOW)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// 提权执行 schtasks(ShellExecuteExW runas, 等待 UAC 流程结束)
fn elevated_schtasks(args: &[&str]) -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
    use windows_sys::Win32::UI::Shell::{
        ShellExecuteExW, SEE_MASK_FLAG_NO_UI, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE;

    // ⚠ lpFile 已指定程序, 参数里不能再带 "schtasks"(否则执行成 "schtasks schtasks /Create ...")
    let mut cmdline = String::new();
    for a in args {
        if !cmdline.is_empty() {
            cmdline.push(' ');
        }
        cmdline.push_str(a);
    }
    unsafe {
        // ⚠ 悬垂指针陷阱: 临时 Vec<u16> 在语句结束即 drop, 必须用变量延长生命周期,
        // 否则 ShellExecuteExW 读到垃圾字节 → "Windows找不到文件'<乱码>'"
        let verb = w("runas");
        let file = w("schtasks.exe");
        let params = w(&cmdline);
        let mut sei: SHELLEXECUTEINFOW = std::mem::zeroed();
        sei.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
        sei.fMask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_FLAG_NO_UI; // 失败不弹错误框
        sei.lpVerb = verb.as_ptr();
        sei.lpFile = file.as_ptr();
        sei.lpParameters = params.as_ptr();
        sei.nShow = SW_HIDE as i32;
        if ShellExecuteExW(&mut sei) == 0 {
            return false;
        }
        let h = sei.hProcess;
        if h.is_null() {
            return false;
        }
        WaitForSingleObject(h, 60_000);
        let mut code: u32 = 0;
        GetExitCodeProcess(h, &mut code);
        CloseHandle(h);
        code == 0
    }
}

/// 查询自启是否启用(免提权)
pub fn enabled() -> bool {
    run_schtasks(&["/Query", "/TN", TASK_NAME])
}

pub fn set(enable: bool) {
    // 清理旧注册表 Run 项(旧版曾用)与旧任务名(曾叫 -Boot)
    remove_legacy_run_key();
    run_schtasks(&["/Delete", "/TN", "SrunAutoLogin-Boot", "/F"]);
    if enable {
        let tr = format!("\"{}\" --minimized", exe_path());
        let args = ["/Create", "/TN", TASK_NAME, "/TR", &tr, "/SC", "ONLOGON", "/F"];
        // 根目录创建需管理员: 先提权(一次 UAC), 失败退回普通权限(开发环境可能可写)
        if !elevated_schtasks(&args) {
            run_schtasks(&args);
        }
    } else {
        let args = ["/Delete", "/TN", TASK_NAME, "/F"];
        if !elevated_schtasks(&args) {
            run_schtasks(&args);
        }
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
