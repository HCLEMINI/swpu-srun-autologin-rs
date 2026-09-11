//! Srun 认证客户端: get_challenge → srun_portal 两步 JSONP。
//! 端点与加密均与已验证的 Python 版 / 官方前端一致。

use serde::Serialize;
use serde_json::Value;

use crate::crypto::{hmac_md5_hex, now_ms, sha1_hex, srun_base64, urlencode, x_encode};
use crate::http::http_get;

pub const ENC_VER: &str = "srun_bx1";
pub const N: &str = "200";
pub const TYPE: &str = "1";
pub const NCSI_HOST: &str = "www.msftconnecttest.com";
pub const NCSI_MAGIC: &str = "Microsoft Connect Test";

#[derive(Clone)]
pub struct SrunClient {
    pub server: String,
    pub ac_id: String,
    pub username: String,
    pub password: String,
    pub domain: String,
}

impl SrunClient {
    pub fn full_username(&self) -> String {
        format!("{}{}", self.username, self.domain)
    }

    /// 第一步: 取 challenge 与 client_ip
    pub fn get_challenge(&self) -> Result<(String, String), String> {
        let cb = format!("srun{}", now_ms());
        let path = format!(
            "/cgi-bin/get_challenge?callback={}&username={}&ip=",
            cb,
            urlencode(&self.full_username())
        );
        let body = http_get(&self.server, 80, &path, 8000)?;
        let json = parse_jsonp(&body)?;
        if json.get("error").and_then(|v| v.as_str()) != Some("ok") {
            return Err(format!("获取 challenge 失败: {}", &body[..body.len().min(160)]));
        }
        let challenge = json.get("challenge").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let ip = json.get("client_ip").and_then(|v| v.as_str()).unwrap_or("").to_string();
        Ok((challenge, ip))
    }

    /// 第二步: 登录。返回服务器 error 字段("ok" = 成功)。
    pub fn login(&self) -> Result<String, String> {
        let (token, ip) = self.get_challenge()?;
        let hmd5 = hmac_md5_hex(token.as_bytes(), self.password.as_bytes());
        let username = self.full_username();

        // info 内层 JSON 字段顺序必须为 username,password,ip,acid,enc_ver(与 JS stringify 一致)
        let info_obj = InfoObj {
            username: &username,
            password: &self.password,
            ip: &ip,
            acid: &self.ac_id,
            enc_ver: ENC_VER,
        };
        let info_json = serde_json::to_string(&info_obj).map_err(|e| e.to_string())?;
        let info_field = format!("{{SRBX1}}{}", srun_base64(&x_encode(info_json.as_bytes(), token.as_bytes())));

        let chkstr = format!(
            "{}{}{}{}{}{}{}{}{}{}{}{}{}{}",
            token, username,
            token, hmd5,
            token, self.ac_id,
            token, ip,
            token, N,
            token, TYPE,
            token, info_field
        );
        let chksum = sha1_hex(chkstr.as_bytes());

        let cb = format!("srun{}", now_ms());
        let path = format!(
            "/cgi-bin/srun_portal?callback={}&action=login&username={}&password={{MD5}}{}&ac_id={}&ip={}&chksum={}&info={}&n={}&type={}&os=Windows&name=PC&double_stack=0",
            cb,
            urlencode(&username),
            hmd5,
            urlencode(&self.ac_id),
            urlencode(&ip),
            urlencode(&chksum),
            urlencode(&info_field),
            N,
            TYPE
        );
        let body = http_get(&self.server, 80, &path, 8000)?;
        let json = parse_jsonp(&body)?;
        Ok(json
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("?")
            .to_string())
    }

    /// 注销
    pub fn logout(&self) -> Result<String, String> {
        let (token, ip) = self.get_challenge()?;
        let username = self.full_username();
        let info_obj = InfoLogout {
            username: &username,
            ip: &ip,
            acid: &self.ac_id,
            enc_ver: ENC_VER,
        };
        let info_json = serde_json::to_string(&info_obj).map_err(|e| e.to_string())?;
        let info_field = format!("{{SRBX1}}{}", srun_base64(&x_encode(info_json.as_bytes(), token.as_bytes())));
        let chkstr = format!("{}{}{}{}{}{}{}{}", token, username, token, self.ac_id, token, ip, token, info_field);
        let chksum = sha1_hex(chkstr.as_bytes());
        let cb = format!("srun{}", now_ms());
        let path = format!(
            "/cgi-bin/srun_portal?callback={}&action=logout&username={}&ac_id={}&ip={}&chksum={}&info={}&n={}&type={}",
            cb, urlencode(&username), urlencode(&self.ac_id), urlencode(&ip), urlencode(&chksum), urlencode(&info_field), N, TYPE
        );
        let body = http_get(&self.server, 80, &path, 8000)?;
        let json = parse_jsonp(&body)?;
        Ok(json.get("error").and_then(|v| v.as_str()).unwrap_or("?").to_string())
    }
}

#[derive(Serialize)]
struct InfoObj<'a> {
    username: &'a str,
    password: &'a str,
    ip: &'a str,
    acid: &'a str,
    enc_ver: &'a str,
}

#[derive(Serialize)]
struct InfoLogout<'a> {
    username: &'a str,
    ip: &'a str,
    acid: &'a str,
    enc_ver: &'a str,
}

/// 解析 JSONP 响应: callback({...}) 或纯 {...}
pub fn parse_jsonp(body: &str) -> Result<Value, String> {
    let t = body.trim();
    if t.starts_with('{') {
        return serde_json::from_str(t).map_err(|e| format!("JSON 解析失败: {}", e));
    }
    if let Some(open) = t.find('(') {
        let close = t.rfind(')').unwrap_or(t.len());
        return serde_json::from_str(&t[open + 1..close]).map_err(|e| format!("JSONP 解析失败: {}", e));
    }
    Err(format!("非 JSON/JSONP 响应: {:?}", &t[..t.len().min(100)]))
}

/// 连通性探测: Windows NCSI, 联网时返回固定字符串; 被强制门户劫持/断网时不同
pub fn is_online() -> bool {
    match http_get(NCSI_HOST, 80, "/connecttest.txt", 4000) {
        Ok(body) => body.trim() == NCSI_MAGIC,
        Err(_) => false,
    }
}

/// 网络四态: 「门户可达(在不在校园网) × NCSI(有没有互联网)」联合判定
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NetState {
    /// 校园网且已认证
    Authenticated,
    /// 在校园网但未认证(该登录)
    NeedLogin,
    /// 不在校园网但已有互联网(热点/家宽) —— 跳过登录, 不必连校园网
    OtherNet,
    /// 完全无网络
    NoNet,
}

/// 门户可达性 = 是否身处校园网。
/// 服务器是 172.16.x.x 私网地址, 出了校园就不可路由 —— 有线无线通吃, 比 SSID 判定可靠
pub fn portal_reachable(server: &str) -> bool {
    http_get(server, 80, "/", 2000).is_ok()
}

/// 一次完整探测(门户 + NCSI 两个独立请求, 各自带超时)
pub fn probe(server: &str) -> NetState {
    match (portal_reachable(server), is_online()) {
        (true, true) => NetState::Authenticated,
        (true, false) => NetState::NeedLogin,
        (false, true) => NetState::OtherNet,
        (false, false) => NetState::NoNet,
    }
}
