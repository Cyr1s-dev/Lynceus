//! Evidence 层模型 —— `server/core/models/evidence.py` 的移植。

use serde::Deserialize;
use serde::Serialize;
use serde_json::Map;
use serde_json::Value;

use crate::common::Timestamp;
use crate::common::new_id;
use crate::common::utcnow;
use crate::ids::BranchId;
use crate::ids::EvidenceId;
use crate::ids::MissionId;
use crate::ids::ProjectId;
use crate::ids::RunId;
use crate::ids::TaskId;
use crate::ids::ToolInvocationId;
use crate::lifecycle::EvidenceKind;

fn default_evidence_id() -> EvidenceId {
    EvidenceId::new(new_id("evd"))
}

/// CodeLocation：源码或二进制中的物理位置（`CodeLocation`），映射 SARIF location。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodeLocation {
    /// 文件路径、模块名或二进制名。
    pub artifact: String,
    /// 起始行号。
    #[serde(default)]
    pub start_line: Option<i64>,
    /// 结束行号。
    #[serde(default)]
    pub end_line: Option<i64>,
    /// 二进制地址。
    #[serde(default)]
    pub address: Option<String>,
    /// 符号名。
    #[serde(default)]
    pub symbol: Option<String>,
    /// 代码片段。
    #[serde(default)]
    pub snippet: Option<String>,
}

impl CodeLocation {
    /// 以 Python 默认值构造（`CodeLocation(artifact=...)`）。
    #[must_use]
    pub fn new(artifact: String) -> Self {
        Self {
            artifact,
            start_line: None,
            end_line: None,
            address: None,
            symbol: None,
            snippet: None,
        }
    }
}

/// Evidence：支撑漏洞判断的具体证据（`Evidence`）。
///
/// 每个 Finding 必须绑定至少一个 Evidence；Evidence 可以是源码片段、
/// 调用链、污点路径、反编译伪代码、工具输出摘要、崩溃输入、PoC 描述
/// 或 SARIF 位置。`produced_by_*` 保持证据可回溯到创建它的 run/task/
/// 工具调用；`fingerprint` + `evidence_path` 服务 Guardian 质量门与
/// Provenance Gate（出处是神圣的）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    /// Evidence 标识符。
    #[serde(default = "default_evidence_id")]
    pub id: EvidenceId,
    /// 所属 Project。
    pub project_id: ProjectId,
    /// 所属 Mission。
    #[serde(default)]
    pub mission_id: Option<MissionId>,
    /// 所属 Branch。
    #[serde(default)]
    pub branch_id: Option<BranchId>,
    /// 内容类型。
    pub kind: EvidenceKind,
    /// 摘要。
    pub summary: String,
    /// 结构化内容（形态取决于 `kind`，键序 = 插入序）。
    #[serde(default)]
    pub content: Map<String, Value>,
    /// 涉及的物理位置。
    #[serde(default)]
    pub locations: Vec<CodeLocation>,
    /// 支撑的 Fact ID 列表。
    #[serde(default)]
    pub supports_fact_ids: Vec<String>,
    /// 产生该证据的 Task。
    #[serde(default)]
    pub produced_by_task_id: Option<TaskId>,
    /// 产生该证据的 `ToolInvocation`。
    #[serde(default)]
    pub produced_by_tool_invocation_id: Option<ToolInvocationId>,
    /// 产生该证据的 Run。
    #[serde(default)]
    pub run_id: Option<RunId>,
    /// 工件内容指纹（Provenance Gate 校验对象）。
    #[serde(default)]
    pub fingerprint: Option<String>,
    /// 工件落盘路径。
    #[serde(default)]
    pub evidence_path: Option<String>,
    /// 创建时间。
    #[serde(default = "crate::common::utcnow")]
    pub created_at: Timestamp,
}

impl Evidence {
    /// 以 Python 默认值构造（`Evidence(project_id=..., kind=...,
    /// summary=...)`）。
    #[must_use]
    pub fn new(project_id: ProjectId, kind: EvidenceKind, summary: String) -> Self {
        Self {
            id: default_evidence_id(),
            project_id,
            mission_id: None,
            branch_id: None,
            kind,
            summary,
            content: Map::new(),
            locations: Vec::new(),
            supports_fact_ids: Vec::new(),
            produced_by_task_id: None,
            produced_by_tool_invocation_id: None,
            run_id: None,
            fingerprint: None,
            evidence_path: None,
            created_at: utcnow(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timestamp() -> Timestamp {
        "2026-08-24T12:00:00.123456Z"
            .parse()
            .unwrap_or_else(|error| panic!("固定时间必须可解析: {error}"))
    }

    #[test]
    fn evidence_serializes_to_python_wire_bytes() {
        // 期望串逐字节来自 scripts/probe_parity_wire.py 探针输出。
        let expected = concat!(
            r#"{"id":"evd_fix_0001","project_id":"proj_parity","#,
            r#""mission_id":"mission_fix_0001","branch_id":"branch_fix_0001","#,
            r#""kind":"taint_path","summary":"tainted flow to eval","#,
            r#""content":{"zz":"last","aa":"first"},"#,
            r#""locations":[{"artifact":"src/app.py","start_line":10,"#,
            r#""end_line":42,"address":null,"symbol":"handler","#,
            r#""snippet":"eval(req.data)"}],"supports_fact_ids":["fact_1"],"#,
            r#""produced_by_task_id":"task_fix_0001","#,
            r#""produced_by_tool_invocation_id":"tool_fix_0001","#,
            r#""run_id":"run_fix_0001","fingerprint":"sha256:abc123","#,
            r#""evidence_path":"artifacts/evd.json","#,
            r#""created_at":"2026-08-24T12:00:00.123456Z"}"#
        );
        let evidence = Evidence {
            id: EvidenceId::new("evd_fix_0001".to_string()),
            project_id: ProjectId::new("proj_parity".to_string()),
            mission_id: Some(MissionId::new("mission_fix_0001".to_string())),
            branch_id: Some(BranchId::new("branch_fix_0001".to_string())),
            kind: EvidenceKind::TaintPath,
            summary: "tainted flow to eval".to_string(),
            content: [("zz", Value::from("last")), ("aa", Value::from("first"))]
                .into_iter()
                .map(|(key, value)| (key.to_string(), value))
                .collect(),
            locations: vec![CodeLocation {
                artifact: "src/app.py".to_string(),
                start_line: Some(10),
                end_line: Some(42),
                address: None,
                symbol: Some("handler".to_string()),
                snippet: Some("eval(req.data)".to_string()),
            }],
            supports_fact_ids: vec!["fact_1".to_string()],
            produced_by_task_id: Some(TaskId::new("task_fix_0001".to_string())),
            produced_by_tool_invocation_id: Some(ToolInvocationId::new(
                "tool_fix_0001".to_string(),
            )),
            run_id: Some(RunId::new("run_fix_0001".to_string())),
            fingerprint: Some("sha256:abc123".to_string()),
            evidence_path: Some("artifacts/evd.json".to_string()),
            created_at: timestamp(),
        };
        let json = serde_json::to_string(&evidence)
            .unwrap_or_else(|error| panic!("Evidence 序列化不会失败: {error}"));
        assert_eq!(json, expected);

        let back: Evidence = serde_json::from_str(&json)
            .unwrap_or_else(|error| panic!("自身输出必须可解析: {error}"));
        assert_eq!(back, evidence);
    }

    #[test]
    fn evidence_defaults_match_python() {
        let evidence = Evidence::new(
            ProjectId::new("p".to_string()),
            EvidenceKind::ToolOutput,
            "x".to_string(),
        );
        assert!(evidence.id.as_str().starts_with("evd_"));
        assert_eq!(evidence.kind, EvidenceKind::ToolOutput);
        assert!(evidence.content.is_empty());
        assert!(evidence.locations.is_empty());
        assert_eq!(evidence.fingerprint, None);
        assert_eq!(evidence.evidence_path, None);
    }

    #[test]
    fn code_location_deserialize_applies_python_defaults() {
        let json = r#"{"artifact":"main.rs"}"#;
        let location: CodeLocation = serde_json::from_str(json)
            .unwrap_or_else(|error| panic!("pydantic 接受缺省字段，serde 必须同样接受: {error}"));
        assert_eq!(location.artifact, "main.rs");
        assert_eq!(location.start_line, None);
        assert_eq!(location.snippet, None);
    }

    #[test]
    fn evidence_deserialize_rejects_unknown_fields() {
        let result: Result<Evidence, _> = serde_json::from_str(
            r#"{"project_id":"p","kind":"tool_output","summary":"s","surprise":1}"#,
        );
        assert!(result.is_err(), "extra=forbid：未知字段必须被拒绝");
    }
}
