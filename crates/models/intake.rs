//! Raw 用户输入契约 —— `server/core/models/intake.py` 契约部分的移植。
//!
//! 复杂度部分（`TaskComplexityAssessment` 家族）在 [`crate::complexity`]。
//! 红线：`raw_user_query` 原样保存，不做翻译、改写或规范化。

use std::collections::HashSet;

use serde::Deserialize;
use serde::Serialize;
use serde_json::Map;
use serde_json::Value;

/// 最大侵入级别（Python `Literal["passive", "active", "exploit"]`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaxIntrusiveness {
    /// 被动（只读探测）。
    Passive,
    /// 主动（改变目标状态的验证）。
    Active,
    /// 利用（PoC 级验证）。
    Exploit,
}

fn default_max_intrusiveness() -> MaxIntrusiveness {
    MaxIntrusiveness::Active
}

fn default_constraint_list() -> Vec<String> {
    Vec::new()
}

/// Python `_drop_blank_items` 字段校验器：strip、去空、去重、保序。
///
/// 约束列表进入存储与路由后不得再出现空白项或重复项——下游以集合语义
/// 消费这些列表，脏输入会让"同一禁止动作出现两次"这类噪声进审计链。
fn drop_blank_items(items: &[String]) -> Vec<String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut cleaned: Vec<String> = Vec::new();
    for item in items {
        let stripped = item.trim();
        if stripped.is_empty() || !seen.insert(stripped.to_string()) {
            continue;
        }
        cleaned.push(stripped.to_string());
    }
    cleaned
}

fn deserialize_normalized_list<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Vec::<String>::deserialize(deserializer)?;
    Ok(drop_blank_items(&raw))
}

/// 从原始用户输入提取的结构化范围与安全约束
/// （`StructuredConstraintContract`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StructuredConstraintContract {
    /// 授权范围内的目标。
    #[serde(
        default = "default_constraint_list",
        deserialize_with = "deserialize_normalized_list"
    )]
    pub in_scope: Vec<String>,
    /// 禁止触碰的目标。
    #[serde(
        default = "default_constraint_list",
        deserialize_with = "deserialize_normalized_list"
    )]
    pub forbidden_targets: Vec<String>,
    /// 禁止触碰的端口。
    #[serde(
        default = "default_constraint_list",
        deserialize_with = "deserialize_normalized_list"
    )]
    pub forbidden_ports: Vec<String>,
    /// 禁止执行的动作。
    #[serde(
        default = "default_constraint_list",
        deserialize_with = "deserialize_normalized_list"
    )]
    pub forbidden_actions: Vec<String>,
    /// 最大侵入级别（默认 `active`）。
    #[serde(default = "default_max_intrusiveness")]
    pub max_intrusiveness: MaxIntrusiveness,
    /// 附注。
    #[serde(
        default = "default_constraint_list",
        deserialize_with = "deserialize_normalized_list"
    )]
    pub notes: Vec<String>,
}

impl Default for StructuredConstraintContract {
    fn default() -> Self {
        Self {
            in_scope: Vec::new(),
            forbidden_targets: Vec::new(),
            forbidden_ports: Vec::new(),
            forbidden_actions: Vec::new(),
            max_intrusiveness: MaxIntrusiveness::Active,
            notes: Vec::new(),
        }
    }
}

impl StructuredConstraintContract {
    /// 以五个约束列表构造（经 `_drop_blank_items` 归一化——镜像 Python
    /// 字段校验器在构造期即生效的语义）。
    #[must_use]
    pub fn new(
        in_scope: &[String],
        forbidden_targets: &[String],
        forbidden_ports: &[String],
        forbidden_actions: &[String],
        notes: &[String],
    ) -> Self {
        Self {
            in_scope: drop_blank_items(in_scope),
            forbidden_targets: drop_blank_items(forbidden_targets),
            forbidden_ports: drop_blank_items(forbidden_ports),
            forbidden_actions: drop_blank_items(forbidden_actions),
            max_intrusiveness: MaxIntrusiveness::Active,
            notes: drop_blank_items(notes),
        }
    }

    /// 以指定侵入级别覆盖构造。
    #[must_use]
    pub fn with_intrusiveness(mut self, level: MaxIntrusiveness) -> Self {
        self.max_intrusiveness = level;
        self
    }
}

/// `raw_user_query` 构造期校验失败（Python `ValueError` 的类型化对应）。
#[derive(Debug, thiserror::Error)]
pub enum IntakeError {
    /// 查询为空白串。
    #[error("raw_user_query must not be blank")]
    BlankQuery,
}

fn deserialize_non_blank_query<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = String::deserialize(deserializer)?;
    if raw.trim().is_empty() {
        return Err(serde::de::Error::custom("raw_user_query must not be blank"));
    }
    Ok(raw)
}

/// 不可变的原始用户查询 + 结构化规划注记（`RawInputEnvelope`）。
///
/// 红线：`raw_user_query` 只承载原文。翻译、改写、规范化一律发生在
/// 派生字段上。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawInputEnvelope {
    /// 原始用户查询（非空白，原样保存）。
    #[serde(deserialize_with = "deserialize_non_blank_query")]
    pub raw_user_query: String,
    /// 结构化约束。
    #[serde(default)]
    pub structured_constraints: StructuredConstraintContract,
    /// 输出语言提示。
    #[serde(default)]
    pub output_language_hint: Option<String>,
    /// 地区提示。
    #[serde(default)]
    pub region_hint: Option<String>,
    /// 附加元数据（键序 = 插入序）。
    #[serde(default)]
    pub metadata: Map<String, Value>,
}

impl RawInputEnvelope {
    /// 构造（空白查询报错——镜像 Python 校验器）。
    ///
    /// # Errors
    /// `raw_user_query` 为空白串时返回 [`IntakeError::BlankQuery`]。
    pub fn try_new(raw_user_query: String) -> Result<Self, IntakeError> {
        if raw_user_query.trim().is_empty() {
            return Err(IntakeError::BlankQuery);
        }
        Ok(Self {
            raw_user_query,
            structured_constraints: StructuredConstraintContract::default(),
            output_language_hint: None,
            region_hint: None,
            metadata: Map::new(),
        })
    }

    /// 以指定约束构造。
    #[must_use]
    pub fn with_constraints(mut self, constraints: StructuredConstraintContract) -> Self {
        self.structured_constraints = constraints;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constraint_lists_normalize_on_deserialize() {
        let raw = serde_json::json!({
            "in_scope": [" allowed.test ", "", "allowed.test", "other.test"],
            "forbidden_actions": ["exploit"],
        });
        let contract: StructuredConstraintContract =
            serde_json::from_value(raw).expect("合法 wire JSON 必须可解析");
        assert_eq!(contract.in_scope, ["allowed.test", "other.test"]);
        assert_eq!(contract.forbidden_actions, ["exploit"]);
        assert_eq!(contract.max_intrusiveness, MaxIntrusiveness::Active);
    }

    #[test]
    fn constraint_constructor_normalizes_like_python_validator() {
        let contract = StructuredConstraintContract::new(
            &[" a ".to_string(), String::new(), "a".to_string()],
            &[],
            &[],
            &[],
            &[],
        );
        assert_eq!(contract.in_scope, ["a"]);
    }

    #[test]
    fn constraint_rejects_unknown_fields() {
        let raw = serde_json::json!({"in_scope": [], "extra": 1});
        let result: Result<StructuredConstraintContract, _> = serde_json::from_value(raw);
        assert!(result.is_err(), "extra=forbid：未知字段必须被拒绝");
    }

    #[test]
    fn raw_input_rejects_blank_query_on_deserialize() {
        let raw = serde_json::json!({"raw_user_query": "   "});
        let result: Result<RawInputEnvelope, _> = serde_json::from_value(raw);
        assert!(result.is_err(), "空白查询必须被拒绝");
    }

    #[test]
    fn raw_input_try_new_rejects_blank_and_applies_defaults() {
        assert!(RawInputEnvelope::try_new("  ".to_string()).is_err());
        let envelope = RawInputEnvelope::try_new("帮我审计这个站点".to_string())
            .expect("非空白查询必须可构造");
        assert_eq!(envelope.raw_user_query, "帮我审计这个站点");
        assert_eq!(
            envelope.structured_constraints,
            StructuredConstraintContract::default()
        );
        assert!(envelope.metadata.is_empty());
    }

    #[test]
    fn raw_input_roundtrips_with_constraints() {
        let envelope = RawInputEnvelope::try_new("audit it".to_string())
            .expect("非空白查询必须可构造")
            .with_constraints(
                StructuredConstraintContract::new(
                    &[],
                    &["10.0.0.0/8".to_string()],
                    &["22".to_string()],
                    &["exploit".to_string()],
                    &[],
                )
                .with_intrusiveness(MaxIntrusiveness::Passive),
            );
        let wire = serde_json::to_value(&envelope).expect("序列化不会失败");
        let back: RawInputEnvelope = serde_json::from_value(wire).expect("wire 形态必须可往返");
        assert_eq!(back, envelope);
        assert_eq!(
            back.structured_constraints.max_intrusiveness,
            MaxIntrusiveness::Passive
        );
    }
}
