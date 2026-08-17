//! Win32 原生 GUI + 托盘 + 后台监控(零 GUI 框架, 保持二进制极小)
//! 布局: 状态行[●状态][连接][断开][检测] / 账号 / 密码 / 线路·服务器 / 间隔·自启 / WiFi / 保存 / 日志

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    CreateBitmap, CreateDIBSection, DeleteObject, GetDC, GetStockObject, ReleaseDC, SetBitmapBits,
    SetTextColor, UpdateWindow, WHITE_BRUSH, DEFAULT_GUI_FONT, BI_RGB, BITMAPINFO, BITMAPINFOHEADER,
    DIB_RGB_COLORS,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::*;
use windows_sys::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NOTIFYICONDATAW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::config;
use crate::config::Config;
use crate::srun::{self, SrunClient};
use crate::wifi;

/// COLORREF: 0x00BBGGRR
fn rgb(r: u8, g: u8, b: u8) -> u32 {
    (r as u32) | ((g as u32) << 8) | ((b as u32) << 16)
}

// ------------------------------------------------------------------ //
//  状态托盘图标: 纯代码生成圆形图标(灰=检测中 / 绿=已连接 / 红=无网络 / 橙=连接中)
// ------------------------------------------------------------------ //
const ICON_COLORS: [u32; 4] = [0xFF888888, 0xFF2E8B57, 0xFFC0392B, 0xFFE08A00];
// HICON=*mut c_void 非 Send/Sync, 故存 isize 再转换
static TRAY_ICONS: OnceLock<[isize; 4]> = OnceLock::new();

fn tray_icon(idx: usize) -> HICON {
    TRAY_ICONS
        .get()
        .map(|a| a[idx] as HICON)
        .unwrap_or(std::ptr::null_mut())
}

fn status_icon_idx(status: Status) -> usize {
    match status {
        Status::Online => 1,
        Status::Offline => 2,
        Status::Busy => 3,
    }
}

/// 生成 32x32 ARGB 圆形图标(抗锯齿边缘, 预乘 alpha)
unsafe fn make_status_icon(color: u32) -> HICON {
    const S: i32 = 32;
    let mut bmi: BITMAPINFO = std::mem::zeroed();
    bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
    bmi.bmiHeader.biWidth = S;
    bmi.bmiHeader.biHeight = -S; // 自上而下
    bmi.bmiHeader.biPlanes = 1;
    bmi.bmiHeader.biBitCount = 32;
    bmi.bmiHeader.biCompression = BI_RGB;

    let hdc = GetDC(std::ptr::null_mut());
    let mut bits: *mut c_void = std::ptr::null_mut();
    let hbmp = CreateDIBSection(hdc, &bmi, DIB_RGB_COLORS, &mut bits, std::ptr::null_mut(), 0);
    ReleaseDC(std::ptr::null_mut(), hdc);
    if hbmp.is_null() || bits.is_null() {
        return std::ptr::null_mut();
    }

    let px = std::slice::from_raw_parts_mut(bits as *mut u32, (S * S) as usize);
    let (r_in, r_out) = (13.0f64, 14.5f64);
    let center = (S as f64 - 1.0) / 2.0;
    let (cr, cg, cb) = ((color >> 16) & 0xFF, (color >> 8) & 0xFF, color & 0xFF);
    for y in 0..S {
        for x in 0..S {
            let dx = x as f64 - center;
            let dy = y as f64 - center;
            let d = (dx * dx + dy * dy).sqrt();
            let alpha = if d <= r_in {
                1.0
            } else if d < r_out {
                1.0 - (d - r_in) / (r_out - r_in)
            } else {
                0.0
            };
            if alpha <= 0.0 {
                px[(y * S + x) as usize] = 0;
            } else {
                let a = (alpha * 255.0).round() as u32;
                // 预乘 alpha; u32 内存序 = BGRA
                px[(y * S + x) as usize] = (a << 24)
                    | ((cr * a / 255) << 16)
                    | ((cg * a / 255) << 8)
                    | (cb * a / 255);
            }
        }
    }

    // 1bpp 掩码位图(全 1)
    let hbmp_mask = CreateBitmap(S, S, 1, 1, std::ptr::null());
    let mask_buf = vec![0xFFu8; ((S * S) / 8 + 8) as usize];
    SetBitmapBits(hbmp_mask, mask_buf.len() as u32, mask_buf.as_ptr() as *const c_void);

    let mut ii: ICONINFO = std::mem::zeroed();
    ii.fIcon = 1;
    ii.hbmColor = hbmp;
    ii.hbmMask = hbmp_mask;
    let hicon = CreateIconIndirect(&ii);
    DeleteObject(hbmp);
    DeleteObject(hbmp_mask);
    hicon
}

fn init_tray_icons() {
    let icons = unsafe {
        [
            make_status_icon(ICON_COLORS[0]),
            make_status_icon(ICON_COLORS[1]),
            make_status_icon(ICON_COLORS[2]),
            make_status_icon(ICON_COLORS[3]),
        ]
    };
    let _ = TRAY_ICONS.set(icons.map(|h| h as isize));
}

fn destroy_tray_icons() {
    if let Some(icons) = TRAY_ICONS.get() {
        unsafe {
            for &i in icons {
                let h = i as HICON;
                if !h.is_null() {
                    DestroyIcon(h);
                }
            }
        }
    }
}

// ------------------------------------------------------------------ //
//  共享状态 / 命令通道
// ------------------------------------------------------------------ //
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Status {
    Online,
    Offline,
    Busy,
}

pub enum Cmd {
    Check,
    Login,
    Logout,
    Reload,
}

pub struct Shared {
    /// 完整日志文本(内存维护, 定时器用 WM_SETTEXT 整段写入只读框)
    pub log_text: String,
    pub log_dirty: bool,
    pub status: Status,
    pub cfg: Config,
}

static SHARED: OnceLock<Arc<Mutex<Shared>>> = OnceLock::new();
static CMD_TX: OnceLock<mpsc::Sender<Cmd>> = OnceLock::new();
static HWND_MAIN: AtomicIsize = AtomicIsize::new(0);

fn w(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn hwnd_main() -> HWND {
    HWND_MAIN.load(Ordering::SeqCst) as HWND
}

fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

// ------------------------------------------------------------------ //
//  控件 ID
// ------------------------------------------------------------------ //
const ID_STATUS: i32 = 100;
const ID_USER: i32 = 101;
const ID_PWD: i32 = 102;
const ID_DOMAIN: i32 = 103;
const ID_SERVER: i32 = 104;
const ID_INTERVAL: i32 = 105;
const ID_BTN_CONN: i32 = 106;
const ID_BTN_DISC: i32 = 107;
const ID_BTN_CHECK: i32 = 108;
const ID_BTN_SAVE: i32 = 109;
const ID_LOG: i32 = 110;
const ID_CHK_AUTO: i32 = 111;
const ID_WIFI: i32 = 112;

const WM_TRAY: u32 = WM_APP + 1;
const WM_TIMER_POLL: usize = 1;

const TRAY_SHOW: i32 = 2001;
const TRAY_QUIT: i32 = 2002;

const DOMAINS: [&str; 5] = ["@yd", "@ydyx", "@dxwx", "@stu", "@tch"];
const DOMAIN_NAMES: [&str; 5] = ["移动无线", "移动有线", "电信", "学生", "教师"];

// ------------------------------------------------------------------ //
//  工具
// ------------------------------------------------------------------ //
fn set_text(hwnd: HWND, s: &str) {
    unsafe { SetWindowTextW(hwnd, w(s).as_ptr()) };
}

fn get_text(hwnd: HWND) -> String {
    unsafe {
        let mut buf = vec![0u16; 1024];
        let n = GetWindowTextW(hwnd, buf.as_mut_ptr(), 1024);
        String::from_utf16_lossy(&buf[..n.max(0) as usize])
    }
}

/// 将内存中的完整日志写入只读 EDIT 框(WM_SETTEXT 对只读框可靠; EM_REPLACESEL 对 ES_READONLY 不生效)
fn flush_log(text: &str) {
    unsafe {
        let log_hwnd = GetDlgItem(hwnd_main(), ID_LOG);
        if log_hwnd.is_null() {
            return;
        }
        SendMessageW(log_hwnd, WM_SETTEXT, 0, w(text).as_ptr() as LPARAM);
        // 滚动到底部
        let len = SendMessageW(log_hwnd, WM_GETTEXTLENGTH, 0, 0);
        SendMessageW(log_hwnd, EM_SETSEL, len as WPARAM, len as LPARAM);
        SendMessageW(log_hwnd, EM_SCROLLCARET, 0, 0);
    }
}

fn client_from_cfg(cfg: &Config) -> SrunClient {
    SrunClient {
        server: cfg.server.clone(),
        ac_id: cfg.ac_id.clone(),
        username: cfg.username.clone(),
        password: cfg.password.clone(),
        domain: cfg.domain.clone(),
    }
}

fn push_log(msg: &str) {
    let ts = format!(
        "{:02}:{:02}:{:02}",
        (now_ms() / 3_600_000) % 24,
        (now_ms() / 60_000) % 60,
        (now_ms() / 1000) % 60
    );
    if let Some(s) = SHARED.get() {
        if let Ok(mut g) = s.lock() {
            // ⚠ Win32 EDIT 控件只认 \r\n 换行, 单 \n 会显示成方块/不换行
            g.log_text.push_str(&format!("{}  {}\r\n", ts, msg));
            // 截断: 只保留最近约 6 万字符(约 800 行)
            if g.log_text.len() > 60_000 {
                let cut = g.log_text.len() - 60_000;
                g.log_text = g.log_text[cut..].to_string();
            }
            g.log_dirty = true;
        }
    }
}

fn set_status(s: Status) {
    if let Some(shared) = SHARED.get() {
        if let Ok(mut g) = shared.lock() {
            g.status = s;
        }
    }
}

// ------------------------------------------------------------------ //
//  后台监控线程
// ------------------------------------------------------------------ //
fn worker_loop(shared: Arc<Mutex<Shared>>, rx: mpsc::Receiver<Cmd>) {
    let mut last_online: Option<bool> = None;
    let mut fail: u64 = 0;
    let mut next_check = 0u128;
    loop {
        // 1) 消费 UI 命令
        match rx.try_recv() {
            Ok(Cmd::Login) => {
                let cfg = shared.lock().unwrap().cfg.clone();
                ensure_wifi(&cfg);
                let _ = do_login(&cfg);
                let on = srun::is_online();
                set_status(if on { Status::Online } else { Status::Offline });
                fail = if on { 0 } else { fail + 1 };
                last_online = None;
            }
            Ok(Cmd::Logout) => {
                let cfg = shared.lock().unwrap().cfg.clone();
                let c = client_from_cfg(&cfg);
                match c.logout() {
                    Ok(e) => push_log(&format!("已注销 ({})", e)),
                    Err(e) => push_log(&format!("注销异常: {}", e)),
                }
                last_online = None;
            }
            Ok(Cmd::Check) => {
                let on = srun::is_online();
                set_status(if on { Status::Online } else { Status::Offline });
                push_log(&format!("当前: {}", if on { "在线" } else { "离线" }));
                last_online = None;
            }
            Ok(Cmd::Reload) => {
                let mut g = shared.lock().unwrap();
                g.cfg = config::load();
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(_) => break,
        }
        // 2) 周期断连重连
        if now_ms() >= next_check {
            let iv = {
                let g = shared.lock().unwrap();
                g.cfg.check_interval.max(5)
            };
            let backoff = std::cmp::min(180, iv * (fail + 1));
            next_check = now_ms() + backoff as u128 * 1000;

            let on = srun::is_online();
            if last_online != Some(on) {
                push_log(if on { "✓ 已连接" } else { "⚠ 检测到断线, 尝试重连…" });
                last_online = Some(on);
            }
            if !on {
                let cfg = shared.lock().unwrap().cfg.clone();
                ensure_wifi(&cfg); // 断网时先确保连上目标 WiFi(失败不阻塞: 有线用户不受影响)
                let ok = do_login(&cfg);
                fail = if ok { 0 } else { fail + 1 };
                let on2 = srun::is_online();
                set_status(if on2 { Status::Online } else { Status::Offline });
            } else {
                set_status(Status::Online);
                fail = 0;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
    }
}

/// 关机/注销时提前退出登录(异步线程, 只发一次)
static SHUTDOWN_LOGOUT_DONE: AtomicBool = AtomicBool::new(false);
fn fire_shutdown_logout() {
    if SHUTDOWN_LOGOUT_DONE.swap(true, Ordering::SeqCst) {
        return;
    }
    let cfg = SHARED
        .get()
        .and_then(|s| s.lock().ok())
        .map(|g| g.cfg.clone());
    std::thread::spawn(move || {
        let Some(cfg) = cfg else { return };
        if cfg.username.is_empty() {
            return;
        }
        push_log("🔌 检测到系统关机/注销, 退出登录中…");
        match client_from_cfg(&cfg).logout() {
            Ok(e) => push_log(&format!("已注销 ({})", e)),
            Err(e) => push_log(&format!("注销失败(网络可能已断开): {}", e)),
        }
    });
}

/// 断网重连前的 WiFi 保障: 未连接目标 SSID 则自动连接(仅当已配置 SSID)
fn ensure_wifi(cfg: &Config) {
    if cfg.wifi_ssid.trim().is_empty() {
        return;
    }
    match wifi::ensure(&cfg.wifi_ssid) {
        Ok(false) => {
            push_log(&format!("📶 WiFi 已自动连接 {}", cfg.wifi_ssid));
            std::thread::sleep(std::time::Duration::from_secs(3)); // 等关联+DHCP
        }
        Ok(true) => {}
        Err(e) => push_log(&format!("⚠ WiFi: {}", e)),
    }
}

fn do_login(cfg: &Config) -> bool {
    if cfg.username.is_empty() || cfg.password.is_empty() {
        push_log("⚠ 尚未填写账号/密码, 请填写后点[保存设置]");
        return false;
    }
    set_status(Status::Busy);
    let c = client_from_cfg(cfg);
    match c.login() {
        Ok(e) if e == "ok" => {
            push_log(&format!("✓ 登录成功 ({})", cfg.domain));
            true
        }
        Ok(e) => {
            push_log(&format!("✗ 登录失败: {}", e));
            false
        }
        Err(e) => {
            push_log(&format!("✗ 登录异常: {}", e));
            false
        }
    }
}

// ------------------------------------------------------------------ //
//  托盘
// ------------------------------------------------------------------ //
unsafe fn add_tray(hwnd: HWND) {
    let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
    nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uID = 1;
    nid.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
    nid.uCallbackMessage = WM_TRAY;
    // 初始: 灰色(检测中)
    nid.hIcon = tray_icon(0);
    if nid.hIcon.is_null() {
        nid.hIcon = LoadIconW(GetModuleHandleW(std::ptr::null()), IDI_APPLICATION);
    }
    set_tip(&mut nid, "校园网登录 · 检测中");
    Shell_NotifyIconW(NIM_ADD, &nid);
}

unsafe fn remove_tray(hwnd: HWND) {
    let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
    nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uID = 1;
    Shell_NotifyIconW(NIM_DELETE, &nid);
}

unsafe fn set_tip(nid: &mut NOTIFYICONDATAW, tip: &str) {
    let mut buf = [0u16; 128];
    let chars = tip.encode_utf16().take(127);
    for (i, c) in chars.enumerate() {
        buf[i] = c;
    }
    nid.szTip = buf;
}

unsafe fn show_menu(hwnd: HWND) {
    let menu = CreatePopupMenu();
    AppendMenuW(menu, MF_STRING, TRAY_SHOW as usize, w("显示窗口").as_ptr());
    AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
    AppendMenuW(menu, MF_STRING, TRAY_QUIT as usize, w("退出").as_ptr());
    SetForegroundWindow(hwnd);
    let mut pt: POINT = std::mem::zeroed();
    GetCursorPos(&mut pt);
    TrackPopupMenu(menu, TPM_LEFTALIGN | TPM_BOTTOMALIGN, pt.x, pt.y, 0, hwnd, std::ptr::null());
    DestroyMenu(menu);
}

// ------------------------------------------------------------------ //
//  窗口过程
// ------------------------------------------------------------------ //
unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_COMMAND => {
            let id = (wparam & 0xFFFF) as i32;
            match id {
                ID_BTN_CONN => {
                    if let Some(tx) = CMD_TX.get() {
                        let _ = tx.send(Cmd::Login);
                    }
                }
                ID_BTN_DISC => {
                    if let Some(tx) = CMD_TX.get() {
                        let _ = tx.send(Cmd::Logout);
                    }
                }
                ID_BTN_CHECK => {
                    if let Some(tx) = CMD_TX.get() {
                        let _ = tx.send(Cmd::Check);
                    }
                }
                ID_BTN_SAVE => save_settings(hwnd),
                TRAY_SHOW => show_window_from_tray(hwnd),
                TRAY_QUIT => {
                    remove_tray(hwnd);
                    DestroyWindow(hwnd);
                }
                _ => {}
            }
            0
        }
        WM_TIMER if wparam == WM_TIMER_POLL => {
            if let Some(s) = SHARED.get() {
                let (text, status) = {
                    let mut g = s.lock().unwrap();
                    if g.log_dirty {
                        g.log_dirty = false;
                        (Some(g.log_text.clone()), g.status)
                    } else {
                        (None, g.status)
                    }
                };
                if let Some(t) = text {
                    flush_log(&t);
                }
                update_status_ui(hwnd, status);
            }
            0
        }
        WM_TRAY => match lparam as u32 {
            WM_LBUTTONUP | WM_LBUTTONDBLCLK => {
                show_window_from_tray(hwnd);
                0
            }
            WM_RBUTTONUP => {
                show_menu(hwnd);
                0
            }
            _ => 0,
        },
        WM_SIZE => {
            // 最小化 → 缩回托盘
            if wparam == SIZE_MINIMIZED as WPARAM {
                ShowWindow(hwnd, SW_HIDE);
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_CTLCOLORSTATIC => {
            if lparam as HWND == GetDlgItem(hwnd, ID_STATUS) {
                let (r, g, b) = match current_status() {
                    Status::Online => (46, 139, 87),
                    Status::Busy => (224, 138, 0),
                    Status::Offline => (192, 57, 43),
                };
                SetTextColor(wparam as _, rgb(r, g, b));
            }
            GetStockObject(WHITE_BRUSH) as LRESULT
        }
        // 关机/注销/重启: 先退出登录, 避免校园网 reject(直接断电会被标记异常, 下次开机拒连 WiFi)
        // 时机: WM_QUERYENDSESSION 阶段网络必然还通(网络关闭在关机时序末尾), 立即放行 + 异步 logout,
        // 比 WM_ENDSESSION 早最多 5 秒; 即使关机被取消, 周期检测也会自动重新登录, 无副作用
        WM_QUERYENDSESSION => {
            fire_shutdown_logout();
            1
        }
        WM_ENDSESSION if wparam != 0 => {
            fire_shutdown_logout(); // 兜底(正常流程 QUERY 已触发)
            PostQuitMessage(0);
            0
        }
        // wparam==0: 关机被取消(如其他程序拒绝) → 复位, 使下次关机可再次触发注销
        WM_ENDSESSION => {
            SHUTDOWN_LOGOUT_DONE.store(false, Ordering::SeqCst);
            0
        }
        WM_CLOSE => {
            remove_tray(hwnd);
            DestroyWindow(hwnd);
            0
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

fn current_status() -> Status {
    SHARED
        .get()
        .and_then(|s| s.lock().ok())
        .map(|g| g.status)
        .unwrap_or(Status::Offline)
}

fn update_status_ui(hwnd: HWND, status: Status) {
    let text = match status {
        Status::Online => "● 已连接",
        Status::Busy => "● 连接中…",
        Status::Offline => "● 未连接",
    };
    let st = unsafe { GetDlgItem(hwnd, ID_STATUS) };
    if !st.is_null() {
        set_text(st, text);
    }
    // 托盘图标 + 提示同步(绿=已连接 / 红=无网络 / 橙=连接中)
    unsafe {
        let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = hwnd;
        nid.uID = 1;
        nid.uFlags = NIF_ICON | NIF_TIP;
        nid.hIcon = tray_icon(status_icon_idx(status));
        set_tip(&mut nid, &format!("校园网登录 · {}", text.trim_start_matches("● ")));
        Shell_NotifyIconW(NIM_MODIFY, &nid);
    }
}

fn show_window_from_tray(hwnd: HWND) {
    unsafe {
        ShowWindow(hwnd, SW_SHOW);
        ShowWindow(hwnd, SW_RESTORE);
        SetForegroundWindow(hwnd);
    }
}

fn save_settings(hwnd: HWND) {
    unsafe {
        let user = get_text(GetDlgItem(hwnd, ID_USER));
        let pwd = get_text(GetDlgItem(hwnd, ID_PWD));
        let server = get_text(GetDlgItem(hwnd, ID_SERVER));
        let interval: u64 = get_text(GetDlgItem(hwnd, ID_INTERVAL)).trim().parse().unwrap_or(20);
        let sel = SendMessageW(GetDlgItem(hwnd, ID_DOMAIN), CB_GETCURSEL, 0, 0);
        let domain = DOMAINS
            .get(sel as usize)
            .copied()
            .unwrap_or("@yd")
            .to_string();
        let wifi_ssid = get_text(GetDlgItem(hwnd, ID_WIFI)).trim().to_string();
        let cfg = Config {
            server: if server.is_empty() { "172.16.245.50".into() } else { server },
            ac_id: "1".into(),
            username: user,
            password: pwd,
            domain,
            check_interval: interval,
            wifi_ssid,
        };
        let _ = config::save(&cfg);
        if let Some(s) = SHARED.get() {
            if let Ok(mut g) = s.lock() {
                g.cfg = cfg;
            }
        }
        // 开机自启复选框
        let chk = SendMessageW(GetDlgItem(hwnd, ID_CHK_AUTO), BM_GETCHECK, 0, 0);
        crate::autostart::set(chk as u32 == BST_CHECKED);
        push_log("✓ 设置已保存");
        push_log(&format!("开机自启: {}", if crate::autostart::enabled() { "已启用" } else { "未启用" }));
    }
}

fn create_controls(hwnd: HWND) {
    unsafe {
        let font = GetStockObject(DEFAULT_GUI_FONT) as WPARAM;
        let mk = |class: &str, text: &str, id: i32, style: u32, x: i32, y: i32, ww: i32, hh: i32| {
            let h = CreateWindowExW(
                0,
                w(class).as_ptr(),
                w(text).as_ptr(),
                style,
                x, y, ww, hh,
                hwnd,
                id as *mut _,
                GetModuleHandleW(std::ptr::null()),
                std::ptr::null(),
            );
            SendMessageW(h, WM_SETFONT, font, 1);
            h
        };
        let label = WS_CHILD | WS_VISIBLE;
        let edit = WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_BORDER | ES_AUTOHSCROLL as u32;
        let btn = WS_CHILD | WS_VISIBLE | WS_TABSTOP;
        let group = btn | BS_GROUPBOX as u32;

        // 顶部: 状态 + 操作按钮
        mk("STATIC", "● 检测中…", ID_STATUS, label, 12, 10, 160, 24);
        mk("BUTTON", "连接", ID_BTN_CONN, btn | BS_PUSHBUTTON as u32, 246, 6, 60, 28);
        mk("BUTTON", "断开", ID_BTN_DISC, btn | BS_PUSHBUTTON as u32, 311, 6, 60, 28);
        mk("BUTTON", "立即检测", ID_BTN_CHECK, btn | BS_PUSHBUTTON as u32, 376, 6, 70, 28);

        // 分组 1: 账号与线路(输入框统一 x=64 对齐)
        mk("BUTTON", "账号与线路", 0, group, 8, 38, 438, 100);
        mk("STATIC", "账号", 0, label, 16, 60, 42, 22);
        mk("EDIT", "", ID_USER, edit, 64, 58, 250, 24);
        mk("STATIC", "密码", 0, label, 16, 92, 42, 22);
        mk("EDIT", "", ID_PWD, edit | ES_PASSWORD as u32, 64, 90, 250, 24);
        mk("STATIC", "线路", 0, label, 16, 124, 42, 22);
        let combo = mk(
            "COMBOBOX",
            "",
            ID_DOMAIN,
            WS_CHILD | WS_VISIBLE | CBS_DROPDOWNLIST as u32 | WS_VSCROLL,
            64, 122, 110, 120,
        );
        for d in DOMAIN_NAMES {
            SendMessageW(combo, CB_ADDSTRING, 0, w(d).as_ptr() as LPARAM);
        }
        SendMessageW(combo, CB_SETCURSEL, 0, 0);
        mk("STATIC", "服务器", 0, label, 186, 124, 42, 22);
        mk("EDIT", "172.16.245.50", ID_SERVER, edit, 232, 122, 210, 24);

        // 分组 2: 监控与自启
        mk("BUTTON", "监控与自启", 0, group, 8, 142, 438, 88);
        mk("STATIC", "间隔(秒)", 0, label, 16, 164, 52, 22);
        mk("EDIT", "20", ID_INTERVAL, edit, 74, 162, 50, 24);
        mk("BUTTON", "开机自启(登录时)", ID_CHK_AUTO, btn | BS_AUTOCHECKBOX as u32, 140, 166, 170, 20);
        mk("BUTTON", "保存设置", ID_BTN_SAVE, btn | BS_PUSHBUTTON as u32, 376, 162, 70, 26);
        mk("STATIC", "WiFi名", 0, label, 16, 196, 52, 22);
        mk("EDIT", "SWPU-EDU", ID_WIFI, edit, 74, 194, 240, 24);
        mk("STATIC", "(留空=不自动连WiFi)", 0, label, 322, 198, 110, 20);

        // 分组 3: 运行日志
        mk("BUTTON", "运行日志", 0, group, 8, 234, 438, 330);
        mk(
            "EDIT",
            "",
            ID_LOG,
            WS_CHILD | WS_VISIBLE | WS_VSCROLL | ES_MULTILINE as u32 | ES_READONLY as u32,
            20, 252, 414, 300,
        );
    }
}

// ------------------------------------------------------------------ //
//  入口
// ------------------------------------------------------------------ //
pub fn run() -> ! {
    let cfg = config::load();
    let shared = Arc::new(Mutex::new(Shared {
        log_text: String::new(),
        log_dirty: false,
        status: Status::Offline,
        cfg,
    }));
    let _ = SHARED.set(shared.clone());

    unsafe {
        let hinst = GetModuleHandleW(std::ptr::null());
        let class = w("SrunWin");
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinst,
            hIcon: LoadIconW(hinst, IDI_APPLICATION),
            hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
            hbrBackground: GetStockObject(WHITE_BRUSH),
            lpszMenuName: std::ptr::null(),
            lpszClassName: class.as_ptr(),
        };
        if RegisterClassW(&wc) == 0 {
            eprintln!("注册窗口类失败");
            std::process::exit(1);
        }
        let hwnd = CreateWindowExW(
            0,
            class.as_ptr(),
            w("校园网自动登录").as_ptr(),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT, CW_USEDEFAULT, 470, 600,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            hinst,
            std::ptr::null(),
        );
        if hwnd.is_null() {
            eprintln!("创建窗口失败");
            std::process::exit(1);
        }
        HWND_MAIN.store(hwnd as isize, Ordering::SeqCst);
        create_controls(hwnd);
        // 预填配置
        {
            let g = shared.lock().unwrap();
            let cfg = &g.cfg;
            set_text(GetDlgItem(hwnd, ID_USER), &cfg.username);
            set_text(GetDlgItem(hwnd, ID_PWD), &cfg.password);
            set_text(GetDlgItem(hwnd, ID_SERVER), &cfg.server);
            set_text(GetDlgItem(hwnd, ID_INTERVAL), &cfg.check_interval.to_string());
            set_text(GetDlgItem(hwnd, ID_WIFI), &cfg.wifi_ssid);
            let idx = DOMAINS.iter().position(|d| *d == cfg.domain).unwrap_or(0) as WPARAM;
            SendMessageW(GetDlgItem(hwnd, ID_DOMAIN), CB_SETCURSEL, idx, 0);
            let chk = if crate::autostart::enabled() { BST_CHECKED as WPARAM } else { 0 };
            SendMessageW(GetDlgItem(hwnd, ID_CHK_AUTO), BM_SETCHECK, chk, 0);
        }
        init_tray_icons();
        add_tray(hwnd);
        SetTimer(hwnd, WM_TIMER_POLL, 500, None);

        // 启动监控线程
        let (tx, rx) = mpsc::channel::<Cmd>();
        let _ = CMD_TX.set(tx);
        std::thread::spawn(move || worker_loop(shared, rx));

        // 静默启动进托盘
        if std::env::args().any(|a| a == "--minimized") {
            ShowWindow(hwnd, SW_HIDE);
        } else {
            ShowWindow(hwnd, SW_SHOW);
        }
        UpdateWindow(hwnd);

        // 消息循环
        let mut msg: MSG = std::mem::zeroed();
        loop {
            let ret = GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0);
            if ret <= 0 {
                break;
            }
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    destroy_tray_icons();
    std::process::exit(0);
}
