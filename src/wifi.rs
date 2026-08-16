//! WiFi 自动连接 —— Windows 原生 Native Wifi API(无需管理员权限)
//! 前提: 目标 SSID 需在系统中保存过配置文件(手动连接过一次即可)
//! 与 netsh wlan connect 不同, WlanConnect API 普通用户即可调用

use std::ffi::c_void;

use windows_sys::core::GUID;
use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::NetworkManagement::WiFi::*;

const WLAN_API_VERSION: u32 = 2; // WLAN_API_VERSION_2_0
const SSID_MAX: usize = 32;

fn open() -> Result<HANDLE, String> {
    unsafe {
        let mut negotiated = 0u32;
        let mut h: HANDLE = std::ptr::null_mut();
        let r = WlanOpenHandle(WLAN_API_VERSION, std::ptr::null(), &mut negotiated, &mut h);
        if r != 0 {
            Err(format!("初始化 WiFi 失败 0x{r:X}"))
        } else {
            Ok(h)
        }
    }
}

/// 第一块无线网卡的 GUID(无网卡返回 Err)
unsafe fn first_interface(h: HANDLE) -> Result<GUID, String> {
    let mut list: *mut WLAN_INTERFACE_INFO_LIST = std::ptr::null_mut();
    let r = WlanEnumInterfaces(h, std::ptr::null(), &mut list);
    if r != 0 {
        return Err(format!("枚举无线网卡失败 0x{r:X}"));
    }
    let mut guid = None;
    if !list.is_null() {
        let infos = &*list;
        if infos.dwNumberOfItems > 0 {
            guid = Some((&*infos.InterfaceInfo.as_ptr()).InterfaceGuid);
        }
        WlanFreeMemory(list as *const c_void);
    }
    guid.ok_or_else(|| "无无线网卡(WiFi 被禁用?)".to_string())
}

fn ssid_of(ssid: &DOT11_SSID) -> String {
    let len = (ssid.uSSIDLength as usize).min(SSID_MAX);
    String::from_utf8_lossy(&ssid.ucSSID[..len]).into_owned()
}

/// 定长 UTF-16 缓冲区 → String(遇 0 截断)
fn wstr(buf: &[u16]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len])
}

// ------------------------------------------------------------------ //
//  公开接口
// ------------------------------------------------------------------ //

/// 当前连接的 WiFi SSID(未连接/无网卡返回 Err)
pub fn current_ssid() -> Result<Option<String>, String> {
    let h = open()?;
    let r = unsafe { current_ssid_impl(h) };
    unsafe { WlanCloseHandle(h, std::ptr::null()); }
    r
}

unsafe fn current_ssid_impl(h: HANDLE) -> Result<Option<String>, String> {
    let guid = first_interface(h)?;
    let mut size: u32 = 0;
    let mut data: *mut c_void = std::ptr::null_mut();
    let mut vtype: WLAN_OPCODE_VALUE_TYPE = 0;
    let r = WlanQueryInterface(
        h,
        &guid,
        wlan_intf_opcode_current_connection,
        std::ptr::null(),
        &mut size,
        &mut data,
        &mut vtype,
    );
    if r != 0 {
        return Err(format!("查询连接状态失败 0x{r:X}"));
    }
    if data.is_null() {
        return Ok(None);
    }
    let attrs = &*(data as *const WLAN_CONNECTION_ATTRIBUTES);
    // 注意: 0.59 元数据里 SSID 在关联属性中, 不在顶层
    let s = ssid_of(&attrs.wlanAssociationAttributes.dot11Ssid);
    WlanFreeMemory(data);
    Ok(if s.is_empty() { None } else { Some(s) })
}

/// 连接指定 SSID(需系统已保存其配置文件)
pub fn connect(ssid: &str) -> Result<(), String> {
    let h = open()?;
    let r = unsafe { connect_impl(h, ssid) };
    unsafe { WlanCloseHandle(h, std::ptr::null()); }
    r
}

unsafe fn connect_impl(h: HANDLE, ssid: &str) -> Result<(), String> {
    let guid = first_interface(h)?;
    let profile = find_profile(h, &guid, ssid)?;
    if profile.is_empty() {
        return Err(format!(
            "{ssid} 可见但没有已保存的配置文件, 请先在系统 WiFi 设置里手动连接一次"
        ));
    }
    let mut params: WLAN_CONNECTION_PARAMETERS = std::mem::zeroed();
    params.wlanConnectionMode = wlan_connection_mode_profile;
    let wide: Vec<u16> = profile.encode_utf16().chain(std::iter::once(0)).collect();
    params.strProfile = wide.as_ptr();
    params.dot11BssType = dot11_BSS_type_infrastructure;
    params.dwFlags = 0;
    let r = WlanConnect(h, &guid, &params, std::ptr::null());
    if r != 0 {
        return Err(format!("连接失败 0x{r:X} (配置文件 '{profile}')"));
    }
    Ok(())
}

/// 主动扫描并找到目标 SSID 对应的已保存配置文件(等扫描完成, 最多 ~7s)
unsafe fn find_profile(h: HANDLE, guid: &GUID, ssid: &str) -> Result<String, String> {
    // 触发一次主动扫描(异步); 失败不致命, 直接读缓存
    let _ = WlanScan(h, guid, std::ptr::null(), std::ptr::null(), std::ptr::null());
    let mut last_err = String::new();
    for _ in 0..7 {
        std::thread::sleep(std::time::Duration::from_secs(1));
        let mut list: *mut WLAN_AVAILABLE_NETWORK_LIST = std::ptr::null_mut();
        let r = WlanGetAvailableNetworkList(h, guid, 0, std::ptr::null(), &mut list);
        if r != 0 {
            last_err = format!("0x{r:X}");
            continue;
        }
        let mut profile = String::new();
        if !list.is_null() {
            let nets = &*list;
            for i in 0..nets.dwNumberOfItems {
                let n = &*nets.Network.as_ptr().add(i as usize);
                if ssid_of(&n.dot11Ssid).eq_ignore_ascii_case(ssid) {
                    profile = wstr(&n.strProfileName);
                    break;
                }
            }
            WlanFreeMemory(list as *const c_void);
        }
        // 命中立即返回; 未命中继续等扫描完成(扫描通常 2-4s 才可见, 不能提前退出)
        if !profile.is_empty() {
            return Ok(profile);
        }
    }
    if last_err.is_empty() {
        Ok(String::new()) // 扫描完仍未发现 → 空配置文件名
    } else {
        Err(format!("查询网络列表失败 {last_err}"))
    }
}

/// 断线重连前的 WiFi 保障:
/// Ok(true)  = 已在目标网络(无需动作)
/// Ok(false) = 刚发起连接, 调用方稍等几秒(等关联+DHCP)再登录
/// Err(msg)  = 无法连接(无网卡/无配置/信号不可见), 由调用方记日志后继续(有线场景不受影响)
pub fn ensure(ssid: &str) -> Result<bool, String> {
    if ssid.trim().is_empty() {
        return Ok(true); // 未配置 → 跳过
    }
    match current_ssid() {
        Ok(Some(cur)) if cur.eq_ignore_ascii_case(ssid.trim()) => Ok(true),
        _ => {
            connect(ssid.trim())?;
            Ok(false)
        }
    }
}
