//! Win32 原生 GUI + 托盘 + 后台监控(零 GUI 框架, 保持二进制极小)
//! 布局: 状态行[●状态][连接][断开][检测] / 账号 / 密码 / 线路·服务器 / 间隔·自启 / WiFi / 保存 / 日志

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    CreateBitmap, CreateDIBSection, CreateFontIndirectW, CreateSolidBrush, DeleteObject, GetDC,
    GetStockObject, HBRUSH, LOGFONTW, ReleaseDC, SetBitmapBits, SetTextColor, UpdateWindow,
    WHITE_BRUSH, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS,
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
//  状态托盘图标: 纯代码生成圆形图标(灰=检测中 / 绿=已连接 / 红=无网络 / 橙=连接中 / 蓝=其他网络)
// ------------------------------------------------------------------ //
const ICON_COLORS: [u32; 5] = [0xFF888888, 0xFF2E8B57, 0xFFC0392B, 0xFFE08A00, 0xFF2F6FDE];
// HICON=*mut c_void 非 Send/Sync, 故存 isize 再转换
static TRAY_ICONS: OnceLock<[isize; 5]> = OnceLock::new();

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
        Status::OtherNet => 4,
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
            make_status_icon(ICON_COLORS[4]),
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
    /// 其他网络在线(热点/家宽, 非校园网) —— 跳过登录
    OtherNet,
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
    /// 用户手动[断开]后暂停自动重连 —— 直到点[连接]/保存设置/检测到已在线才恢复。
    /// 否则注销后 ~20s 又被自动登录回来, 违背用户意愿
    pub manual_offline: bool,
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

/// 本地时间(年-月-日 时:分:秒)—— 之前用 UTC 有 8 小时时差; 加日期因日志跨天难分辨
fn local_time() -> String {
    use windows_sys::Win32::Foundation::SYSTEMTIME;
    use windows_sys::Win32::System::SystemInformation::GetLocalTime;
    unsafe {
        let mut st: SYSTEMTIME = std::mem::zeroed();
        GetLocalTime(&mut st);
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute, st.wSecond
        )
    }
}

fn push_log(msg: &str) {
    let line = format!("{}  {}", local_time(), msg);
    if let Some(s) = SHARED.get() {
        if let Ok(mut g) = s.lock() {
            // ⚠ Win32 EDIT 控件只认 \r\n 换行, 单 \n 会显示成方块/不换行
            g.log_text.push_str(&format!("{}\r\n", line));
            // 截断: 只保留最近约 6 万字符(约 800 行)
            if g.log_text.len() > 60_000 {
                let mut cut = g.log_text.len() - 60_000;
                // 截断点必须落在字符边界上, 否则切在 emoji 多字节中间会 panic
                while cut > 0 && !g.log_text.is_char_boundary(cut) {
                    cut -= 1;
                }
                g.log_text = g.log_text[cut..].to_string();
            }
            g.log_dirty = true;
        }
    }
    // 同步落盘: 关机注销等事件进程随关机消亡, 只有文件日志事后可查
    crate::logger::append_line(&line);
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
/// NetState → 界面状态
fn status_of(st: srun::NetState) -> Status {
    match st {
        srun::NetState::Authenticated => Status::Online,
        srun::NetState::OtherNet => Status::OtherNet,
        srun::NetState::NeedLogin | srun::NetState::NoNet => Status::Offline,
    }
}

fn worker_loop(shared: Arc<Mutex<Shared>>, rx: mpsc::Receiver<Cmd>) {
    let mut last_state: Option<srun::NetState> = None;
    let mut fail: u64 = 0;
    let mut next_check = 0u128;
    loop {
        // 1) 消费 UI 命令
        match rx.try_recv() {
            Ok(Cmd::Login) => {
                let cfg = {
                    let mut g = shared.lock().unwrap();
                    g.manual_offline = false; // 用户显式要求连接 → 解除暂停
                    g.cfg.clone()
                };
                ensure_wifi(&cfg);
                let _ = do_login(&cfg);
                // 登录后立即探测刷新状态(不等下一周期)
                let st = srun::probe(&cfg.server);
                set_status(status_of(st));
                fail = if st == srun::NetState::Authenticated { 0 } else { fail + 1 };
                // 不写 last_state: 让下一周期把「已连接」的恢复过程记进日志
            }
            Ok(Cmd::Logout) => {
                let cfg = shared.lock().unwrap().cfg.clone();
                let c = client_from_cfg(&cfg);
                match c.logout() {
                    Ok(e) => push_log(&format!("已注销 ({})", e)),
                    Err(e) => push_log(&format!("注销异常: {}", e)),
                }
                // 手动断开 = 用户不想联网: 暂停自动重连, 点[连接]或保存设置才恢复
                shared.lock().unwrap().manual_offline = true;
                push_log("⏸ 已暂停自动重连(点击[连接]或保存设置恢复)");
                set_status(Status::Offline);
                last_state = None;
            }
            Ok(Cmd::Check) => {
                let cfg = shared.lock().unwrap().cfg.clone();
                let st = srun::probe(&cfg.server);
                set_status(status_of(st));
                push_log(match st {
                    srun::NetState::Authenticated => "当前: 在线(校园网已认证)",
                    srun::NetState::NeedLogin => "当前: 校园网未认证",
                    srun::NetState::OtherNet => "当前: 其他网络在线(非校园网)",
                    srun::NetState::NoNet => "当前: 无网络",
                });
                last_state = Some(st);
            }
            Ok(Cmd::Reload) => {
                let mut g = shared.lock().unwrap();
                g.cfg = config::load();
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(_) => break,
        }
        // 2) 周期探测与自动维护
        if now_ms() >= next_check {
            let (iv, cfg, manual) = {
                let g = shared.lock().unwrap();
                (g.cfg.check_interval.max(5), g.cfg.clone(), g.manual_offline)
            };
            let backoff = std::cmp::min(180, iv * (fail + 1));
            next_check = now_ms() + backoff as u128 * 1000;

            let st = srun::probe(&cfg.server);
            if last_state != Some(st) {
                push_log(match st {
                    srun::NetState::Authenticated => "✓ 校园网已连接",
                    srun::NetState::NeedLogin => "⚠ 在校园网, 尚未认证",
                    srun::NetState::OtherNet => "ℹ 其他网络已联网, 跳过校园网登录",
                    srun::NetState::NoNet => "⚠ 无网络连接",
                });
                last_state = Some(st);
            }
            match st {
                srun::NetState::Authenticated => {
                    set_status(Status::Online);
                    fail = 0;
                    if manual {
                        shared.lock().unwrap().manual_offline = false;
                        push_log("检测到已在线(可能浏览器手动登录过), 恢复自动维护");
                    }
                }
                srun::NetState::OtherNet => {
                    set_status(Status::OtherNet);
                    fail = 0;
                }
                srun::NetState::NeedLogin | srun::NetState::NoNet => {
                    if manual {
                        // 用户手动断开后: 只展示状态, 绝不自动登录
                        set_status(Status::Offline);
                    } else {
                        ensure_wifi(&cfg); // 断网先确保连上目标 WiFi(失败不阻塞: 有线用户不受影响)
                        // 门户不可达时登录注定失败(get_challenge 连的就是它), 不白发请求
                        if srun::probe(&cfg.server) == srun::NetState::NeedLogin {
                            if !do_login(&cfg) {
                                fail += 1;
                            }
                        } else {
                            fail += 1;
                        }
                        // 立即复查刷新状态(不等下一周期)
                        let st2 = srun::probe(&cfg.server);
                        set_status(status_of(st2));
                        if st2 == srun::NetState::Authenticated {
                            fail = 0;
                        }
                        // 不写 last_state: 让下一周期把状态变化记进日志(否则恢复过程静默)
                    }
                }
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
                    Status::OtherNet => (47, 111, 222),
                    Status::Offline => (192, 57, 43),
                };
                SetTextColor(wparam as _, rgb(r, g, b));
                GetStockObject(WHITE_BRUSH) as LRESULT
            } else if lparam as HWND == GetDlgItem(hwnd, ID_LOG) {
                // 只读 EDIT 走 CTLCOLORSTATIC: 淡灰底 + 深灰字, 控制台质感
                SetTextColor(wparam as _, rgb(30, 30, 30));
                log_brush() as LRESULT
            } else {
                GetStockObject(WHITE_BRUSH) as LRESULT
            }
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
            // 给异步注销线程留完成时间(局域网两跳通常 <100ms)。
            // 阻塞此处无害——已在关机时序里; 不留宽限则消息循环退出即 process::exit, 可能掐死注销
            std::thread::sleep(std::time::Duration::from_millis(500));
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

fn current_manual() -> bool {
    SHARED
        .get()
        .and_then(|s| s.lock().ok())
        .map(|g| g.manual_offline)
        .unwrap_or(false)
}

fn update_status_ui(hwnd: HWND, status: Status) {
    let text = match status {
        Status::Online => "● 已连接",
        Status::Busy => "● 连接中…",
        Status::OtherNet => "● 其他网络在线",
        Status::Offline => {
            if current_manual() {
                "● 已手动断开"
            } else {
                "● 未连接"
            }
        }
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
        let mut was_manual = false;
        if let Some(s) = SHARED.get() {
            if let Ok(mut g) = s.lock() {
                g.cfg = cfg;
                was_manual = g.manual_offline;
                // 保存设置 = 用户要它正常工作, 解除手动暂停
                g.manual_offline = false;
            }
        }
        if was_manual {
            push_log("已恢复自动重连");
        }
        // 开机自启复选框
        let chk = SendMessageW(GetDlgItem(hwnd, ID_CHK_AUTO), BM_GETCHECK, 0, 0);
        crate::autostart::set(chk as u32 == BST_CHECKED);
        push_log("✓ 设置已保存");
        push_log(&format!("开机自启: {}", if crate::autostart::enabled() { "已启用" } else { "未启用" }));
    }
}

/// 按名创建逻辑字体(启动时一次, 进程退出时由系统回收)
unsafe fn make_font(face: &str, height: i32, bold: bool) -> WPARAM {
    let mut lf: LOGFONTW = std::mem::zeroed();
    lf.lfHeight = height;
    lf.lfWeight = if bold { 700 } else { 400 }; // FW_BOLD / FW_NORMAL
    lf.lfCharSet = 1; // DEFAULT_CHARSET
    lf.lfQuality = 5; // CLEARTYPE_QUALITY
    let f: Vec<u16> = face.encode_utf16().collect();
    lf.lfFaceName[..f.len()].copy_from_slice(&f);
    CreateFontIndirectW(&lf) as WPARAM
}

/// 日志底色画刷(创建一次, 每帧返回句柄零成本)
static LOG_BRUSH: OnceLock<isize> = OnceLock::new();
fn log_brush() -> HBRUSH {
    *LOG_BRUSH.get_or_init(|| unsafe { CreateSolidBrush(rgb(246, 246, 248)) } as isize) as HBRUSH
}

fn create_controls(hwnd: HWND) {
    unsafe {
        let font = make_font("Segoe UI", -12, false); // 全局面 9pt
        let font_bold = make_font("Segoe UI", -12, true); // 状态/分组标题加粗
        let font_mono = make_font("Consolas", -13, false); // 日志等宽
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
        let st = mk("STATIC", "● 检测中…", ID_STATUS, label, 12, 10, 170, 24);
        SendMessageW(st, WM_SETFONT, font_bold, 1);
        mk("BUTTON", "连接", ID_BTN_CONN, btn | BS_PUSHBUTTON as u32, 246, 6, 60, 28);
        mk("BUTTON", "断开", ID_BTN_DISC, btn | BS_PUSHBUTTON as u32, 311, 6, 60, 28);
        mk("BUTTON", "立即检测", ID_BTN_CHECK, btn | BS_PUSHBUTTON as u32, 376, 6, 70, 28);

        // 分组 1: 账号与线路 —— 标题带占 y38~56, 内容自 y=60 起; 标签/输入统一 x=16/78 网格
        let g1 = mk("BUTTON", "账号与线路", 0, group, 8, 38, 438, 116);
        SendMessageW(g1, WM_SETFONT, font_bold, 1);
        mk("STATIC", "账号", 0, label, 16, 62, 56, 22);
        mk("EDIT", "", ID_USER, edit, 78, 60, 236, 24);
        mk("STATIC", "密码", 0, label, 16, 92, 56, 22);
        mk("EDIT", "", ID_PWD, edit | ES_PASSWORD as u32, 78, 90, 236, 24);
        mk("STATIC", "线路", 0, label, 16, 124, 56, 22);
        let combo = mk(
            "COMBOBOX",
            "",
            ID_DOMAIN,
            WS_CHILD | WS_VISIBLE | CBS_DROPDOWNLIST as u32 | WS_VSCROLL,
            78, 122, 110, 130,
        );
        for d in DOMAIN_NAMES {
            SendMessageW(combo, CB_ADDSTRING, 0, w(d).as_ptr() as LPARAM);
        }
        SendMessageW(combo, CB_SETCURSEL, 0, 0);
        mk("STATIC", "服务器", 0, label, 196, 124, 46, 22);
        mk("EDIT", "172.16.245.50", ID_SERVER, edit, 246, 122, 196, 24);

        // 分组 2: 监控与自启 —— 标题带占 y160~178, 内容自 y=182 起(修复与「间隔(秒)」重叠)
        let g2 = mk("BUTTON", "监控与自启", 0, group, 8, 160, 438, 92);
        SendMessageW(g2, WM_SETFONT, font_bold, 1);
        mk("STATIC", "间隔(秒)", 0, label, 16, 184, 56, 22);
        mk("EDIT", "20", ID_INTERVAL, edit, 78, 182, 52, 24);
        mk("BUTTON", "开机自启(登录时)", ID_CHK_AUTO, btn | BS_AUTOCHECKBOX as u32, 146, 184, 164, 22);
        mk("BUTTON", "保存设置", ID_BTN_SAVE, btn | BS_PUSHBUTTON as u32, 368, 182, 74, 26);
        mk("STATIC", "WiFi名", 0, label, 16, 214, 56, 22);
        mk("EDIT", "SWPU-EDU", ID_WIFI, edit, 78, 212, 236, 24);
        mk("STATIC", "(留空=不自动连WiFi)", 0, label, 322, 216, 118, 20);

        // 分组 3: 运行日志(等宽字体, 配淡灰底)
        let g3 = mk("BUTTON", "运行日志", 0, group, 8, 258, 438, 302);
        SendMessageW(g3, WM_SETFONT, font_bold, 1);
        let log = mk(
            "EDIT",
            "",
            ID_LOG,
            WS_CHILD | WS_VISIBLE | WS_VSCROLL | ES_MULTILINE as u32 | ES_READONLY as u32,
            20, 280, 414, 270,
        );
        SendMessageW(log, WM_SETFONT, font_mono, 1);
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
        manual_offline: false,
    }));
    let _ = SHARED.set(shared.clone());
    push_log("程序已启动, 后台监控运行中");

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
