//! Project 模型 —— `server/core/models/project.py` 的移植。

use serde::Deserialize;
use serde::Serialize;

use crate::common::StrMap;
use crate::common::Timestamp;
use crate::common::new_id;
use crate::common::utcnow;
use crate::domain::AuditDomain;
use crate::domain::AuditDomainParseError;
use crate::ids::ProjectId;

fn default_project_id() -> ProjectId {
    ProjectId::new(new_id("proj"))
}

/// Project：审计工作的顶层单元（`Project`）。
///
/// Project 拥有一个 append-only 审计图（Fact/Intent/Hint/Evidence/
/// Finding）及其 `AuditRun`。`target` 描述**审计对象**（仓库 URL、二进制
/// 路径、IDA 数据库等），核心层不关心具体介质。
///
/// Python 侧 `field_validator("audit_domain", mode="before")` 对字符串
/// 做规范化（大小写/连字符变体）后折算为枚举，经 serde `try_from` 的
/// 解析路径镜像。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "ProjectWire")]
pub struct Project {
    /// Project 标识符。
    pub id: ProjectId,
    /// 名称。
    pub name: String,
    /// 形式化审计域。
    pub audit_domain: AuditDomain,
    /// 描述。
    pub description: Option<String>,
    /// 审计目标的自由格式描述（如 `{"repo": ..., "language": "php"}`），覆盖层解释它。
    pub target: StrMap,
    /// 创建时间。
    pub created_at: Timestamp,
    /// 最后更新时间。
    pub updated_at: Timestamp,
}

impl Project {
    /// 以 Python 默认值构造（`Project(name=..., audit_domain=...)`）。
    #[must_use]
    pub fn new(name: String, audit_domain: AuditDomain) -> Self {
        Self {
            id: default_project_id(),
            name,
            audit_domain,
            description: None,
            target: StrMap::new(),
            created_at: utcnow(),
            updated_at: utcnow(),
        }
    }
}

/// [`Project`] 的解析镜像：缺省字段取 Python 默认值，未知字段拒绝
/// （`extra="forbid"`），`audit_domain` 字符串走 `normalize_audit_domain`。
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ProjectWire {
    id: ProjectId,
    name: String,
    #[serde(deserialize_with = "deserialize_audit_domain")]
    audit_domain: AuditDomain,
    description: Option<String>,
    target: StrMap,
    created_at: Timestamp,
    updated_at: Timestamp,
}

/// Python `field_validator("audit_domain", mode="before")`：字符串值经
/// 规范化后再折算为枚举（接受大小写/连字符变体）。
fn deserialize_audit_domain<'de, D>(deserializer: D) -> Result<AuditDomain, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error as _;

    let text = <&str>::deserialize(deserializer)?;
    crate::domain::normalize_audit_domain(text)
        .map_err(|error: AuditDomainParseError| D::Error::custom(error.to_string()))
}

impl Default for ProjectWire {
    fn default() -> Self {
        let base = Project::new(String::new(), AuditDomain::Composite);
        Self {
            id: base.id,
            name: base.name,
            audit_domain: base.audit_domain,
            description: base.description,
            target: base.target,
            created_at: base.created_at,
            updated_at: base.updated_at,
        }
    }
}

impl From<ProjectWire> for Project {
    fn from(wire: ProjectWire) -> Self {
        Self {
            id: wire.id,
            name: wire.name,
            audit_domain: wire.audit_domain,
            description: wire.description,
            target: wire.target,
            created_at: wire.created_at,
            updated_at: wire.updated_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_defaults_match_python() {
        let project = Project::new("p".to_string(), AuditDomain::WebSast);
        assert!(project.id.as_str().starts_with("proj_"));
        assert_eq!(project.description, None);
        assert!(project.target.is_empty());
    }

    #[test]
    fn project_wire_normalizes_audit_domain() {
        let json = r#"{"id":"proj_1","name":"source","audit_domain":"WEB-SAST","target":{"repo_path":"D:/src/app"}}"#;
        let project: Project = serde_json::from_str(json)
            .unwrap_or_else(|error| panic!("audit_domain 规范化后必须可解析: {error}"));
        assert_eq!(project.audit_domain, AuditDomain::WebSast);
        assert_eq!(project.target.get("repo_path"), Some("D:/src/app"));

        let result: Result<Project, _> =
            serde_json::from_str(r#"{"name":"x","audit_domain":"bogus"}"#);
        assert!(result.is_err(), "未知 audit_domain 必须被拒绝");
        let result: Result<Project, _> = serde_json::from_str(r#"{"name":"x","extra":1}"#);
        assert!(result.is_err(), "extra=forbid：未知字段必须被拒绝");
    }
}
