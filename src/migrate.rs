//! 从旧版（AutoHotkey）config.ini 迁移到新配置。
//!
//! 只迁移实际启用的那套：文字牌 + 白名单 + 运行参数；旧版未启用的方案不生成。
//! 旧文件通常是 UTF-16LE（带 BOM），也会兼容 UTF-8。
//! 迁移前会把新配置备份成 config.ini.bak，旧文件原样不动。

use std::fs;
use std::path::Path;

use crate::config::AppConfig;
use crate::log;
use crate::state::InputState;

const STATES: [InputState; 5] = [
    InputState::Cn,
    InputState::En,
    InputState::Caps,
    InputState::Jp,
    InputState::Kr,
];

fn decode(bytes: &[u8]) -> String {
    if bytes.starts_with(&[0xFF, 0xFE]) {
        let units: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&units)
    } else if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        String::from_utf8_lossy(&bytes[3..]).to_string()
    } else {
        String::from_utf8_lossy(bytes).to_string()
    }
}

fn first_of(ini: &crate::config::Ini, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|k| ini.get("Settings", k).map(|v| v.to_string()))
}

/// 执行迁移，返回给人看的摘要
pub fn run(old_path: &Path) -> Result<String, String> {
    let bytes = fs::read(old_path).map_err(|e| format!("读取旧配置失败: {e}"))?;
    let text = decode(&bytes);
    let old = crate::config::Ini::parse(&text);

    let mut cfg = AppConfig::load();
    // 备份现有新配置
    if cfg.path.exists() {
        let bak = cfg.path.with_extension("ini.bak");
        let _ = fs::copy(&cfg.path, &bak);
    }

    let mut report: Vec<String> = Vec::new();

    // 文字与底色
    for state in STATES {
        let suffix = match state {
            InputState::Cn => "CN",
            InputState::En => "EN",
            InputState::Caps => "Caps",
            InputState::Jp => "JP",
            InputState::Kr => "KR",
            _ => continue,
        };
        if let Some(v) = old.get("Settings", &format!("caretSymbolTextContent{suffix}")) {
            if !v.trim().is_empty() {
                cfg.ini.set("symbol", &format!("text.{}", state.key()), v.trim());
            }
        }
        if let Some(v) = old.get("Settings", &format!("caretSymbolTextBgColor{suffix}")) {
            let v = v.trim();
            if !v.is_empty() {
                cfg.ini.set("symbol", &format!("bg.{}", state.key()), v);
            }
        }
    }

    // 外观
    if let Some(v) = first_of(&old, &["caretSymbolTextFontCN", "caretSymbolTextFontEN"]) {
        if !v.trim().is_empty() {
            cfg.ini.set("symbol", "font", v.trim());
            report.push(format!("字体 -> {}", v.trim()));
        }
    }
    if let Some(v) = first_of(&old, &["caretSymbolTextSizeCN", "caretSymbolTextSizeEN"]) {
        if let Ok(n) = v.trim().parse::<f32>() {
            cfg.ini.set("symbol", "size", &format!("{n}"));
            report.push(format!("字号 -> {n}"));
        }
    }
    if let Some(v) = first_of(&old, &["caretSymbolTextWeightCN", "caretSymbolTextWeightEN"]) {
        if let Ok(n) = v.trim().parse::<i32>() {
            cfg.ini.set("symbol", "weight", &format!("{n}"));
        }
    }
    if let Some(v) = first_of(&old, &["caretSymbolTextTransparentCN", "caretSymbolTextTransparentEN"]) {
        if let Ok(n) = v.trim().parse::<u32>() {
            // 旧版取值 100 表示不透明；这里按百分比直接映射
            let opacity = n.min(100);
            cfg.ini.set("symbol", "opacity", &format!("{opacity}"));
        }
    }
    let offset_x = first_of(&old, &["symbolNearCursorOffsetX"]).and_then(|v| v.trim().parse::<i32>().ok());
    let offset_y = first_of(&old, &["symbolNearCursorOffsetY"]).and_then(|v| v.trim().parse::<i32>().ok());
    if let Some(x) = offset_x {
        cfg.ini.set("symbol", "offset_x", &format!("{x}"));
    }
    if let Some(y) = offset_y {
        // originY=below 时旧版用正偏移，这里统一成「上方为负」
        let origin = old.get("Settings", "caretSymbolOriginY").unwrap_or("above").trim().to_ascii_lowercase();
        let y = if origin == "below" { y.abs() } else { -y.abs() };
        cfg.ini.set("symbol", "offset_y", &format!("{y}"));
        report.push(format!("偏移 -> ({}, {y})，origin={origin}", offset_x.unwrap_or(0)));
    }
    if let Some(v) = first_of(&old, &["caretSymbolHideDelay"]) {
        if let Ok(n) = v.trim().parse::<u64>() {
            cfg.ini.set("symbol", "hide_delay_ms", &format!("{n}"));
            report.push(format!("隐藏延迟 -> {n}ms"));
        }
    }

    // 白名单：旧版按 [Window.CaretSymbol.Rule.*] 逐条记录，只取 trigger=show
    let mut procs: Vec<String> = Vec::new();
    for section in old.sections() {
        let name = section.name.to_ascii_lowercase();
        if !name.starts_with("window.caretsymbol.rule") {
            continue;
        }
        let trigger = old
            .get(&section.name, "trigger")
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        if trigger != "show" {
            continue;
        }
        if let Some(proc_name) = old.get(&section.name, "process") {
            let p = proc_name.trim();
            if !p.is_empty() && !procs.iter().any(|x| x.eq_ignore_ascii_case(p)) {
                procs.push(p.to_string());
            }
        }
    }
    if !procs.is_empty() {
        cfg.ini.set("rules", "show", &procs.join(","));
        report.push(format!("白名单 -> {} 个进程", procs.len()));
    }

    // 运行参数
    if let Some(v) = first_of(&old, &["language"]) {
        if !v.trim().is_empty() {
            cfg.ini.set("app", "language", v.trim());
        }
    }
    if let Some(v) = first_of(&old, &["pollInterval"]) {
        if let Ok(n) = v.trim().parse::<u32>() {
            cfg.ini.set("app", "poll_interval_ms", &format!("{n}"));
        }
    }
    if let Some(v) = first_of(&old, &["inputMethodSwitchState"]) {
        let v = v.trim();
        if !v.is_empty() {
            cfg.ini.set("switch", "hotkey", v);
            report.push(format!("切换热键 -> {v}（P2 启用）"));
        }
    }

    cfg.save().map_err(|e| format!("写入新配置失败: {e}"))?;
    log::info(
        "migrate",
        &format!("从 {} 迁移完成：{}", old_path.display(), report.join("; ")),
    );
    report.push(format!("新配置 -> {}", cfg.path.display()));
    Ok(report.join("\n"))
}
