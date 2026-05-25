//! 运行时共享配置
//!
//! 这些配置可在运行时被 Admin API 修改并即时生效（无需重启）。
//! 与 `Config` 不同，运行时配置使用 `Arc<RwLock<...>>` 在 Anthropic
//! 请求处理器和 Admin 服务之间共享。
//!
//! 当 Admin API 写入这些配置时，会同步回写到 `config.json`，确保下次重启
//! 也能保留更改。

use std::sync::Arc;

use parking_lot::RwLock;

use super::config::{Config, SystemPromptPosition, UserPreset};

/// Prompt 注入运行时配置
///
/// 字段含义：
/// - `enabled`：注入总开关；关闭后所有 preset + 自定义文本都不注入
/// - `enabled_presets`：启用的 preset id 列表（混合内置 + 用户自定义）
/// - `user_presets`：用户自定义预设清单（与内置 `PRESETS` 并列）
/// - `custom_content`：自由文本补充（追加到所有 preset 之后）
/// - `position`：拼接结果在 system role 中的插入位置
#[derive(Debug, Clone)]
pub struct PromptRuntimeConfig {
    pub enabled: bool,
    pub enabled_presets: Vec<String>,
    pub user_presets: Vec<UserPreset>,
    pub custom_content: Option<String>,
    pub position: SystemPromptPosition,
}

impl PromptRuntimeConfig {
    pub fn from_config(cfg: &Config) -> Self {
        Self {
            enabled: cfg.system_prompt_enabled,
            enabled_presets: cfg.enabled_presets.clone(),
            user_presets: cfg.user_presets.clone(),
            custom_content: cfg.system_prompt.clone(),
            position: cfg.system_prompt_position,
        }
    }

    /// 计算最终要注入的文本。返回 `None` 表示无需注入。
    ///
    /// 拼接顺序：
    /// 1. 内置 preset（按 `PRESETS` 数组顺序）
    /// 2. 用户 preset（按 `user_presets` 顺序）
    /// 3. `custom_content`
    /// 各段之间用空行连接。
    pub fn build_injection_text(&self) -> Option<String> {
        if !self.enabled {
            return None;
        }

        let mut parts: Vec<String> = Vec::new();

        for p in crate::anthropic::prompt_presets::PRESETS {
            if self.enabled_presets.iter().any(|id| id == p.id) {
                parts.push(p.content.trim().to_string());
            }
        }
        for up in &self.user_presets {
            if self.enabled_presets.iter().any(|id| id == &up.id) {
                let trimmed = up.content.trim();
                if !trimmed.is_empty() {
                    parts.push(trimmed.to_string());
                }
            }
        }
        if let Some(c) = self.custom_content.as_deref() {
            let t = c.trim();
            if !t.is_empty() {
                parts.push(t.to_string());
            }
        }

        if parts.is_empty() {
            None
        } else {
            Some(parts.join("\n\n"))
        }
    }
}

/// 跨模块共享的可变 Prompt 配置句柄
pub type SharedPromptConfig = Arc<RwLock<PromptRuntimeConfig>>;

/// 从 `Config` 构建共享句柄
pub fn shared_from_config(cfg: &Config) -> SharedPromptConfig {
    Arc::new(RwLock::new(PromptRuntimeConfig::from_config(cfg)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_cfg() -> PromptRuntimeConfig {
        PromptRuntimeConfig {
            enabled: false,
            enabled_presets: Vec::new(),
            user_presets: Vec::new(),
            custom_content: None,
            position: SystemPromptPosition::Append,
        }
    }

    #[test]
    fn disabled_returns_none() {
        let mut c = empty_cfg();
        c.custom_content = Some("hi".into());
        assert!(c.build_injection_text().is_none());
    }

    #[test]
    fn enabled_but_empty_returns_none() {
        let mut c = empty_cfg();
        c.enabled = true;
        assert!(c.build_injection_text().is_none());
    }

    #[test]
    fn enabled_builtin_concise() {
        let mut c = empty_cfg();
        c.enabled = true;
        c.enabled_presets.push("concise".to_string());
        let out = c.build_injection_text().unwrap();
        assert!(out.contains("CONCISE"));
    }

    #[test]
    fn user_preset_appears_when_enabled() {
        let mut c = empty_cfg();
        c.enabled = true;
        c.user_presets.push(UserPreset {
            id: "u1".into(),
            name: "u1".into(),
            description: String::new(),
            content: "USER_TEXT_MARKER".into(),
        });
        c.enabled_presets.push("u1".to_string());
        let out = c.build_injection_text().unwrap();
        assert!(out.contains("USER_TEXT_MARKER"));
    }

    #[test]
    fn custom_content_appended() {
        let mut c = empty_cfg();
        c.enabled = true;
        c.enabled_presets.push("concise".to_string());
        c.custom_content = Some("EXTRA_NOTE".into());
        let out = c.build_injection_text().unwrap();
        let concise_pos = out.find("CONCISE").unwrap();
        let extra_pos = out.find("EXTRA_NOTE").unwrap();
        assert!(extra_pos > concise_pos, "custom_content 应在 preset 之后");
    }

    #[test]
    fn unknown_preset_id_skipped() {
        let mut c = empty_cfg();
        c.enabled = true;
        c.enabled_presets.push("nonexistent".to_string());
        assert!(c.build_injection_text().is_none());
    }

    #[test]
    fn empty_user_preset_content_skipped() {
        let mut c = empty_cfg();
        c.enabled = true;
        c.user_presets.push(UserPreset {
            id: "blank".into(),
            name: "blank".into(),
            description: String::new(),
            content: "   \n  ".into(),
        });
        c.enabled_presets.push("blank".to_string());
        assert!(c.build_injection_text().is_none());
    }
}
