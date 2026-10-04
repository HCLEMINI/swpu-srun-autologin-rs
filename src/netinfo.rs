//! 连接介质判定 —— 判定某个本机 IPv4 属于无线网卡还是有线网卡。
//! 用途: 界面/日志显示当前校园网会话实际走哪种介质(线路后缀仍由用户手动选)。
//! 原理: 门户 rad_user_info 返回会话 IP(即登录出口 IP), 与 GetAdaptersAddresses
//! 枚举的网卡地址表比对 —— 跟随系统路由的真实决定, 比"查网线插没插"可靠。

use windows_sys::Win32::NetworkManagement::IpHelper::{
    GetAdaptersAddresses, IP_ADAPTER_ADDRESSES_LH, IP_ADAPTER_UNICAST_ADDRESS_LH,
};
use windows_sys::Win32::Networking::WinSock::SOCKADDR_IN;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Medium {
    Wired,
    Wireless,
    Unknown,
}

impl Medium {
    /// 界面/日志后缀
    pub fn suffix(self) -> &'static str {
        match self {
            Medium::Wired => "(有线)",
            Medium::Wireless => "(无线)",
            Medium::Unknown => "",
        }
    }
}

const IF_TYPE_ETHERNET: u32 = 6;
const IF_TYPE_IEEE80211: u32 = 71;
const AF_INET: u32 = 2;

/// 判定 ip 属于哪类网卡(不匹配任何网卡 = Unknown, 如 VPN/虚拟适配器)
pub fn medium_of_ip(ip: &str) -> Medium {
    if ip.is_empty() {
        return Medium::Unknown;
    }
    // 第一次调用取所需缓冲长度
    let mut len: u32 = 0;
    unsafe {
        GetAdaptersAddresses(AF_INET, 0, std::ptr::null_mut(), std::ptr::null_mut(), &mut len)
    };
    if len == 0 {
        return Medium::Unknown;
    }
    let mut buf = vec![0u8; len as usize];
    let head = buf.as_mut_ptr() as *mut IP_ADAPTER_ADDRESSES_LH;
    if unsafe { GetAdaptersAddresses(AF_INET, 0, std::ptr::null_mut(), head, &mut len) } != 0 {
        return Medium::Unknown;
    }
    let mut cur = head;
    while !cur.is_null() {
        let a = unsafe { &*cur };
        let m = if a.IfType == IF_TYPE_IEEE80211 {
            Medium::Wireless
        } else if a.IfType == IF_TYPE_ETHERNET {
            Medium::Wired
        } else {
            Medium::Unknown
        };
        let mut u = a.FirstUnicastAddress;
        while !u.is_null() {
            let ua = unsafe { &*u };
            let sa = ua.Address.lpSockaddr as *const SOCKADDR_IN;
            if !sa.is_null() {
                // 网络序 → 点分十进制
                let s = unsafe { (*sa).sin_addr.S_un.S_addr };
                let ipstr = format!(
                    "{}.{}.{}.{}",
                    s & 0xFF,
                    (s >> 8) & 0xFF,
                    (s >> 16) & 0xFF,
                    (s >> 24) & 0xFF
                );
                if ipstr == ip {
                    return m;
                }
            }
            u = ua.Next;
        }
        cur = a.Next;
    }
    Medium::Unknown
}
