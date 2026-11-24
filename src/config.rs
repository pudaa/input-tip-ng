//! 配置：%LOCALAPPDATA%\input-tip-ng\config.ini
//!
//! 为什么继续用 INI：
//! 1. 与旧版 config.ini 形态一致，迁移成本最低，也可以直接手改；
//! 2. 无需 serde/TOML 依赖，保持零第三方依赖、可离线构建；
//! 3. 解析时保留未知键，迁移旧配置不会丢信息。
//!
//! 默认值直接采用老大本机在用的那套（文字牌 + 24 条白名单），
//! 首次运行即接近旧版观感，不需要先跑迁移。

use std::fs;
use std::io;
use std::path::PathBuf;

use crate::state::InputState;

/// 默认配置项（唯一来源）：首次运行时写入，之后只补齐缺失键
const DEFAULT_ENTRIES: &[(&str, &str, &str)] = &[
    ("app", "language", "zh-CN"),
    ("app", "poll_interval_ms", "20"),
    ("autostart", "enabled", "true"),
    ("autostart", "method", "task"),
    ("autostart", "delay_secs", "20"),
    ("symbol", "enabled", "true"),
    ("symbol", "font", "霞鹜文楷等宽"),
    ("symbol", "size", "11"),
    ("symbol", "weight", "500"),
    ("symbol", "opacity", "100"),
    ("symbol", "offset_x", "8"),
    ("symbol", "offset_y", "-20"),
    ("symbol", "hide_delay_ms", "2500"),
    ("symbol", "corner", "7"),
    ("symbol", "fg", "0xFFFFFF"),
    ("symbol", "text.CN", "漢"),
    ("symbol", "text.EN", "En"),
    ("symbol", "text.CAPS", "Ca"),
    ("symbol", "text.US", "US"),
    ("symbol", "text.JP", "日"),
    ("symbol", "text.KR", "韩"),
    ("symbol", "bg.CN", "0xA4622D"),
    ("symbol", "bg.EN", "0x87CEFA"),
    ("symbol", "bg.CAPS", "0xFFD166"),
    ("symbol", "bg.US", "0x0000FF"),
    ("symbol", "bg.JP", "0xCCCC00"),
    ("symbol", "bg.KR", "0x800080"),
    (
        "rules",
        "show",
        "Code.exe,Typora.exe,DingTalk.exe,QQ.exe,Weixin.exe,msedge.exe,POWERPNT.EXE,WINWORD.EXE,EXCEL.EXE,ONENOTE.EXE,wps.exe,chrome.exe,explorer.exe,Files.exe,bongo-cat.exe,GitMind.exe,project-graph.exe,fdm.exe,Unity Hub.exe,Animate.exe,Illustrator.exe,Plain Craft Launcher 2.exe",
    ),
];

#[derive(Debug, Clone, Default)]
pub struct Section {
    pub name: String,
    pub entries: Vec<(String, String)>,
}

#[derive(Debug, Clone, Default)]
pub struct Ini {
    sections: Vec<Section>,
}

impl Ini {
    pub fn parse(text: &str) -> Self {
        let mut ini = Ini::default();
        let mut current: Option<usize> = None;

        for raw in text.lines() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
                continue;
            }
            if line.starts_with('[') && line.ends_with(']') {
                let name = line[1..line.len() - 1].trim().to_string();
                ini.sections.push(Section { name, entries: Vec::new() });
                current = Some(ini.sections.len() - 1);
                continue;
            }
            let Some(idx) = current else { continue };
            let Some((k, v)) = line.split_once('=') else { continue };
            ini.sections[idx].entries.push((k.trim().to_string(), unquote(v.trim())));
        }
        ini
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        for (i, s) in self.sections.iter().enumerate() {
            if i > 0 {
                out.push('\n');
            }
            out.push('[');
            out.push_str(&s.name);
            out.push_str("]\n");
            for (k, v) in &s.entries {
                out.push_str(k);
                out.push('=');
                out.push_str(v);
                out.push('\n');
            }
        }
        out
    }

    pub fn sections(&self) -> &[Section] {
        &self.sections
    }

    pub fn get(&self, section: &str, key: &str) -> Option<&str> {
        self.sections
            .iter()
            .find(|s| s.name.eq_ignore_ascii_case(section))
            .and_then(|s| {
                s.entries
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case(key))
                    .map(|(_, v)| v.as_str())
            })
    }

    pub fn set(&mut self, section: &str, key: &str, value: &str) {
        if let Some(s) = self.sections.iter_mut().find(|s| s.name.eq_ignore_ascii_case(section)) {
            if let Some(entry) = s.entries.iter_mut().find(|(k, _)| k.eq_ignore_ascii_case(key)) {
                entry.1 = value.to_string();
            } else {
                s.entries.push((key.to_string(), value.to_string()));
            }
            return;
        }
        self.sections.push(Section {
            name: section.to_string(),
            entries: vec![(key.to_string(), value.to_string())],
        });
    }
}

fn unquote(v: &str) -> String {
    let b = v.as_bytes();
    if b.len() >= 2 && b[0] == b'"' && b[b.len() - 1] == b'"' {
        v[1..v.len() - 1].to_string()
    } else {
        v.to_string()
    }
}

fn default_ini() -> Ini {
    let mut ini = Ini::default();
    for (section, key, value) in DEFAULT_ENTRIES {
        ini.set(section, key, value);
    }
    ini
}

fn parse_color(v: &str) -> Option<u32> {
    let s = v.trim();
    let s = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")).unwrap_or(s);
    u32::from_str_radix(s, 16).ok()
}

#[derive(Debug, Clone)]
pub struct SymbolTheme {
    pub enabled: bool,
    pub font: String,
    pub size: f32,
    pub weight: i32,
    pub opacity: u8,
    pub offset_x: i32,
    pub offset_y: i32,
    pub hide_delay_ms: u64,
    pub corner: f32,
    pub fg: u32,
}

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub path: PathBuf,
    pub ini: Ini,
}

impl AppConfig {
    pub fn load() -> Self {
        let path = crate::log::root_dir().join("config.ini");
        let mut ini = match fs::read_to_string(&path) {
            Ok(text) => Ini::parse(&text),
            Err(_) => default_ini(),
        };
        // 补齐缺失键：升级后新加的配置项自动出现默认值，不覆盖用户已有值
        for (section, key, value) in DEFAULT_ENTRIES {
            if ini.get(section, key).is_none() {
                ini.set(section, key, value);
            }
        }
        let cfg = AppConfig { path, ini };
        let _ = cfg.save();
        cfg
    }

    pub fn save(&self) -> io::Result<()> {
        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::write(&self.path, self.ini.render())
    }

    pub fn autostart_enabled(&self) -> bool {
        match self.ini.get("autostart", "enabled") {
            Some(v) => !(v.eq_ignore_ascii_case("false") || v == "0"),
            None => true,
        }
    }

    pub fn autostart_method(&self) -> &str {
        self.ini.get("autostart", "method").unwrap_or("task")
    }

    pub fn autostart_delay_secs(&self) -> u32 {
        self.ini.get("autostart", "delay_secs").and_then(|v| v.parse().ok()).unwrap_or(20)
    }

    pub fn set_autostart(&mut self, enabled: bool, method: &str) {
        self.ini.set("autostart", "enabled", if enabled { "true" } else { "false" });
        self.ini.set("autostart", "method", method);
    }

    pub fn poll_interval_ms(&self) -> u32 {
        self.ini
            .get("app", "poll_interval_ms")
            .and_then(|v| v.parse().ok())
            .unwrap_or(20)
            .clamp(10, 1000)
    }

    pub fn symbol_theme(&self) -> SymbolTheme {
        let num = |key: &str, def: f32| -> f32 {
            self.ini
                .get("symbol", key)
                .and_then(|v| v.trim().parse::<f32>().ok())
                .unwrap_or(def)
        };
        SymbolTheme {
            enabled: self
                .ini
                .get("symbol", "enabled")
                .map(|v| !(v.eq_ignore_ascii_case("false") || v == "0"))
                .unwrap_or(true),
            font: self.ini.get("symbol", "font").unwrap_or("霞鹜文楷等宽").to_string(),
            size: num("size", 11.0).clamp(6.0, 96.0),
            weight: num("weight", 500.0) as i32,
            opacity: num("opacity", 100.0).clamp(0.0, 100.0) as u8,
            offset_x: num("offset_x", 0.0) as i32,
            offset_y: num("offset_y", -20.0) as i32,
            hide_delay_ms: num("hide_delay_ms", 500.0).max(0.0) as u64,
            corner: num("corner", 6.0).max(0.0),
            fg: self
                .ini
                .get("symbol", "fg")
                .and_then(parse_color)
                .unwrap_or(0xFFFFFF),
        }
    }

    pub fn symbol_text(&self, state: InputState) -> String {
        self.ini
            .get("symbol", &format!("text.{}", state.key()))
            .unwrap_or(state.key())
            .to_string()
    }

    pub fn symbol_bg(&self, state: InputState) -> u32 {
        self.ini
            .get("symbol", &format!("bg.{}", state.key()))
            .and_then(parse_color)
            .unwrap_or(0x808080)
    }

    /// 白名单：空表示所有进程都显示
    pub fn rules_show(&self) -> Vec<String> {
        self.ini
            .get("rules", "show")
            .map(|v| {
                v.split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default()
    }
}
