//! 简易文件日志 —— EXE 同目录 srun_service.log(与 Python 版同名)。
//! 动机: GUI 日志只在内存里、headless 的 println 因 GUI 子系统无处可去,
//! 「关机时是否成功注销」事后无从查证 —— 落盘一行, 重启后可查。

use std::io::Write;
use std::path::{Path, PathBuf};

/// 超过 1MB 直接清空重写(校园工具, 无需严谨轮转)
fn rotate_if_huge(p: &Path) {
    if let Ok(md) = std::fs::metadata(p) {
        if md.len() > 1_000_000 {
            let _ = std::fs::write(p, "");
        }
    }
}

/// 追加一行已带时间戳的日志(失败静默: 日志绝不该影响主流程)
pub fn append_line(line: &str) {
    let p: PathBuf = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("srun_service.log");
    rotate_if_huge(&p);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&p) {
        let _ = writeln!(f, "{}", line);
    }
}
