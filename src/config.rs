//! 配置：%LOCALAPPDATA%\input-tip-ng\config.ini
//!
//! 为什么继续用 INI：
//! 1. 与旧版 config.ini 形态一致，迁移成本最低，也可以直接手改；
//! 2. 无需 serde/TOML 依赖，保持零第三方依赖、可离线构建；
//! 3. 解析时保留未知键，迁移旧配置不会丢信息。

use std::fs;
use std::io;
use std::path::PathBuf;

const DEFAULT_INI: &str = "; input-tip-ng 配置（UTF-8）。程序自动维护，手工修改后重启生效。\n\
[app]\n\
language=zh-CN\n\
poll_interval_ms=20\n\
\n\
[autostart]\n\
enabled=true\n\
method=task\n\
delay_secs=20\n\
";

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

    /// 删除键（迁移与清理时使用；当前阶段尚未调用）
    #[allow(dead_code)]
    pub fn remove_key(&mut self, section: &str, key: &str) {
        if let Some(s) = self.sections.iter_mut().find(|s| s.name.eq_ignore_ascii_case(section)) {
            s.entries.retain(|(k, _)| !k.eq_ignore_ascii_case(key));
        }
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
            Err(_) => Ini::parse(DEFAULT_INI),
        };
        if ini.get("app", "language").is_none() {
            ini.set("app", "language", "zh-CN");
        }
        if ini.get("autostart", "enabled").is_none() {
            ini.set("autostart", "enabled", "true");
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
        self.ini
            .get("autostart", "delay_secs")
            .and_then(|v| v.parse().ok())
            .unwrap_or(20)
    }

    pub fn set_autostart(&mut self, enabled: bool, method: &str) {
        self.ini.set("autostart", "enabled", if enabled { "true" } else { "false" });
        self.ini.set("autostart", "method", method);
    }
}
