//! 检索审计记录与运行时设置模型 —— `server/core/models/retrieval.py` 的移植。
//!
//! M5d 起纳入 `ArtifactRecord`（引擎的 `_commit_result` 与
//! `_runtime_progress_snapshot` 依赖它）。
//!
//! 2026-09-22 删除了 chunk 检索子系统：`RetrievalChunk` 实体、其词法打分
//! （`storage/retrieval_search.rs`）与 `/retrieval/*` HTTP 面。理由是
//! Finding 已通过 `evidence_ids` 自带证据链，独立 chunk 索引是重复的事实源。
//! 保留下来的 `RetrievalInvocation` 服务于 solver bootstrap 的 telemetry
//! （knowledge bootstrap / task profile / tool retrieval 三条写入路径），
//! 它记录的是"检索发生过什么"，不再承载"检索到了什么"。

use serde::Deserialize;
use serde::Serialize;
use serde_json::Map;
use serde_json::Value;

use crate::common::Timestamp;
use crate::common::new_id;
use crate::common::utcnow;
use crate::ids::ArtifactRecordId;
use crate::ids::EvidenceId;
use crate::ids::FindingId;
use crate::ids::ModelInvocationId;
use crate::ids::ProjectId;
use crate::ids::RetrievalChunkId;
use crate::ids::RetrievalInvocationId;
use crate::ids::RunId;
use crate::ids::RuntimeSettingId;
use crate::ids::TaskId;
use crate::ids::ToolInvocationId;

/// 可被切块检索为审计上下文的来源类别（`RetrievalSourceKind`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalSourceKind {
    /// 知识卡。
    KnowledgeCard,
    /// 源码文件。
    SourceFile,
    /// SARIF 报告。
    Sarif,
    /// 证据。
    Evidence,
    /// 发现。
    Finding,
    /// 工具调用。
    ToolInvocation,
    /// 模型调用。
    ModelInvocation,
    /// IDA 反编译输出。
    IdaDecompile,
    /// 流量工件。
    TrafficArtifact,
    /// 工件。
    Artifact,
    /// 策略板。
    StrategyBoard,
    /// 上下文包。
    ContextPack,
}

impl RetrievalSourceKind {
    /// wire 值（Python `.value` 镜像，用于文本拼接）。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            RetrievalSourceKind::KnowledgeCard => "knowledge_card",
            RetrievalSourceKind::SourceFile => "source_file",
            RetrievalSourceKind::Sarif => "sarif",
            RetrievalSourceKind::Evidence => "evidence",
            RetrievalSourceKind::Finding => "finding",
            RetrievalSourceKind::ToolInvocation => "tool_invocation",
            RetrievalSourceKind::ModelInvocation => "model_invocation",
            RetrievalSourceKind::IdaDecompile => "ida_decompile",
            RetrievalSourceKind::TrafficArtifact => "traffic_artifact",
            RetrievalSourceKind::Artifact => "artifact",
            RetrievalSourceKind::StrategyBoard => "strategy_board",
            RetrievalSourceKind::ContextPack => "context_pack",
        }
    }
}

/// 一次检索尝试的结果（`RetrievalStatus`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalStatus {
    /// 命中。
    Hit,
    /// 范围内无 chunk。
    Empty,
    /// 有候选但分数不达标。
    LowScore,
    /// 超时。
    Timeout,
    /// 出错。
    Error,
    /// 检索设施不可用。
    Unavailable,
}

/// 运行时设置的作用域（`server/core/models/retrieval.py`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeSettingScope {
    /// 全局默认设置。
    #[default]
    Global,
    /// Project 级覆盖。
    Project,
    /// Run 级覆盖。
    Run,
}

/// 持久化的业务/运行时设置。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeSetting {
    /// 设置标识。
    #[serde(default = "default_runtime_setting_id")]
    pub id: RuntimeSettingId,
    /// 规范化后的键名。
    pub key: String,
    /// JSON 对象值。
    #[serde(default)]
    pub value: Map<String, Value>,
    /// 作用域。
    #[serde(default)]
    pub scope: RuntimeSettingScope,
    /// Project 作用域。
    #[serde(default)]
    pub project_id: Option<ProjectId>,
    /// Run 作用域。
    #[serde(default)]
    pub run_id: Option<RunId>,
    /// 描述。
    #[serde(default)]
    pub description: String,
    /// 更新者。
    #[serde(default = "default_runtime_setting_updated_by")]
    pub updated_by: String,
    /// 创建时间。
    #[serde(default = "utcnow")]
    pub created_at: Timestamp,
    /// 更新时间。
    #[serde(default = "utcnow")]
    pub updated_at: Timestamp,
}

fn default_runtime_setting_id() -> RuntimeSettingId {
    RuntimeSettingId::new(new_id("setting"))
}

fn default_runtime_setting_updated_by() -> String {
    "system".to_string()
}

impl RuntimeSetting {
    /// 以 Python 模型相同的默认值构造设置，并规范化 key。
    ///
    /// # Errors
    /// key 为空白。
    pub fn new(key: &str, value: Map<String, Value>) -> Result<Self, String> {
        let normalized = key.trim().to_lowercase();
        if normalized.is_empty() {
            return Err("setting key must not be blank".to_string());
        }
        let now = utcnow();
        Ok(Self {
            id: default_runtime_setting_id(),
            key: normalized,
            value,
            scope: RuntimeSettingScope::Global,
            project_id: None,
            run_id: None,
            description: String::new(),
            updated_by: default_runtime_setting_updated_by(),
            created_at: now,
            updated_at: now,
        })
    }
}

/// `RetrievalInvocation` 的检索参数默认值（与已删除的 `RetrievalQuery`
/// 共享同一组 Python 默认值，故保留在此）。
fn default_query_purpose() -> String {
    "manual".to_string()
}

fn default_top_k() -> i64 {
    8
}

fn default_fetch_multiplier() -> i64 {
    4
}

fn default_min_score() -> f64 {
    0.1
}

fn default_query_token_budget() -> i64 {
    2048
}

/// 一条检索到的证据承载上下文项（`RetrievedEvidence`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetrievedEvidence {
    /// chunk 标识符。
    pub chunk_id: RetrievalChunkId,
    /// 来源类别。
    pub source_kind: RetrievalSourceKind,
    /// 来源记录 id。
    pub source_id: String,
    /// 得分。
    pub score: f64,
    /// 名次（1 起）。
    pub rank: i64,
    /// 片段。
    pub snippet: String,
    /// 标题。
    #[serde(default)]
    pub title: String,
    /// 关联 `ArtifactRecord`。
    #[serde(default)]
    pub artifact_record_id: Option<String>,
    /// 工件 URI。
    #[serde(default)]
    pub artifact_uri: Option<String>,
    /// 工件 SHA-256。
    #[serde(default)]
    pub artifact_sha256: Option<String>,
    /// 关联 Evidence。
    #[serde(default)]
    pub evidence_id: Option<EvidenceId>,
    /// 关联 Finding。
    #[serde(default)]
    pub finding_id: Option<FindingId>,
    /// 关联 `ToolInvocation`。
    #[serde(default)]
    pub tool_invocation_id: Option<ToolInvocationId>,
    /// 关联 `ModelInvocation`。
    #[serde(default)]
    pub model_invocation_id: Option<ModelInvocationId>,
    /// 位置描述（键序 = 插入序）。
    #[serde(default)]
    pub location: Map<String, Value>,
    /// 附加元数据（键序 = 插入序）。
    #[serde(default)]
    pub metadata: Map<String, Value>,
}

fn default_invocation_id() -> RetrievalInvocationId {
    RetrievalInvocationId::new(new_id("retr"))
}

fn default_invocation_status() -> RetrievalStatus {
    RetrievalStatus::Empty
}

fn default_reason() -> String {
    String::new()
}

fn default_candidate_count() -> i64 {
    0
}

fn default_max_score() -> f64 {
    0.0
}

fn default_cached() -> bool {
    false
}

/// 一次检索尝试的追加只读审计记录（`RetrievalInvocation`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetrievalInvocation {
    /// 调用标识符。
    #[serde(default = "default_invocation_id")]
    pub id: RetrievalInvocationId,
    /// 所属 Project。
    pub project_id: ProjectId,
    /// 所属 Run。
    #[serde(default)]
    pub run_id: Option<RunId>,
    /// 关联 Task。
    #[serde(default)]
    pub task_id: Option<TaskId>,
    /// 用途。
    #[serde(default = "default_query_purpose")]
    pub purpose: String,
    /// 查询文本。
    pub query_text: String,
    /// 查询哈希。
    #[serde(default)]
    pub query_hash: Option<String>,
    /// 结果状态。
    #[serde(default = "default_invocation_status")]
    pub status: RetrievalStatus,
    /// 状态说明。
    #[serde(default = "default_reason")]
    pub reason: String,
    /// 返回条数上限（Python 侧约束 ≥1）。
    #[serde(default = "default_top_k")]
    pub top_k: i64,
    /// 候选拉取倍率（Python 侧约束 ≥1）。
    #[serde(default = "default_fetch_multiplier")]
    pub fetch_multiplier: i64,
    /// 分数下限（Python 侧约束 ≥0）。
    #[serde(default = "default_min_score")]
    pub min_score: f64,
    /// token 预算（Python 侧约束 ≥1）。
    #[serde(default = "default_query_token_budget")]
    pub token_budget: i64,
    /// 通过 scope/过滤的候选数。
    #[serde(default = "default_candidate_count")]
    pub candidate_count: i64,
    /// 通过评分下限的候选数。
    #[serde(default = "default_candidate_count")]
    pub filtered_count: i64,
    /// 最高得分。
    #[serde(default = "default_max_score")]
    pub max_score: f64,
    /// 命中列表。
    #[serde(default)]
    pub retrieved: Vec<RetrievedEvidence>,
    /// 耗时（毫秒，Python 侧约束 ≥0）。
    #[serde(default)]
    pub duration_ms: Option<i64>,
    /// 是否命中缓存。
    #[serde(default = "default_cached")]
    pub cached: bool,
    /// 错误说明。
    #[serde(default)]
    pub error: Option<String>,
    /// 创建时间。
    #[serde(default = "crate::common::utcnow")]
    pub created_at: Timestamp,
}

impl RetrievalInvocation {
    /// 以 Python 默认值构造（`RetrievalInvocation(project_id=...,
    /// query_text=...)`）。
    #[must_use]
    pub fn new(project_id: ProjectId, query_text: String) -> Self {
        Self {
            id: default_invocation_id(),
            project_id,
            run_id: None,
            task_id: None,
            purpose: default_query_purpose(),
            query_text,
            query_hash: None,
            status: default_invocation_status(),
            reason: default_reason(),
            top_k: default_top_k(),
            fetch_multiplier: default_fetch_multiplier(),
            min_score: default_min_score(),
            token_budget: default_query_token_budget(),
            candidate_count: default_candidate_count(),
            filtered_count: default_candidate_count(),
            max_score: default_max_score(),
            retrieved: Vec::new(),
            duration_ms: None,
            cached: default_cached(),
            error: None,
            created_at: utcnow(),
        }
    }
}

/// 一次执行可产出的工件类别（`ArtifactKind`）。
///
/// Python 侧定义在 `execution.py`；M5d 范围只拉取引擎路径
/// （`_commit_result` / `_runtime_progress_snapshot`）用到的该枚举，
/// 其余执行面模型（`Artifact` / `ExecutionRequest`）随 M6 执行控制面移植。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    /// 原始输出。
    RawOutput,
    /// 日志。
    Log,
    /// SARIF 报告。
    Sarif,
    /// HAR 流量。
    Har,
    /// 截图。
    Screenshot,
    /// 崩溃转储。
    Crash,
    /// 跟踪。
    Trace,
    /// 其它。
    Other,
}

impl ArtifactKind {
    /// wire 值（Python `.value` 镜像）。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            ArtifactKind::RawOutput => "raw_output",
            ArtifactKind::Log => "log",
            ArtifactKind::Sarif => "sarif",
            ArtifactKind::Har => "har",
            ArtifactKind::Screenshot => "screenshot",
            ArtifactKind::Crash => "crash",
            ArtifactKind::Trace => "trace",
            ArtifactKind::Other => "other",
        }
    }
}

fn default_artifact_kind() -> ArtifactKind {
    ArtifactKind::Other
}

fn default_artifact_record_id() -> ArtifactRecordId {
    ArtifactRecordId::new(new_id("art"))
}

fn default_storage_backend() -> String {
    "local_filesystem".to_string()
}

fn default_artifact_summary() -> String {
    String::new()
}

/// 存储在审计行之外的高增长工件的持久元数据（`ArtifactRecord`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactRecord {
    /// 工件记录标识符。
    #[serde(default = "default_artifact_record_id")]
    pub id: ArtifactRecordId,
    /// 所属 Project（该实体允许游离）。
    #[serde(default)]
    pub project_id: Option<ProjectId>,
    /// 所属 Run。
    #[serde(default)]
    pub run_id: Option<RunId>,
    /// 关联 Task。
    #[serde(default)]
    pub task_id: Option<TaskId>,
    /// 产出工件的 `ToolInvocation`。
    #[serde(default)]
    pub tool_invocation_id: Option<ToolInvocationId>,
    /// 关联 `ModelInvocation`。
    #[serde(default)]
    pub model_invocation_id: Option<ModelInvocationId>,
    /// 关联 Evidence。
    #[serde(default)]
    pub evidence_id: Option<EvidenceId>,
    /// 关联 Finding。
    #[serde(default)]
    pub finding_id: Option<FindingId>,
    /// 工件类别。
    #[serde(default = "default_artifact_kind")]
    pub kind: ArtifactKind,
    /// 工件 URI（Python 侧约束非空）。
    pub uri: String,
    /// 存储后端。
    #[serde(default = "default_storage_backend")]
    pub storage_backend: String,
    /// 摘要。
    #[serde(default = "default_artifact_summary")]
    pub summary: String,
    /// MIME 类型。
    #[serde(default)]
    pub mime_type: Option<String>,
    /// 大小（字节，Python 侧约束 ≥0）。
    #[serde(default)]
    pub size_bytes: Option<i64>,
    /// 内容 SHA-256。
    #[serde(default)]
    pub sha256: Option<String>,
    /// 附加元数据（键序 = 插入序）。
    #[serde(default)]
    pub metadata: Map<String, Value>,
    /// 创建时间。
    #[serde(default = "crate::common::utcnow")]
    pub created_at: Timestamp,
}

impl ArtifactRecord {
    /// 以 Python 默认值构造（`ArtifactRecord(uri=...)`）。
    #[must_use]
    pub fn new(uri: String) -> Self {
        Self {
            id: default_artifact_record_id(),
            project_id: None,
            run_id: None,
            task_id: None,
            tool_invocation_id: None,
            model_invocation_id: None,
            evidence_id: None,
            finding_id: None,
            kind: default_artifact_kind(),
            uri,
            storage_backend: default_storage_backend(),
            summary: default_artifact_summary(),
            mime_type: None,
            size_bytes: None,
            sha256: None,
            metadata: Map::new(),
            created_at: utcnow(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::assert_wire_values;

    fn project() -> ProjectId {
        ProjectId::new("proj_1".to_string())
    }

    #[test]
    fn retrieval_enums_match_python_wire_values() {
        assert_wire_values(&[
            (RetrievalSourceKind::KnowledgeCard, "knowledge_card"),
            (RetrievalSourceKind::SourceFile, "source_file"),
            (RetrievalSourceKind::Sarif, "sarif"),
            (RetrievalSourceKind::Evidence, "evidence"),
            (RetrievalSourceKind::Finding, "finding"),
            (RetrievalSourceKind::ToolInvocation, "tool_invocation"),
            (RetrievalSourceKind::ModelInvocation, "model_invocation"),
            (RetrievalSourceKind::IdaDecompile, "ida_decompile"),
            (RetrievalSourceKind::TrafficArtifact, "traffic_artifact"),
            (RetrievalSourceKind::Artifact, "artifact"),
            (RetrievalSourceKind::StrategyBoard, "strategy_board"),
            (RetrievalSourceKind::ContextPack, "context_pack"),
        ]);
        assert_wire_values(&[
            (RetrievalStatus::Hit, "hit"),
            (RetrievalStatus::Empty, "empty"),
            (RetrievalStatus::LowScore, "low_score"),
            (RetrievalStatus::Timeout, "timeout"),
            (RetrievalStatus::Error, "error"),
            (RetrievalStatus::Unavailable, "unavailable"),
        ]);
        assert_wire_values(&[
            (RuntimeSettingScope::Global, "global"),
            (RuntimeSettingScope::Project, "project"),
            (RuntimeSettingScope::Run, "run"),
        ]);
    }

    #[test]
    fn runtime_setting_defaults_and_key_normalization_match_python() {
        let setting =
            RuntimeSetting::new(" Example_Key ", Map::new()).expect("nonblank key must normalize");
        assert_eq!(setting.key, "example_key");
        assert_eq!(setting.scope, RuntimeSettingScope::Global);
        assert_eq!(setting.updated_by, "system");
        assert!(RuntimeSetting::new("   ", Map::new()).is_err());
    }

    #[test]
    fn invocation_defaults_match_python() {
        let invocation = RetrievalInvocation::new(project(), "q".to_string());
        assert!(invocation.id.as_str().starts_with("retr_"));
        assert_eq!(invocation.status, RetrievalStatus::Empty);
        assert_eq!(invocation.candidate_count, 0);
        assert!((invocation.max_score - 0.0).abs() < f64::EPSILON);
        assert!(!invocation.cached);
        assert!(invocation.duration_ms.is_none());
    }

    #[test]
    fn artifact_record_defaults_match_python() {
        let record = ArtifactRecord::new("file:///tmp/out.txt".to_string());
        assert!(record.id.as_str().starts_with("art_"));
        assert!(record.project_id.is_none());
        assert_eq!(record.kind, ArtifactKind::Other);
        assert_eq!(record.storage_backend, "local_filesystem");
        assert_eq!(record.summary, "");
        assert!(record.sha256.is_none());
        assert!(record.metadata.is_empty());
    }

    #[test]
    fn artifact_kinds_match_python_wire_values() {
        assert_wire_values(&[
            (ArtifactKind::RawOutput, "raw_output"),
            (ArtifactKind::Log, "log"),
            (ArtifactKind::Sarif, "sarif"),
            (ArtifactKind::Har, "har"),
            (ArtifactKind::Screenshot, "screenshot"),
            (ArtifactKind::Crash, "crash"),
            (ArtifactKind::Trace, "trace"),
            (ArtifactKind::Other, "other"),
        ]);
    }

    #[test]
    fn retrieved_evidence_roundtrips() {
        let item = RetrievedEvidence {
            chunk_id: RetrievalChunkId::new("chunk_1".to_string()),
            source_kind: RetrievalSourceKind::Sarif,
            source_id: "sarif_1".to_string(),
            score: 0.75,
            rank: 1,
            snippet: "snip".to_string(),
            title: "t".to_string(),
            artifact_record_id: None,
            artifact_uri: Some("file:///x".to_string()),
            artifact_sha256: None,
            evidence_id: None,
            finding_id: None,
            tool_invocation_id: None,
            model_invocation_id: None,
            location: Map::new(),
            metadata: Map::new(),
        };
        let json = serde_json::to_string(&item).expect("序列化不会失败");
        let back: RetrievedEvidence =
            serde_json::from_str(&json).unwrap_or_else(|error| panic!("往返必须可解析: {error}"));
        assert_eq!(back, item);
    }
}
