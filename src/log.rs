//! 日志与数据目录。
//!
//! 约束：所有可写数据只落在 %LOCALAPPDATA%\input-tip-ng\，绝不写 exe 所在目录。
//! 旧版把 config.ini 写在 exe 旁边，安装到 Program Files 或移动目录后配置与自启一起失效。

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

use windows::Win32::Foundation::SYSTEMTIME;
use windows::Win32::System::SystemInformation::GetLocalTime;

/// 数据根目录：%LOCALAPPDATA%\input-tip-ng
pub fn root_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("input-tip-ng")
}

pub fn log_dir() -> PathBuf {
    root_dir().join("logs")
}

pub fn ensure_dirs() -> std::io::Result<()> {
    fs::create_dir_all(log_dir())
}

/// 本地时间时间戳（毫秒精度）
pub fn timestamp() -> String {
    let st: SYSTEMTIME = unsafe { GetLocalTime() };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
        st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute, st.wSecond, st.wMilliseconds
    )
}

/// 写一行日志。日志失败必须静默：排障信息不能拖垮主流程。
pub fn write(scope: &str, level: &str, msg: &str) {
    let ts = timestamp();
    let day = ts.get(0..10).unwrap_or("unknown").to_string();
    let _ = ensure_dirs();
    let path = log_dir().join(format!("{scope}-{day}.log"));

    // 单文件超过 2MB 滚动一次
    if let Ok(meta) = fs::metadata(&path) {
        if meta.len() > 2 * 1024 * 1024 {
            let _ = fs::rename(&path, log_dir().join(format!("{scope}-{day}.1.log")));
        }
    }

    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "[{ts}] [{level}] {msg}");
    }
}

pub fn info(scope: &str, msg: &str) {
    write(scope, "info", msg);
}

pub fn warn(scope: &str, msg: &str) {
    write(scope, "warn", msg);
}

pub fn error(scope: &str, msg: &str) {
    write(scope, "error", msg);
}
