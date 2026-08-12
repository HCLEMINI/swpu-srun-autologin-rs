//! 极简 HTTP/1.1 GET —— 零依赖, 纯 std TcpStream。
//! 校园门户与 NCSI 探测均为明文 http, 无需 TLS, 故不引入任何 HTTP 库。

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

/// 发起 GET 请求, 返回响应体(截掉 HTTP 头)。
/// host: 域名或 IP; port: 端口; path_query: 如 "/cgi-bin/srun_portal?action=login&..."
pub fn http_get(host: &str, port: u16, path_query: &str, timeout_ms: u64) -> Result<String, String> {
    let mut stream = TcpStream::connect((host, port)).map_err(|e| format!("连接 {} 失败: {}", host, e))?;
    stream
        .set_read_timeout(Some(Duration::from_millis(timeout_ms)))
        .ok();
    stream
        .set_write_timeout(Some(Duration::from_millis(timeout_ms)))
        .ok();

    let req = format!(
        "GET {} HTTP/1.1\r\n\
         Host: {}\r\n\
         User-Agent: Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0 Safari/537.36\r\n\
         Accept: */*\r\n\
         Referer: http://{}/srun_portal_pc?ac_id=1&theme=basic\r\n\
         Connection: close\r\n\
         \r\n",
        path_query, host, host
    );
    stream
        .write_all(req.as_bytes())
        .map_err(|e| format!("发送请求失败: {}", e))?;

    let mut buf = Vec::new();
    stream
        .read_to_end(&mut buf)
        .map_err(|e| format!("读取响应失败: {}", e))?;

    let s = String::from_utf8_lossy(&buf);
    if let Some(pos) = s.find("\r\n\r\n") {
        Ok(s[pos + 4..].to_string())
    } else {
        Ok(s.to_string())
    }
}
