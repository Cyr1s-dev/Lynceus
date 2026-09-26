//! 工具调用审计记录 —— `server/core/models/tool.py` 的移植。

use serde::Deserialize;
use serde::Serialize;
use serde_json::Map;
use serde_json::Value;

use crate::common::Timestamp;
use crate::common::new_id;
use crate::common::utcnow;
use crate::ids::BranchId;
use crate::ids::MissionId;
use crate::ids::ProjectId;
use crate::ids::RunId;
use crate::ids::TaskId;
use crate::ids::ToolInvocationId;
use crate::lifecycle::ToolStatus;

fn default_tool_invocation_id() -> ToolInvocationId {
    ToolInvocationId::new(new_id("tool"))
}

fn default_tool_status() -> ToolStatus {
    ToolStatus::Ok
}

/// ToolInvocation：单次工具调用的审计记录（`ToolInvocation`）。
///
/// 每一次经 `ToolAdapter` 路由的调用（含 MCP 工具）都必须产生一条——
/// **包括失败**。输入/输出只存摘要（不存原始 blob），大工件落盘后经
/// `artifact_paths` 引用。字段顺序、默认值与 wire 格式冻结自 Python 模型。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolInvocation {
    /// 调用标识符。
    #[serde(default = "default_tool_invocation_id")]
    pub id: ToolInvocationId,
    /// 所属 Project（该实体允许游离于 Project 之外）。
    #[serde(default)]
    pub project_id: Option<ProjectId>,
    /// 所属 Mission。
    #[serde(default)]
    pub mission_id: Option<MissionId>,
    /// 所属 Branch。
    #[serde(default)]
    pub branch_id: Option<BranchId>,
    /// 所属 Run。
    #[serde(default)]
    pub run_id: Option<RunId>,
    /// 所属 Task。
    #[serde(default)]
    pub task_id: Option<TaskId>,
    /// 提供该工具的模块（Python 侧为自由字符串引用）。
    #[serde(default)]
    pub module_id: Option<String>,
    /// 发起本次调用的 Worker 身份（工具调用记录 的对应字段）。
    ///
    /// 与 `module_id` 的分工：`module_id` 回答"哪个模块提供的工具"，
    /// `worker_id` 回答"哪个 worker 真的按下了按钮"。二者常常同时有值，
    /// 但只来自 Tool Broker 的 `ExecutionScope.worker_id`——worker 不能
    /// 从请求参数覆盖它。
    #[serde(default)]
    pub worker_id: Option<String>,
    /// 工具名。
    pub tool_name: String,
    /// 输入摘要。
    pub input_summary: String,
    /// 输出摘要。
    #[serde(default)]
    pub output_summary: String,
    /// 调用结果。
    #[serde(default = "default_tool_status")]
    pub status: ToolStatus,
    /// 进程退出码。
    #[serde(default)]
    pub exit_code: Option<i64>,
    /// 耗时（毫秒）。
    #[serde(default)]
    pub duration_ms: Option<i64>,
    /// 落盘工件路径列表。
    #[serde(default)]
    pub artifact_paths: Vec<String>,
    /// 失败原因。
    #[serde(default)]
    pub error: Option<String>,
    /// 附加元数据（键序 = 插入序）。
    #[serde(default)]
    pub metadata: Map<String, Value>,
    /// 开始时间。
    #[serde(default = "crate::common::utcnow")]
    pub started_at: Timestamp,
    /// 结束时间。
    #[serde(default)]
    pub finished_at: Option<Timestamp>,
}

impl ToolInvocation {
    /// 以 Python 默认值构造（`ToolInvocation(tool_name=...,
    /// input_summary=...)`）。
    #[must_use]
    pub fn new(tool_name: String, input_summary: String) -> Self {
        Self {
            id: default_tool_invocation_id(),
            project_id: None,
            mission_id: None,
            branch_id: None,
            run_id: None,
            task_id: None,
            module_id: None,
            worker_id: None,
            tool_name,
            input_summary,
            output_summary: String::new(),
            status: default_tool_status(),
            exit_code: None,
            duration_ms: None,
            artifact_paths: Vec::new(),
            error: None,
            metadata: Map::new(),
            started_at: utcnow(),
            finished_at: None,
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
    fn tool_invocation_serializes_to_python_wire_bytes() {
        // 期望串逐字节来自 scripts/probe_parity_wire.py 探针输出。
        let expected = concat!(
            r#"{"id":"tool_fix_0001","project_id":"proj_parity","#,
            r#""mission_id":"mission_fix_0001","branch_id":"branch_fix_0001","#,
            r#""run_id":"run_fix_0001","task_id":"task_fix_0001","module_id":null,"#,
            r#""worker_id":null,"tool_name":"semgrep","input_summary":"scan src/","#,
            r#""output_summary":"3 findings","status":"ok","exit_code":0,"#,
            r#""duration_ms":1234,"artifact_paths":["artifacts/semgrep.json"],"#,
            r#""error":null,"metadata":{"zz":1,"aa":2},"#,
            r#""started_at":"2026-08-24T12:00:00.123456Z","#,
            r#""finished_at":"2026-08-24T12:00:00.123456Z"}"#
        );
        let invocation = ToolInvocation {
            id: ToolInvocationId::new("tool_fix_0001".to_string()),
            project_id: Some(ProjectId::new("proj_parity".to_string())),
            mission_id: Some(MissionId::new("mission_fix_0001".to_string())),
            branch_id: Some(BranchId::new("branch_fix_0001".to_string())),
            run_id: Some(RunId::new("run_fix_0001".to_string())),
            task_id: Some(TaskId::new("task_fix_0001".to_string())),
            module_id: None,
            worker_id: None,
            tool_name: "semgrep".to_string(),
            input_summary: "scan src/".to_string(),
            output_summary: "3 findings".to_string(),
            status: ToolStatus::Ok,
            exit_code: Some(0),
            duration_ms: Some(1234),
            artifact_paths: vec!["artifacts/semgrep.json".to_string()],
            error: None,
            metadata: [("zz", Value::from(1)), ("aa", Value::from(2))]
                .into_iter()
                .map(|(key, value)| (key.to_string(), value))
                .collect(),
            started_at: timestamp(),
            finished_at: Some(timestamp()),
        };
        let json = serde_json::to_string(&invocation)
            .unwrap_or_else(|error| panic!("ToolInvocation 序列化不会失败: {error}"));
        assert_eq!(json, expected);

        let back: ToolInvocation = serde_json::from_str(&json)
            .unwrap_or_else(|error| panic!("自身输出必须可解析: {error}"));
        assert_eq!(back, invocation);
    }

    #[test]
    fn tool_invocation_defaults_match_python() {
        let invocation = ToolInvocation::new("nuclei".to_string(), "-u https://t".to_string());
        assert!(invocation.id.as_str().starts_with("tool_"));
        assert_eq!(invocation.project_id, None);
        assert_eq!(invocation.status, ToolStatus::Ok);
        assert_eq!(invocation.output_summary, "");
        assert!(invocation.artifact_paths.is_empty());
        assert!(invocation.metadata.is_empty());
        assert!(invocation.finished_at.is_none());
    }

    #[test]
    fn tool_invocation_deserialize_applies_python_defaults_on_missing_fields() {
        let json = r#"{"tool_name":"ffuf","input_summary":"dir scan"}"#;
        let invocation: ToolInvocation = serde_json::from_str(json)
            .unwrap_or_else(|error| panic!("pydantic 接受缺省字段，serde 必须同样接受: {error}"));
        assert_eq!(invocation.tool_name, "ffuf");
        assert_eq!(invocation.status, ToolStatus::Ok);
        assert!(invocation.id.as_str().starts_with("tool_"));
        assert_eq!(invocation.exit_code, None);
    }

    #[test]
    fn tool_invocation_worker_id_is_populated_from_broker_scope() {
        // 工具调用记录的 worker 对应字段：Broker 从 ExecutionScope
        // 带下来，worker 不能从请求参数覆盖。
        let json = r#"{"tool_name":"nuclei","input_summary":"-u t","worker_id":"solver-7"}"#;
        let invocation: ToolInvocation = serde_json::from_str(json)
            .unwrap_or_else(|error| panic!("worker_id 必须可解析: {error}"));
        assert_eq!(invocation.worker_id.as_deref(), Some("solver-7"));

        // 旧 payload 没有 worker_id 也必须能读（字段是新增的，默认 None）。
        let legacy = r#"{"tool_name":"nuclei","input_summary":"-u t"}"#;
        let invocation: ToolInvocation = serde_json::from_str(legacy)
            .unwrap_or_else(|error| panic!("旧 payload 必须可解析: {error}"));
        assert_eq!(invocation.worker_id, None);
    }

    #[test]
    fn tool_invocation_deserialize_rejects_unknown_fields() {
        let result: Result<ToolInvocation, _> =
            serde_json::from_str(r#"{"tool_name":"t","input_summary":"i","surprise":1}"#);
        assert!(result.is_err(), "extra=forbid：未知字段必须被拒绝");
    }
}
