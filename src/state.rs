//! 输入状态定义与配色/文字的固定映射。
//!
//! 六个状态与旧版一致：CN 中文、EN 英文、Caps 大写锁定、US 英文键盘、JP 日文、KR 韩文。

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputState {
    Cn,
    En,
    Caps,
    Us,
    Jp,
    Kr,
}

impl InputState {
    /// 配置键与日志里使用的短名
    pub fn key(self) -> &'static str {
        match self {
            InputState::Cn => "CN",
            InputState::En => "EN",
            InputState::Caps => "CAPS",
            InputState::Us => "US",
            InputState::Jp => "JP",
            InputState::Kr => "KR",
        }
    }

    pub fn from_key(key: &str) -> Option<InputState> {
        match key.to_ascii_uppercase().as_str() {
            "CN" => Some(InputState::Cn),
            "EN" => Some(InputState::En),
            "CAPS" => Some(InputState::Caps),
            "US" => Some(InputState::Us),
            "JP" => Some(InputState::Jp),
            "KR" => Some(InputState::Kr),
            _ => None,
        }
    }
}
