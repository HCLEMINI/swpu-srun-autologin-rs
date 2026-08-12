//! Srun 认证加密原语 —— 与已验证的 Python 实现逐字节对齐:
//!  - xEncode = XXTEA (delta 0x9E3779B9)
//!  - 自定义 base64 (Srun 字母表 "LVoJPiCN2R8G90yg...")
//!  - HMAC-MD5(密码, challenge)  → "{MD5}" + hex
//!  - SHA1(chkstr)               → hex

use hmac::{Hmac, Mac};
use md5::Md5;
use sha1::{Digest, Sha1};

/// Srun 自定义 base64 字母表(与官方前端一致)
const ALPHA: &[u8] = b"LVoJPiCN2R8G90yg+hmFHuacZ1OWMnrsSTXkYpUq/3dlbfKwv6xztjI7DeBE45QA";

// ------------------------------------------------------------------ //
//  XXTEA (xEncode)
// ------------------------------------------------------------------ //
fn s_bytes(a: &[u8], append_len: bool) -> Vec<u32> {
    let mut v = Vec::with_capacity(a.len().div_ceil(4) + 1);
    let mut i = 0;
    while i < a.len() {
        let b0 = a[i] as u32;
        let b1 = if i + 1 < a.len() { a[i + 1] as u32 } else { 0 };
        let b2 = if i + 2 < a.len() { a[i + 2] as u32 } else { 0 };
        let b3 = if i + 3 < a.len() { a[i + 3] as u32 } else { 0 };
        v.push(b0 | (b1 << 8) | (b2 << 16) | (b3 << 24));
        i += 4;
    }
    if append_len {
        v.push(a.len() as u32);
    }
    v
}

fn l_bytes(a: &[u32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(a.len() * 4);
    for &x in a {
        out.push((x & 0xFF) as u8);
        out.push(((x >> 8) & 0xFF) as u8);
        out.push(((x >> 16) & 0xFF) as u8);
        out.push(((x >> 24) & 0xFF) as u8);
    }
    out
}

/// 对应 JS xEncode(str, key); 返回字节串
pub fn x_encode(text: &[u8], key: &[u8]) -> Vec<u8> {
    if text.is_empty() {
        return Vec::new();
    }
    let mut v = s_bytes(text, true);
    let mut k = s_bytes(key, false);
    if k.len() < 4 {
        k.resize(4, 0);
    }
    let n = v.len() - 1;
    let mut z = v[n];
    let mut y: u32 = 0;
    let mut d: u32 = 0;
    let delta: u32 = 0x9E37_79B9;
    let q = 6 + 52 / (n + 1);
    for _ in 0..q {
        d = d.wrapping_add(delta);
        let e = (d >> 2) & 3;
        let mut p = 0usize;
        while p < n {
            y = v[p + 1];
            let m = ((z >> 5) ^ (y << 2))
                .wrapping_add(((y >> 3) ^ (z << 4)) ^ (d ^ y))
                .wrapping_add(k[(p & 3) ^ e as usize] ^ z);
            z = v[p].wrapping_add(m);
            v[p] = z;
            p += 1;
        }
        // p == n
        y = v[0];
        let m = ((z >> 5) ^ (y << 2))
            .wrapping_add(((y >> 3) ^ (z << 4)) ^ (d ^ y))
            .wrapping_add(k[(p & 3) ^ e as usize] ^ z);
        z = v[n].wrapping_add(m);
        v[n] = z;
    }
    l_bytes(&v)
}

// ------------------------------------------------------------------ //
//  Srun 自定义 base64
// ------------------------------------------------------------------ //
pub fn srun_base64(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    let mut i = 0;
    while i + 3 <= input.len() {
        let b10 = ((input[i] as u32) << 16) | ((input[i + 1] as u32) << 8) | (input[i + 2] as u32);
        out.push(ALPHA[(b10 >> 18) as usize] as char);
        out.push(ALPHA[((b10 >> 12) & 63) as usize] as char);
        out.push(ALPHA[((b10 >> 6) & 63) as usize] as char);
        out.push(ALPHA[(b10 & 63) as usize] as char);
        i += 3;
    }
    match input.len() - i {
        2 => {
            let b10 = ((input[i] as u32) << 16) | ((input[i + 1] as u32) << 8);
            out.push(ALPHA[(b10 >> 18) as usize] as char);
            out.push(ALPHA[((b10 >> 12) & 63) as usize] as char);
            out.push(ALPHA[((b10 >> 6) & 63) as usize] as char);
            out.push('=');
        }
        1 => {
            let b10 = (input[i] as u32) << 16;
            out.push(ALPHA[(b10 >> 18) as usize] as char);
            out.push(ALPHA[((b10 >> 12) & 63) as usize] as char);
            out.push('=');
            out.push('=');
        }
        _ => {}
    }
    out
}

// ------------------------------------------------------------------ //
//  HMAC-MD5 / SHA1 (标准)
// ------------------------------------------------------------------ //
type HmacMd5 = Hmac<Md5>;

/// 对应 JS pwd = md5(password, challenge) = HMAC-MD5(key=challenge, msg=password)
pub fn hmac_md5_hex(key: &[u8], msg: &[u8]) -> String {
    let mut mac = HmacMd5::new_from_slice(key).expect("hmac-md5");
    mac.update(msg);
    hex(&mac.finalize().into_bytes())
}

/// 对应 JS chksum = sha1(chkstr)
pub fn sha1_hex(data: &[u8]) -> String {
    let mut h = Sha1::new();
    h.update(data);
    hex(&h.finalize())
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

// ------------------------------------------------------------------ //
//  URL 编码(与 JS encodeURIComponent 一致的严格编码)
// ------------------------------------------------------------------ //
pub fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

/// 当前毫秒时间戳(用于模拟 jQuery 的 JSONP 回调名)
pub fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}
