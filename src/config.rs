//! 配置读写 —— 与 Python 版共用同一个 config.json(字段完全兼容)

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone)]
pub struct Config {
    #[serde(default = "d_server")]
    pub server: String,
    #[serde(default = "d_ac_id")]
    pub ac_id: String,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
    #[serde(default = "d_domain")]
    pub domain: String,
    #[serde(default = "d_interval")]
    pub check_interval: u64,
}

fn d_server() -> String {
    "172.16.245.50".into()
}
fn d_ac_id() -> String {
    "1".into()
}
fn d_domain() -> String {
    "@yd".into()
}
fn d_interval() -> u64 {
    20
}

impl Default for Config {
    fn default() -> Self {
        Self {
            server: d_server(),
            ac_id: d_ac_id(),
            username: String::new(),
            password: String::new(),
            domain: d_domain(),
            check_interval: d_interval(),
        }
    }
}

/// 配置文件位置: EXE 同目录(打包/开发一致)
pub fn config_path() -> PathBuf {
    let dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."));
    dir.join("config.json")
}

pub fn load() -> Config {
    let p = config_path();
    if let Ok(s) = std::fs::read_to_string(&p) {
        if let Ok(c) = serde_json::from_str(&s) {
            return c;
        }
        eprintln!("[config] 解析失败, 使用默认配置: {}", p.display());
    }
    Config::default()
}

pub fn save(cfg: &Config) -> Result<(), String> {
    let s = serde_json::to_string_pretty(cfg).map_err(|e| e.to_string())?;
    std::fs::write(config_path(), s).map_err(|e| e.to_string())
}
