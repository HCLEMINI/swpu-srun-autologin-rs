//! 关机/注销监听 —— headless 模式无主窗口, 用隐藏消息窗口接收
//! Windows 关机/注销/重启前会给所有顶层窗口发 WM_QUERYENDSESSION / WM_ENDSESSION。
//! GUI 模式直接用主窗口 wndproc 处理, 不走这里。

use std::sync::{Arc, Mutex, OnceLock};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

fn w(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

static HANDLER: OnceLock<Arc<Mutex<Option<Box<dyn Fn() + Send>>>>> = OnceLock::new();

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        // 立即放行关机(不阻塞; 注销动作放到 WM_ENDSESSION 阶段)
        WM_QUERYENDSESSION => 1,
        // 系统已决定关机/注销/重启: 执行一次回调(退出登录)
        WM_ENDSESSION if wparam != 0 => {
            if let Some(h) = HANDLER.get() {
                if let Ok(mut g) = h.lock() {
                    if let Some(f) = g.take() {
                        f();
                    }
                }
            }
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
