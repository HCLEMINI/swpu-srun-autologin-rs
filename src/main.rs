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
    let cfg = config::load();
    let st = srun::probe(&cfg.server);
    let msg = match st {
        srun::NetState::Authenticated => "在线(校园网已认证)",
        srun::NetState::NeedLogin => "离线(在校园网, 尚未认证)",
        srun::NetState::OtherNet => "在线(其他网络, 非校园网)",
        srun::NetState::NoNet => "离线(无网络)",
    };
    println!("{}", msg);
    match st {
        srun::NetState::Authenticated | srun::NetState::OtherNet => 0,
        _ => 1,
    }
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

/// headless 日志: 控制台 + 文件双写。
/// GUI 子系统下无控制台时 println 静默失败, 文件才是权威记录
fn hlog(msg: &str) {
    let line = format!("{}  {}", ts(), msg);
    println!("{}", line);
    crate::logger::append_line(&line);
}

/// 无界面服务模式: 周期探测(四态), 断连自动重连(与 Python 版 --headless 相同行为)
fn run_headless() -> ! {
    hlog("[headless] 服务模式启动");
    let cfg = config::load();
    if cfg.username.is_empty() || cfg.password.is_empty() {
        eprintln!("[headless] config.json 未配置账号/密码, 退出");
        std::process::exit(1);
    }
    let client = client_from(&cfg);
    // 关机/注销/重启前先退出登录(校园网直接断电会触发 reject, 下次开机被拒连 WiFi)
    crate::shutdown::watch(|| {
        let cfg = config::load();
        if cfg.username.is_empty() {
            return;
        }
        let client = client_from(&cfg);
        match client.logout() {
            Ok(e) => hlog(&format!("🔌 系统关机/注销, 已退出登录 ({})", e)),
            Err(e) => hlog(&format!("🔌 关机注销失败(网络可能已断开): {}", e)),
        }
    });
    let mut last_state: Option<srun::NetState> = None;
    let mut fail = 0u32;
    loop {
        let st = srun::probe(&cfg.server);
        if last_state != Some(st) {
            hlog(match st {
                srun::NetState::Authenticated => "状态: 校园网已连接",
                srun::NetState::NeedLogin => "状态: 校园网未认证, 尝试登录…",
                srun::NetState::OtherNet => "状态: 其他网络已联网, 跳过校园网登录",
                srun::NetState::NoNet => "状态: 无网络",
            });
            last_state = Some(st);
        }
        match st {
            srun::NetState::Authenticated | srun::NetState::OtherNet => fail = 0,
            srun::NetState::NeedLogin | srun::NetState::NoNet => {
                // 断网先确保连上目标 WiFi(失败不阻塞: 有线用户不受影响)
                if !cfg.wifi_ssid.trim().is_empty() {
                    match wifi::ensure(&cfg.wifi_ssid) {
                        Ok(false) => {
                            hlog(&format!("📶 WiFi 已自动连接 {}", cfg.wifi_ssid));
                            std::thread::sleep(std::time::Duration::from_secs(3)); // 等关联+DHCP
                        }
                        Ok(true) => {}
                        Err(e) => hlog(&format!("⚠ WiFi: {}", e)),
                    }
                }
                // 门户不可达时登录注定失败(get_challenge 连的就是它), 不白发请求
                if srun::probe(&cfg.server) == srun::NetState::NeedLogin {
                    match client.login() {
                        Ok(e) if e == "ok" => {
                            hlog(&format!("✓ 登录成功 ({})", cfg.domain));
                            fail = 0;
                        }
                        Ok(e) => {
                            hlog(&format!("✗ 登录失败: {}", e));
                            fail += 1;
                        }
                        Err(e) => {
                            hlog(&format!("✗ 登录异常: {}", e));
                            fail += 1;
                        }
                    }
                } else {
                    fail += 1;
                }
            }
        }
        // 退避: 连续失败时逐步拉长
        let iv = std::cmp::min(180u64, cfg.check_interval.max(5) * (fail as u64 + 1));
        std::thread::sleep(std::time::Duration::from_secs(iv));
    }
}

fn ts() -> String {
    // 本地时间(GetLocalTime), 避免 UTC 时差; 带日期: 日志跨天可分辨
    use windows_sys::Win32::Foundation::SYSTEMTIME;
    use windows_sys::Win32::System::SystemInformation::GetLocalTime;
    let mut st: SYSTEMTIME = unsafe { std::mem::zeroed() };
    unsafe { GetLocalTime(&mut st) };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute, st.wSecond
    )
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
mod logger;
mod shutdown;
