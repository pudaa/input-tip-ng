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
mod caret;
mod config;
mod icon;
mod ime;
mod log;
mod migrate;
mod process;
mod state;
mod symbol;
mod tray;

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
  input-tip-ng --probe               打印前台窗口的进程/状态/caret 探测详情（排障用）
  input-tip-ng --migrate <旧 config.ini>  从旧版配置迁移文字牌、白名单与运行参数
  input-tip-ng --gen-icon <输出 ico>  生成多尺寸应用图标（16~256）
  input-tip-ng --preview [CN|EN|CAPS|US|JP|KR]  在屏幕上预览一次文字牌（校色/校字号用）
  input-tip-ng --version             显示版本
  input-tip-ng --help                显示本帮助
";

fn main() -> ExitCode {
    // 必须在任何窗口创建之前：Per-Monitor V2 让坐标与像素 1:1，否则系统会把
    // 整个窗口按 DPI 拉伸，文字与圆角都会糊（旧版就吃了这个亏）。
    enable_dpi_awareness();
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
        "--probe" => with_com(cmd_probe),
        "--migrate" => cmd_migrate(args.get(1).map(String::as_str)),
        "--gen-icon" => cmd_gen_icon(args.get(1).map(String::as_str)),
        "--preview" => cmd_preview(args.get(1).map(String::as_str).unwrap_or("CN")),
        "" | "--autostart" => cmd_run(cmd == "--autostart"),
        other => {
            eprintln!("未知参数: {other}\n");
            println!("{HELP}");
            ExitCode::from(2)
        }
    }
}

fn enable_dpi_awareness() {
    use windows::Win32::UI::HiDpi::{
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
    };
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
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
        ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE,
        SetStdHandle,
    };
    use windows::core::w;

    unsafe {
        // 已经被重定向到文件/管道时 stdout 有效，不能再改成 CONOUT$（否则输出会丢进控制台）
        let has_stdout = match GetStdHandle(STD_OUTPUT_HANDLE) {
            Ok(h) => !h.is_invalid() && !h.0.is_null(),
            Err(_) => false,
        };
        if has_stdout {
            return;
        }
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

/// 预览：在屏幕固定位置显示 8 秒文字牌，便于用截图核对颜色、字号与圆角
fn cmd_preview(state_key: &str) -> ExitCode {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, TranslateMessage,
    };

    let cfg = config::AppConfig::load();
    let theme = cfg.symbol_theme();
    let state = state::InputState::from_key(state_key).unwrap_or(state::InputState::Cn);
    let text = cfg.symbol_text(state);
    let bg = cfg.symbol_bg(state);

    let hwnd = symbol::create();
    if hwnd.0.is_null() {
        eprintln!("创建预览窗口失败");
        return ExitCode::from(1);
    }
    let caret = RECT { left: 900, top: 600, right: 902, bottom: 602 };
    let dpi = unsafe {
        let fg = windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow();
        let d = windows::Win32::UI::HiDpi::GetDpiForWindow(fg);
        if d == 0 { 96 } else { d }
    };
    unsafe {
        std::env::set_var(
            "ITNG_PREVIEW_DUMP",
            std::env::temp_dir().join("itng-preview.bmp"),
        );
    }
    let rect = symbol::show(
        hwnd,
        &text,
        bg,
        theme.fg,
        &theme.font,
        theme.size,
        theme.weight,
        theme.opacity,
        (theme.offset_x, theme.offset_y),
        theme.corner,
        &caret,
        dpi,
    );

    if let Ok(rect_path) = std::env::var("ITNG_PREVIEW_RECT") {
        // 窗口句柄的坐标会被 DPI 虚拟化，这里直接把渲染时的物理坐标交给外部截图脚本
        if let Some((x, y, w, h)) = rect {
            let _ = std::fs::write(rect_path, format!("{x} {y} {w} {h}"));
        }
    }

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
    let mut msg = MSG::default();
    unsafe {
        while std::time::Instant::now() < deadline {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            std::thread::sleep(std::time::Duration::from_millis(30));
        }
    }
    println!("预览结束（状态 {}，文字 {}，底色 0x{:06X}）", state.key(), text, bg);
    ExitCode::SUCCESS
}

fn cmd_gen_icon(out: Option<&str>) -> ExitCode {
    let Some(out) = out else {
        eprintln!("用法: input-tip-ng --gen-icon <输出 .ico 路径>");
        return ExitCode::from(2);
    };
    match icon::generate_ico(std::path::Path::new(out)) {
        Ok(()) => {
            println!("已生成多尺寸图标: {out}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("生成图标失败: {e}");
            ExitCode::from(1)
        }
    }
}

fn cmd_migrate(old: Option<&str>) -> ExitCode {
    let Some(old) = old else {
        eprintln!("用法: input-tip-ng --migrate <旧 config.ini 路径>");
        return ExitCode::from(2);
    };
    match migrate::run(std::path::Path::new(old)) {
        Ok(summary) => {
            println!("迁移完成：\n{summary}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("迁移失败: {e}");
            ExitCode::from(1)
        }
    }
}

fn cmd_probe() -> ExitCode {
    unsafe {
        let fg = windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow();
        let proc_name = process::name(fg).unwrap_or_else(|| "<未知>".to_string());
        let state = ime::detect(fg);
        let dpi = windows::Win32::UI::HiDpi::GetDpiForWindow(fg);
        println!("前台进程: {proc_name} (hwnd={}, dpi={})", fg.0 as isize, dpi);
        println!("探测原始值: {}", ime::probe(fg));
        println!("判定状态: {}", state.key());
        println!("线程 UI 信息: {}", caret::probe());
        println!("UIA 详情: {}", caret::uia_probe());
        match caret::rect() {
            Some(c) => println!(
                "caret: 屏幕坐标 ({}, {}) 尺寸 {}x{}",
                c.left,
                c.top,
                c.right - c.left,
                c.bottom - c.top
            ),
            None => println!("caret: 未取到"),
        }
        if let Some(owner) = caret::owner() {
            println!("caret 所属窗口: {}", owner.0 as isize);
        }
    }
    ExitCode::SUCCESS
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
