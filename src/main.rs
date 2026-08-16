//! 校园网自动登录 (Rust 版)
//! 用法: srun [--check] [--login] [--logout] [--headless] [--install] [--uninstall]
//! 默认: 启动 GUI

// 关键: GUI 子系统, 双击不弹终端窗口(从 cmd 运行输出仍正常)
#![windows_subsystem = "windows"]

mod config;
mod crypto;
mod http;
mod srun;
mod wifi;

use config::Config;
use srun::SrunClient;

fn client_from(cfg: &Config) -> SrunClient {
    SrunClient {
        server: cfg.server.clone(),
        ac_id: cfg.ac_id.clone(),
        username: cfg.username.clone(),
        password: cfg.password.clone(),
        domain: cfg.domain.clone(),
    }
}

fn cmd_check() -> i32 {
    let on = srun::is_online();
    println!("{}", if on { "在线" } else { "离线" });
    if on { 0 } else { 1 }
}

fn cmd_login() -> i32 {
    let cfg = config::load();
    if cfg.username.is_empty() || cfg.password.is_empty() {
        eprintln!("config.json 未配置账号/密码");
        return 2;
    }
    let client = client_from(&cfg);
    match client.login() {
        Ok(e) if e == "ok" => {
            println!("✓ 登录成功 ({})", cfg.domain);
            0
        }
        Ok(e) => {
            println!("✗ 登录返回: {}", e);
            1
        }
        Err(e) => {
            println!("✗ 登录异常: {}", e);
            1
        }
    }
}

fn cmd_logout() -> i32 {
    let cfg = config::load();
    let client = client_from(&cfg);
    match client.logout() {
        Ok(e) => {
            println!("注销返回: {}", e);
            0
        }
        Err(e) => {
            println!("注销异常: {}", e);
            1
        }
    }
}

/// 无界面服务模式: 周期探活, 断连自动重连(与 Python 版 --headless 相同行为)
fn run_headless() -> ! {
    println!("[headless] 服务模式启动");
    let cfg = config::load();
    if cfg.username.is_empty() || cfg.password.is_empty() {
        eprintln!("[headless] config.json 未配置账号/密码, 退出");
        std::process::exit(1);
    }
    let client = client_from(&cfg);
    let mut last_online: Option<bool> = None;
    let mut fail = 0u32;
    loop {
        let on = srun::is_online();
        if last_online != Some(on) {
            println!("{} 状态: {}", ts(), if on { "已连接" } else { "断开, 尝试重连…" });
            last_online = Some(on);
        }
        if !on {
            // 断网先确保连上目标 WiFi(失败不阻塞: 有线用户不受影响)
            if !cfg.wifi_ssid.trim().is_empty() {
                match wifi::ensure(&cfg.wifi_ssid) {
                    Ok(false) => {
                        println!("{} 📶 WiFi 已自动连接 {}", ts(), cfg.wifi_ssid);
                        std::thread::sleep(std::time::Duration::from_secs(3)); // 等关联+DHCP
                    }
                    Ok(true) => {}
                    Err(e) => println!("{} ⚠ WiFi: {}", ts(), e),
                }
            }
            match client.login() {
                Ok(e) if e == "ok" => {
                    println!("{} ✓ 登录成功 ({})", ts(), cfg.domain);
                    fail = 0;
                    last_online = Some(true);
                }
                Ok(e) => {
                    println!("{} ✗ 登录失败: {}", ts(), e);
                    fail += 1;
                }
                Err(e) => {
                    println!("{} ✗ 登录异常: {}", ts(), e);
                    fail += 1;
                }
            }
        } else {
            fail = 0;
        }
        // 退避: 连续失败时逐步拉长
        let iv = std::cmp::min(180u64, cfg.check_interval.max(5) * (fail as u64 + 1));
        std::thread::sleep(std::time::Duration::from_secs(iv));
    }
}

fn ts() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    format!("{:02}:{:02}:{:02}", (secs / 3600) % 24, (secs / 60) % 60, secs % 60)
}

/// WiFi 调试: 无参=显示当前连接; 带参=尝试连接目标 SSID
fn cmd_wifi(ssid: Option<&str>) -> i32 {
    match wifi::current_ssid() {
        Ok(Some(s)) => println!("当前 WiFi: {s}"),
        Ok(None) => println!("当前未连接任何 WiFi"),
        Err(e) => println!("✗ {e}"),
    }
    match ssid {
        Some(target) => match wifi::ensure(target) {
            Ok(_) => {
                println!("✓ 已连接 {target}");
                0
            }
            Err(e) => {
                println!("✗ {e}");
                1
            }
        },
        None => 0,
    }
}

/// 加密自检: 用固定测试向量打印 hmd5/info/chksum, 与已验证的 Python/JS 参考值比对
fn cmd_selftest() {
    let token = "12ab56cd90ef345678901234567890ab";
    let username = "testuser@ydyx";
    let password = "TestPwd#123";
    let ip = "10.40.222.119";
    let ac_id = "1";
    let hmd5 = crypto::hmac_md5_hex(token.as_bytes(), password.as_bytes());
    let info_json = format!(
        r#"{{"username":"{}","password":"{}","ip":"{}","acid":"{}","enc_ver":"{}"}}"#,
        username, password, ip, ac_id, srun::ENC_VER
    );
    let info_field = format!(
        "{{SRBX1}}{}",
        crypto::srun_base64(&crypto::x_encode(info_json.as_bytes(), token.as_bytes()))
    );
    let chkstr = format!(
        "{}{}{}{}{}{}{}{}{}{}{}{}{}{}",
        token, username, token, hmd5, token, ac_id, token, ip,
        token, srun::N, token, srun::TYPE, token, info_field
    );
    let chksum = crypto::sha1_hex(chkstr.as_bytes());
    println!("hmd5   = {}", hmd5);
    println!("info   = {}", info_field);
    println!("chksum = {}", chksum);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--selftest") {
        cmd_selftest();
    } else if args.iter().any(|a| a == "--headless") {
        run_headless();
    } else if args.iter().any(|a| a == "--check") {
        std::process::exit(cmd_check());
    } else if args.iter().any(|a| a == "--login") {
        std::process::exit(cmd_login());
    } else if args.iter().any(|a| a == "--logout") {
        std::process::exit(cmd_logout());
    } else if let Some(i) = args.iter().position(|a| a == "--wifi") {
        std::process::exit(cmd_wifi(args.get(i + 1).map(String::as_str)));
    } else if args.iter().any(|a| a == "--install" || a == "--uninstall") {
        crate::autostart::handle(args.iter().any(|a| a == "--install"));
    } else {
        // 默认: GUI
        crate::gui::run();
    }
}

mod autostart;
mod gui;
