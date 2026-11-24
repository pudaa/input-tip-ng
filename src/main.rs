//! input-tip-ng 入口：命令行分发 + 常驻进程启动。
//!
//! 命令行与常驻进程共用同一套自启逻辑，方便排障：
//! 出问题时先跑 --check-autostart 看结构化状态，再决定重装还是看日志。
//!
//! 子系统：debug 用控制台（开发时能直接看输出），release 用 GUI 子系统
//! ——常驻进程由计划任务在登录时拉起，绝不能闪一个黑框；
//! release 下从命令行调用时再 AttachConsole 到父进程，把 stdout/stderr 接回去。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod autostart;
mod config;
mod log;

use std::process::ExitCode;

use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::System::Console::SetConsoleOutputCP;

const VERSION: &str = env!("CARGO_PKG_VERSION");

const HELP: &str = "\
input-tip-ng —— InputTip 的 Rust 重写版

用法:
  input-tip-ng                       启动常驻进程
  input-tip-ng --autostart           由计划任务/注册表启动时使用（等同无参数）
  input-tip-ng --install-autostart   注册开机自启（首选计划任务，失败回退 Run 键）
  input-tip-ng --uninstall-autostart 移除全部自启项
  input-tip-ng --check-autostart     检查自启状态并自愈（退出码 0 表示健康）
  input-tip-ng --exit                通知正在运行的实例退出
  input-tip-ng --version             显示版本
  input-tip-ng --help                显示本帮助
";

fn main() -> ExitCode {
    attach_parent_console();
    // 控制台默认 GBK，直接输出 UTF-8 会乱码；无控制台时调用无害
    unsafe {
        let _ = SetConsoleOutputCP(65001);
    }

    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("");

    match cmd {
        "--help" | "-h" => {
            println!("{HELP}");
            ExitCode::SUCCESS
        }
        "--version" | "-V" => {
            println!("input-tip-ng {VERSION}");
            ExitCode::SUCCESS
        }
        "--install-autostart" => with_com(cmd_install),
        "--uninstall-autostart" => with_com(cmd_uninstall),
        "--check-autostart" => with_com(cmd_check),
        "--exit" => cmd_exit(),
        "" | "--autostart" => cmd_run(cmd == "--autostart"),
        other => {
            eprintln!("未知参数: {other}\n");
            println!("{HELP}");
            ExitCode::from(2)
        }
    }
}

/// release 是 GUI 子系统：被 cmd/PowerShell 调起时把父进程的控制台接回来，
/// 否则 println! 无处可去（stdout 句柄为空）。
#[cfg(not(debug_assertions))]
fn attach_parent_console() {
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows::Win32::System::Console::{
        ATTACH_PARENT_PROCESS, AttachConsole, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle,
    };
    use windows::core::w;

    unsafe {
        if AttachConsole(ATTACH_PARENT_PROCESS).is_err() {
            return;
        }
        if let Ok(h) = CreateFileW(
            w!("CONOUT$"),
            windows::Win32::Foundation::GENERIC_WRITE.0,
            FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            None,
        ) {
            let _ = SetStdHandle(STD_OUTPUT_HANDLE, h);
            let _ = SetStdHandle(STD_ERROR_HANDLE, h);
        }
    }
}

/// debug 下本身就是控制台子系统，无需处理
#[cfg(debug_assertions)]
fn attach_parent_console() {}

/// Task Scheduler 走 COM，需要先初始化单元；命令行工具用完即退，故集中在这里管理。
fn with_com(f: impl FnOnce() -> ExitCode) -> ExitCode {
    unsafe {
        let hr = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let should_uninit = hr.is_ok();
        let code = f();
        if should_uninit {
            CoUninitialize();
        }
        code
    }
}

fn cmd_install() -> ExitCode {
    let mut cfg = config::AppConfig::load();
    let exe = autostart::current_exe();
    let delay = cfg.autostart_delay_secs();

    match autostart::install(&exe, delay) {
        Ok(method) => {
            cfg.set_autostart(true, method.as_str());
            let _ = cfg.save();
            let how = match method {
                autostart::Method::Task => "计划任务（当前用户登录时触发，无需管理员）",
                autostart::Method::RunKey => "HKCU Run 键（计划任务不可用时的兜底）",
            };
            println!("已注册开机自启：{how}");
            log::info("autostart", &format!("已注册自启，方式={}", method.as_str()));

            let st = autostart::status(&cfg);
            println!("{}", st.describe());
            if st.healthy {
                ExitCode::SUCCESS
            } else {
                println!("警告：注册后自检未通过，请查看日志");
                ExitCode::from(1)
            }
        }
        Err(e) => {
            eprintln!("注册开机自启失败: {e}");
            log::error("autostart", &format!("注册失败: {e}"));
            ExitCode::from(1)
        }
    }
}

fn cmd_uninstall() -> ExitCode {
    let mut cfg = config::AppConfig::load();
    let method = cfg.autostart_method().to_string();
    autostart::uninstall();
    cfg.set_autostart(false, &method);
    let _ = cfg.save();
    println!("已移除开机自启（计划任务与 Run 键均已清理）");
    log::info("autostart", "已按用户请求移除自启");
    ExitCode::SUCCESS
}

fn cmd_check() -> ExitCode {
    let mut cfg = config::AppConfig::load();
    let healthy = autostart::verify_and_repair(&mut cfg);
    let st = autostart::status(&cfg);
    println!("{}", st.describe());
    if healthy {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

fn cmd_exit() -> ExitCode {
    if app::request_exit() {
        println!("已通知运行中的实例退出");
        ExitCode::SUCCESS
    } else {
        println!("没有发现运行中的实例");
        ExitCode::SUCCESS
    }
}

fn cmd_run(autostarted: bool) -> ExitCode {
    // 每次启动都做一次自检自愈：目录被移动、系统更新后任务指向失效都能自动修
    {
        let mut cfg = config::AppConfig::load();
        if cfg.autostart_enabled() {
            let _ = with_com(|| {
                let ok = autostart::verify_and_repair(&mut cfg);
                println_if_console(&format!(
                    "自启自检: {}",
                    if ok { "健康" } else { "异常，详见日志" }
                ));
                ExitCode::SUCCESS
            });
        }
    }
    let code = app::run(autostarted);
    ExitCode::from(code as u8)
}

fn println_if_console(msg: &str) {
    use windows::Win32::System::Console::GetConsoleWindow;
    let has_console = unsafe { !GetConsoleWindow().0.is_null() };
    if has_console {
        println!("{msg}");
    }
}
