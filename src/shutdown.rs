//! 关机/注销监听 —— headless 模式无主窗口, 用隐藏消息窗口接收
//! Windows 关机/注销/重启前会给所有顶层窗口发 WM_QUERYENDSESSION / WM_ENDSESSION。
//! GUI 模式直接用主窗口 wndproc 处理, 不走这里。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

fn w(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

static HANDLER: OnceLock<Arc<Mutex<Option<Box<dyn Fn() + Send>>>>> = OnceLock::new();
static DONE: AtomicBool = AtomicBool::new(false);

/// 触发一次回调(异步线程, 只发一次)。
/// 选在 WM_QUERYENDSESSION 阶段: 此时网络必然还通(网络关闭在关机时序末尾),
/// 比 WM_ENDSESSION 早最多 5 秒; 若关机被取消, headless 周期检测会自动重新登录, 无副作用
fn fire_once() {
    if DONE.swap(true, Ordering::SeqCst) {
        return;
    }
    if let Some(h) = HANDLER.get() {
        if let Ok(mut g) = h.lock() {
            if let Some(f) = g.take() {
                std::thread::spawn(f);
            }
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        // 立即放行关机 + 提前异步注销
        WM_QUERYENDSESSION => {
            fire_once();
            1
        }
        // 兜底(正常流程 QUERY 已触发)
        WM_ENDSESSION if wparam != 0 => {
            fire_once();
            0
        }
        // wparam==0: 关机被取消 → 复位, 下次关机可再次触发
        WM_ENDSESSION => {
            DONE.store(false, Ordering::SeqCst);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// 注册关机/注销回调(只触发一次)。headless 模式使用; GUI 模式有主窗口, 无需调用
pub fn watch(f: impl Fn() + Send + 'static) {
    std::thread::spawn(move || unsafe {
        let hinst = GetModuleHandleW(std::ptr::null());
        let class = w("SrunShutdownWnd");
        let wc = WNDCLASSW {
            style: 0,
            lpfnWndProc: Some(wndproc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinst,
            hIcon: std::ptr::null_mut(),
            hCursor: std::ptr::null_mut(),
            hbrBackground: std::ptr::null_mut(),
            lpszMenuName: std::ptr::null(),
            lpszClassName: class.as_ptr(),
        };
        if RegisterClassW(&wc) == 0 {
            return;
        }
        let hwnd = CreateWindowExW(
            0,
            class.as_ptr(),
            std::ptr::null(),
            WS_POPUP, // 隐藏窗口, 不显示
            0, 0, 0, 0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            hinst,
            std::ptr::null(),
        );
        if hwnd.is_null() {
            return;
        }
        let _ = HANDLER.set(Arc::new(Mutex::new(Some(Box::new(f)))));
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    });
}
