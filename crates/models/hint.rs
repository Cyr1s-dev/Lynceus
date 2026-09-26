//! Hint —— `server/core/models/hint.py` 的移植。
//!
//! 人类/系统给出的引导，**不是**事实链的一部分：影响 Intent 生成与提示词
//! 上下文，但绝不能作为 Finding 的 Evidence 被引用。

use serde::Deserialize;
use serde::Serialize;

use crate::common::Timestamp;
use crate::common::new_id;
use crate::common::utcnow;
use crate::ids::ProjectId;

fn default_hint_id() -> String {
    new_id("hint")
}

fn default_hint_weight() -> i64 {
    50
}

fn default_hint_created_by() -> String {
    "user".to_string()
}

/// 引导审计但绝不作为事实处理的辅助上下文（`Hint`）。
///
/// 示例："prioritize auth bypass"、"this is a Laravel project"、"focus on
/// heap bugs"。`weight` 的 Python 侧约束为 `[0, 100]`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hint {
    /// 引导标识符。
    #[serde(default = "default_hint_id")]
    pub id: String,
    /// 所属 Project。
    pub project_id: ProjectId,
    /// 引导文本。
    pub text: String,
    /// 可过滤的分类标签（如 "scope" / "`tech_stack`" / "focus"）。
    #[serde(default)]
    pub category: Option<String>,
    /// 权重（`[0, 100]`，默认 50）。
    #[serde(default = "default_hint_weight")]
    pub weight: i64,
    /// 创建者。
    #[serde(default = "default_hint_created_by")]
    pub created_by: String,
    /// 创建时间。
    #[serde(default = "crate::common::utcnow")]
    pub created_at: Timestamp,
}

impl Hint {
    /// 以 Python 默认值构造（`Hint(project_id=..., text=...)`）。
    #[must_use]
    pub fn new(project_id: ProjectId, text: String) -> Self {
        Self {
            id: default_hint_id(),
            project_id,
            text,
            category: None,
            weight: default_hint_weight(),
            created_by: default_hint_created_by(),
            created_at: utcnow(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hint_defaults_match_python() {
        let hint = Hint::new(ProjectId::new("proj_test".to_string()), "focus".to_string());
        assert!(hint.id.starts_with("hint_"));
        assert_eq!(hint.weight, 50);
        assert_eq!(hint.created_by, "user");
        assert_eq!(hint.category, None);
    }

    #[test]
    fn hint_rejects_unknown_fields() {
        let result: Result<Hint, _> =
            serde_json::from_str(r#"{"project_id":"p","text":"t","extra":1}"#);
        assert!(result.is_err(), "extra=forbid：未知字段必须被拒绝");
    }
}
