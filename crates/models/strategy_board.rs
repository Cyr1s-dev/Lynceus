//! `StrategyBoard` 模型 —— `server/core/models/strategy_board.py` 的移植。
//!
//! 策略板是给主求解器用的紧凑、模型维护的状态面，刻意与
//! Facts/Evidence/Findings 分离：板面变化可以指导后续工作，但自身绝不
//! 成为审计真值。快照追加只写——更新板面是新建带 `source_snapshot_id`
//! 的快照而非改写旧快照。

use serde::Deserialize;
use serde::Serialize;
use serde_json::Map;
use serde_json::Value;

use crate::common::Timestamp;
use crate::common::new_id;
use crate::ids::BranchId;
use crate::ids::MissionId;
use crate::ids::ModelInvocationId;
use crate::ids::ProjectId;
use crate::ids::ProviderId;
use crate::ids::RunId;
use crate::ids::StrategyBoardSnapshotId;
use crate::ids::TaskId;

/// Mission 白板单条正文的严格字节上限。
pub const MAX_BLACKBOARD_CONTENT_BYTES: usize = 16 * 1024;
/// 幂等键的严格字节上限。
pub const MAX_BLACKBOARD_IDEMPOTENCY_KEY_BYTES: usize = 256;

/// Mission 白板允许的最小记录类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlackboardEntryKind {
    /// 可执行任务说明。
    Task,
    /// 待验证假设。
    Hypothesis,
    /// Worker 观察结果。
    Observation,
    /// ArtifactRecord 引用。
    ArtifactRef,
    /// Evidence 引用。
    EvidenceRef,
    /// 待回答问题。
    Question,
    /// 阻塞原因。
    Blocker,
    /// 协作决定。
    Decision,
    /// 有界摘要。
    Summary,
}

impl BlackboardEntryKind {
    /// 稳定 wire 值。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Task => "task",
            Self::Hypothesis => "hypothesis",
            Self::Observation => "observation",
            Self::ArtifactRef => "artifact_ref",
            Self::EvidenceRef => "evidence_ref",
            Self::Question => "question",
            Self::Blocker => "blocker",
            Self::Decision => "decision",
            Self::Summary => "summary",
        }
    }

    /// 解析稳定 wire 值。
    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw {
            "task" => Ok(Self::Task),
            "hypothesis" => Ok(Self::Hypothesis),
            "observation" => Ok(Self::Observation),
            "artifact_ref" => Ok(Self::ArtifactRef),
            "evidence_ref" => Ok(Self::EvidenceRef),
            "question" => Ok(Self::Question),
            "blocker" => Ok(Self::Blocker),
            "decision" => Ok(Self::Decision),
            "summary" => Ok(Self::Summary),
            other => Err(other.to_string()),
        }
    }

    /// 全部合法 wire 值（顺序稳定）。MCP `blackboard_append` schema 的 enum
    /// 与错误提示都从这里取，避免两处各自硬编码后漂移。
    pub const ALL: [Self; 9] = [
        Self::Task,
        Self::Hypothesis,
        Self::Observation,
        Self::ArtifactRef,
        Self::EvidenceRef,
        Self::Question,
        Self::Blocker,
        Self::Decision,
        Self::Summary,
    ];

    /// 合法 wire 值的逗号分隔列表（人读 / 拒绝错误信息用）。
    ///
    /// worker 首次 `blackboard_append` 很自然会写 `finding` / `fact` /
    /// `result` 这些词——被拒时若只回 `unknown kind 'x'`，它无从得知该用
    /// 什么，只会连撞几次后放弃并报告"白板不可用"。把合法值直接写进错误，
    /// 拒绝即自愈：worker 立刻改用 `observation` / `summary` 重试。
    #[must_use]
    pub fn valid_values() -> String {
        Self::ALL
            .iter()
            .map(|kind| kind.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }

    #[must_use]
    fn is_reference(self) -> bool {
        matches!(self, Self::ArtifactRef | Self::EvidenceRef)
    }
}

fn default_blackboard_entry_id() -> String {
    new_id("bb")
}

/// Mission-scoped、append-only 的共享白板记录。
///
/// 该模型只表达协作事实和 typed reference，不把白板内容提升为
/// Fact/Evidence/Confirmed Finding。`sequence` 由存储层从事件流原子分配，
/// 调用方提供的值会被忽略。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlackboardEntry {
    /// 白板记录标识符。
    #[serde(default = "default_blackboard_entry_id")]
    pub entry_id: String,
    /// 所属 Project。
    pub project_id: ProjectId,
    /// 所属 Mission（白板的隔离边界）。
    pub mission_id: MissionId,
    /// 所属 Run。
    pub run_id: RunId,
    /// 可选 Branch。
    #[serde(default)]
    pub branch_id: Option<BranchId>,
    /// 可选 Task。
    #[serde(default)]
    pub task_id: Option<TaskId>,
    /// 可选 Intent。
    #[serde(default)]
    pub intent_id: Option<String>,
    /// 由服务端 grant 绑定的作者 WorkerRun。
    pub author_worker_run_id: String,
    /// 记录类别。
    pub kind: BlackboardEntryKind,
    /// 普通记录正文；引用记录不复制 Artifact/Evidence 内容。
    #[serde(default)]
    pub content: Option<String>,
    /// ArtifactRecord 引用。
    #[serde(default)]
    pub artifact_id: Option<String>,
    /// Evidence 引用。
    #[serde(default)]
    pub evidence_id: Option<String>,
    /// 引用定位器（如 artifact URI 或证据位置）。
    #[serde(default)]
    pub locator: Option<String>,
    /// 创建时间。
    #[serde(default = "crate::common::utcnow")]
    pub created_at: Timestamp,
    /// 审计事件流中的单调序号，由 Repository 填充。
    #[serde(default)]
    pub sequence: i64,
    /// 同一 Mission 内的 retry 幂等键。
    pub idempotency_key: String,
}

impl BlackboardEntry {
    /// 构造一条未分配 sequence 的白板记录。
    #[must_use]
    pub fn new(
        project_id: ProjectId,
        mission_id: MissionId,
        run_id: RunId,
        author_worker_run_id: String,
        kind: BlackboardEntryKind,
        content: Option<String>,
        artifact_id: Option<String>,
        evidence_id: Option<String>,
        locator: Option<String>,
        idempotency_key: String,
    ) -> Self {
        Self {
            entry_id: default_blackboard_entry_id(),
            project_id,
            mission_id,
            run_id,
            branch_id: None,
            task_id: None,
            intent_id: None,
            author_worker_run_id,
            kind,
            content,
            artifact_id,
            evidence_id,
            locator,
            created_at: crate::common::utcnow(),
            sequence: 0,
            idempotency_key,
        }
    }

    /// 校验 append-only 白板记录的不变量。
    pub fn validate(&self) -> Result<(), String> {
        for (name, value) in [
            ("entry_id", self.entry_id.as_str()),
            ("project_id", self.project_id.as_str()),
            ("mission_id", self.mission_id.as_str()),
            ("run_id", self.run_id.as_str()),
            ("author_worker_run_id", self.author_worker_run_id.as_str()),
            ("idempotency_key", self.idempotency_key.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(format!("blackboard {name} must be non-empty"));
            }
        }
        if self.idempotency_key.len() > MAX_BLACKBOARD_IDEMPOTENCY_KEY_BYTES {
            return Err(format!(
                "blackboard idempotency_key exceeds {MAX_BLACKBOARD_IDEMPOTENCY_KEY_BYTES} bytes"
            ));
        }
        if let Some(content) = self.content.as_deref() {
            if content.trim().is_empty() {
                return Err("blackboard content must be non-empty".to_string());
            }
            if content.len() > MAX_BLACKBOARD_CONTENT_BYTES {
                return Err(format!(
                    "blackboard content exceeds {MAX_BLACKBOARD_CONTENT_BYTES} bytes"
                ));
            }
        }
        let reference_count =
            usize::from(self.artifact_id.is_some()) + usize::from(self.evidence_id.is_some());
        if usize::from(self.content.is_some()) + reference_count != 1 {
            return Err(
                "blackboard entry must contain content or exactly one typed reference".to_string(),
            );
        }
        if self.kind.is_reference() != (reference_count > 0) {
            return Err(format!(
                "blackboard kind '{}' does not match its reference payload",
                self.kind.as_str()
            ));
        }
        if self.kind == BlackboardEntryKind::ArtifactRef && self.evidence_id.is_some()
            || self.kind == BlackboardEntryKind::EvidenceRef && self.artifact_id.is_some()
        {
            return Err("blackboard reference kind does not match reference id".to_string());
        }
        if let Some(locator) = self.locator.as_deref()
            && locator.trim().is_empty()
        {
            return Err("blackboard locator must be non-empty when provided".to_string());
        }
        if self.sequence < 0 {
            return Err("blackboard sequence cannot be negative".to_string());
        }
        Ok(())
    }
}

/// 检索键用途的领域画像（`StrategyBoardDomain`），不是固定剧本。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StrategyBoardDomain {
    /// 通用。
    General,
    /// CTF Web。
    CtfWeb,
    /// CTF Pwn。
    CtfPwn,
    /// CTF Reverse。
    CtfReverse,
    /// CTF Crypto。
    CtfCrypto,
    /// CTF Forensics。
    CtfForensics,
    /// CTF Misc。
    CtfMisc,
    /// CTF Blockchain。
    CtfBlockchain,
    /// 漏洞研究。
    VulnerabilityResearch,
    /// 项目代码审计。
    ProjectCodeAudit,
    /// Web SAST。
    WebSast,
    /// Web DAST。
    WebDast,
    /// Web IAST。
    WebIast,
    /// 二进制静态。
    BinaryStatic,
    /// 二进制动态。
    BinaryDynamic,
    /// 可利用性。
    Exploitability,
    /// 恶意软件分析。
    MalwareAnalysis,
    /// 事件取证。
    IncidentForensics,
    /// 云原生。
    CloudNative,
    /// 供应链。
    SupplyChain,
    /// 修复。
    Remediation,
}

impl StrategyBoardDomain {
    /// wire 值（Python `.value` 镜像，用于提示词与错误文本拼接）。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            StrategyBoardDomain::General => "general",
            StrategyBoardDomain::CtfWeb => "ctf_web",
            StrategyBoardDomain::CtfPwn => "ctf_pwn",
            StrategyBoardDomain::CtfReverse => "ctf_reverse",
            StrategyBoardDomain::CtfCrypto => "ctf_crypto",
            StrategyBoardDomain::CtfForensics => "ctf_forensics",
            StrategyBoardDomain::CtfMisc => "ctf_misc",
            StrategyBoardDomain::CtfBlockchain => "ctf_blockchain",
            StrategyBoardDomain::VulnerabilityResearch => "vulnerability_research",
            StrategyBoardDomain::ProjectCodeAudit => "project_code_audit",
            StrategyBoardDomain::WebSast => "web_sast",
            StrategyBoardDomain::WebDast => "web_dast",
            StrategyBoardDomain::WebIast => "web_iast",
            StrategyBoardDomain::BinaryStatic => "binary_static",
            StrategyBoardDomain::BinaryDynamic => "binary_dynamic",
            StrategyBoardDomain::Exploitability => "exploitability",
            StrategyBoardDomain::MalwareAnalysis => "malware_analysis",
            StrategyBoardDomain::IncidentForensics => "incident_forensics",
            StrategyBoardDomain::CloudNative => "cloud_native",
            StrategyBoardDomain::SupplyChain => "supply_chain",
            StrategyBoardDomain::Remediation => "remediation",
        }
    }

    /// 从 wire 值解析（Python `StrategyBoardDomain(value)` 的可判别对应）。
    ///
    /// # Errors
    ///
    /// wire 值不在枚举内。
    pub fn parse(raw: &str) -> Result<Self, String> {
        Ok(match raw {
            "general" => StrategyBoardDomain::General,
            "ctf_web" => StrategyBoardDomain::CtfWeb,
            "ctf_pwn" => StrategyBoardDomain::CtfPwn,
            "ctf_reverse" => StrategyBoardDomain::CtfReverse,
            "ctf_crypto" => StrategyBoardDomain::CtfCrypto,
            "ctf_forensics" => StrategyBoardDomain::CtfForensics,
            "ctf_misc" => StrategyBoardDomain::CtfMisc,
            "ctf_blockchain" => StrategyBoardDomain::CtfBlockchain,
            "vulnerability_research" => StrategyBoardDomain::VulnerabilityResearch,
            "project_code_audit" => StrategyBoardDomain::ProjectCodeAudit,
            "web_sast" => StrategyBoardDomain::WebSast,
            "web_dast" => StrategyBoardDomain::WebDast,
            "web_iast" => StrategyBoardDomain::WebIast,
            "binary_static" => StrategyBoardDomain::BinaryStatic,
            "binary_dynamic" => StrategyBoardDomain::BinaryDynamic,
            "exploitability" => StrategyBoardDomain::Exploitability,
            "malware_analysis" => StrategyBoardDomain::MalwareAnalysis,
            "incident_forensics" => StrategyBoardDomain::IncidentForensics,
            "cloud_native" => StrategyBoardDomain::CloudNative,
            "supply_chain" => StrategyBoardDomain::SupplyChain,
            "remediation" => StrategyBoardDomain::Remediation,
            other => return Err(other.to_string()),
        })
    }
}

/// 策略想法的生命周期（`StrategyBoardIdeaStatus`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StrategyBoardIdeaStatus {
    /// 待验证。
    Pending,
    /// 验证中。
    Testing,
    /// 已验证。
    Verified,
    /// 已失败。
    Failed,
    /// 已跳过。
    Skipped,
}

/// 压缩持久记忆的分类（`StrategyBoardMemoryKind`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StrategyBoardMemoryKind {
    /// 事实。
    Fact,
    /// 证据。
    Evidence,
    /// 失败边界。
    FailureBoundary,
    /// 约束。
    Constraint,
    /// 工具行为。
    ToolBehavior,
    /// 引导。
    Hint,
    /// 摘要。
    Summary,
}

/// 维护模型发出的结构化操作词表（`StrategyBoardOpType`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StrategyBoardOpType {
    /// 新增想法。
    IdeaAdd,
    /// 更新想法。
    IdeaUpdate,
    /// 删除想法。
    IdeaDelete,
    /// 新增记忆。
    MemoryAdd,
    /// 更新记忆。
    MemoryUpdate,
    /// 删除记忆。
    MemoryDelete,
    /// 板面合并。
    BoardMerge,
    /// 效率提醒。
    EfficiencyReminder,
}

fn default_idea_id() -> String {
    new_id("idea")
}

fn default_idea_status() -> StrategyBoardIdeaStatus {
    StrategyBoardIdeaStatus::Pending
}

fn default_idea_confidence() -> f64 {
    0.5
}

/// 一个值得主求解器测试的候选方向（`StrategyBoardIdea`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrategyBoardIdea {
    /// 想法标识符。
    #[serde(default = "default_idea_id")]
    pub id: String,
    /// 生命周期状态。
    #[serde(default = "default_idea_status")]
    pub status: StrategyBoardIdeaStatus,
    /// 内容（Python 侧约束 `[1, 600]`）。
    pub content: String,
    /// 理由（Python 侧约束 ≤400）。
    #[serde(default)]
    pub reason: Option<String>,
    /// 引用列表（Python 侧约束 ≤12）。
    #[serde(default)]
    pub refs: Vec<String>,
    /// 置信度（`[0.0, 1.0]`，默认 0.5）。
    #[serde(default = "default_idea_confidence")]
    pub confidence: f64,
    /// 创建时间。
    #[serde(default = "crate::common::utcnow")]
    pub created_at: Timestamp,
    /// 更新时间。
    #[serde(default = "crate::common::utcnow")]
    pub updated_at: Timestamp,
}

fn default_memory_id() -> String {
    new_id("mem")
}

fn default_memory_kind() -> StrategyBoardMemoryKind {
    StrategyBoardMemoryKind::Summary
}

fn default_memory_confidence() -> f64 {
    0.7
}

/// 应在上下文压缩中存续的压缩持久记忆（`StrategyBoardMemory`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrategyBoardMemory {
    /// 记忆标识符。
    #[serde(default = "default_memory_id")]
    pub id: String,
    /// 记忆分类。
    #[serde(default = "default_memory_kind")]
    pub kind: StrategyBoardMemoryKind,
    /// 内容（Python 侧约束 `[1, 800]`）。
    pub content: String,
    /// 理由（Python 侧约束 ≤400）。
    #[serde(default)]
    pub reason: Option<String>,
    /// 引用列表（Python 侧约束 ≤12）。
    #[serde(default)]
    pub refs: Vec<String>,
    /// 置信度（`[0.0, 1.0]`，默认 0.7）。
    #[serde(default = "default_memory_confidence")]
    pub confidence: f64,
    /// 创建时间。
    #[serde(default = "crate::common::utcnow")]
    pub created_at: Timestamp,
    /// 更新时间。
    #[serde(default = "crate::common::utcnow")]
    pub updated_at: Timestamp,
}

/// 一条经校验的板面操作（`StrategyBoardOperation`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrategyBoardOperation {
    /// 操作类型。
    #[serde(rename = "type")]
    pub op_type: StrategyBoardOpType,
    /// 目标标识符。
    #[serde(default)]
    pub id: Option<String>,
    /// 目标状态。
    #[serde(default)]
    pub status: Option<StrategyBoardIdeaStatus>,
    /// 目标记忆分类。
    #[serde(default)]
    pub kind: Option<StrategyBoardMemoryKind>,
    /// 内容（Python 侧约束 ≤1000）。
    #[serde(default)]
    pub content: Option<String>,
    /// 理由（Python 侧约束 ≤500）。
    #[serde(default)]
    pub reason: Option<String>,
    /// 引用列表（Python 侧约束 ≤12）。
    #[serde(default)]
    pub refs: Vec<String>,
    /// 附加元数据（键序 = 插入序）。
    #[serde(default)]
    pub metadata: Map<String, Value>,
}

fn default_snapshot_id() -> StrategyBoardSnapshotId {
    StrategyBoardSnapshotId::new(new_id("board"))
}

fn default_snapshot_version() -> i64 {
    1
}

fn default_domain_profile() -> StrategyBoardDomain {
    StrategyBoardDomain::General
}

fn default_snapshot_trigger() -> String {
    "manual".to_string()
}

fn default_snapshot_created_by() -> String {
    "strategy_board_maintainer".to_string()
}

/// 一个时间点的板面快照（`StrategyBoardSnapshot`）。
///
/// Python 侧模型校验器保证 ideas / memory 的 id 唯一；Rust 侧在
/// [`StrategyBoardSnapshot::validate`] 提供同等检查。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrategyBoardSnapshot {
    /// 快照标识符。
    #[serde(default = "default_snapshot_id")]
    pub id: StrategyBoardSnapshotId,
    /// 所属 Project。
    pub project_id: ProjectId,
    /// 所属 Run。
    #[serde(default)]
    pub run_id: Option<RunId>,
    /// 来源快照 ID。
    #[serde(default)]
    pub source_snapshot_id: Option<StrategyBoardSnapshotId>,
    /// 版本号（≥1）。
    #[serde(default = "default_snapshot_version")]
    pub version: i64,
    /// 领域画像。
    #[serde(default = "default_domain_profile")]
    pub domain_profile: StrategyBoardDomain,
    /// 摘要（Python 侧约束 ≤1000）。
    #[serde(default)]
    pub summary: String,
    /// 候选想法（Python 侧约束 ≤8）。
    #[serde(default)]
    pub ideas: Vec<StrategyBoardIdea>,
    /// 压缩持久记忆（Python 侧约束 ≤12）。
    #[serde(default)]
    pub memory: Vec<StrategyBoardMemory>,
    /// 效率提醒（Python 侧约束 ≤4）。
    #[serde(default)]
    pub efficiency_reminders: Vec<String>,
    /// 已应用的操作（Python 侧约束 ≤4）。
    #[serde(default)]
    pub applied_ops: Vec<StrategyBoardOperation>,
    /// 关联知识卡片 ID 列表（Python 侧约束 ≤16）。
    #[serde(default)]
    pub knowledge_card_ids: Vec<String>,
    /// Provider 标识符。
    #[serde(default)]
    pub provider_id: Option<ProviderId>,
    /// 模型调用标识符。
    #[serde(default)]
    pub model_invocation_id: Option<ModelInvocationId>,
    /// 触发来源（Python 侧约束 ≤120）。
    #[serde(default = "default_snapshot_trigger")]
    pub trigger: String,
    /// 创建者（Python 侧约束 ≤120）。
    #[serde(default = "default_snapshot_created_by")]
    pub created_by: String,
    /// 创建时间。
    #[serde(default = "crate::common::utcnow")]
    pub created_at: Timestamp,
    /// 附加元数据（键序 = 插入序）。
    #[serde(default)]
    pub metadata: Map<String, Value>,
}

/// 快照构造/维护错误（Python 侧 `ValueError` 的类型化对应）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StrategyBoardError {
    /// ideas 的 id 出现重复。
    #[error("strategy board idea ids must be unique")]
    DuplicateIdeaId,
    /// memory 的 id 出现重复。
    #[error("strategy board memory ids must be unique")]
    DuplicateMemoryId,
    /// ideas 超出板上限（8）。
    #[error("strategy board ideas limit exceeded")]
    IdeasLimitExceeded,
    /// memory 超出板上限（12）。
    #[error("strategy board memory limit exceeded")]
    MemoryLimitExceeded,
    /// 操作引用的 idea 不存在。
    #[error("unknown strategy board idea: {0}")]
    UnknownIdea(String),
    /// 操作引用的 memory 不存在。
    #[error("unknown strategy board memory: {0}")]
    UnknownMemory(String),
    /// 操作缺少非空 content。
    #[error("{op_type} requires non-empty content")]
    MissingContent {
        /// 缺内容的操作类型（wire 值）。
        op_type: String,
    },
    /// 操作缺少目标 id。
    #[error("{op_type} requires id")]
    MissingId {
        /// 缺 id 的操作类型（wire 值）。
        op_type: String,
    },
    /// 一批操作超出上限（4）。
    #[error("List should have at most 4 items after validation, not {0}")]
    TooManyOperations(usize),
}

impl StrategyBoardOpType {
    /// wire 值（Python `.value` 镜像，用于错误文本拼接）。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            StrategyBoardOpType::IdeaAdd => "idea_add",
            StrategyBoardOpType::IdeaUpdate => "idea_update",
            StrategyBoardOpType::IdeaDelete => "idea_delete",
            StrategyBoardOpType::MemoryAdd => "memory_add",
            StrategyBoardOpType::MemoryUpdate => "memory_update",
            StrategyBoardOpType::MemoryDelete => "memory_delete",
            StrategyBoardOpType::BoardMerge => "board_merge",
            StrategyBoardOpType::EfficiencyReminder => "efficiency_reminder",
        }
    }
}

impl StrategyBoardSnapshot {
    /// 以 Python 默认值构造（`StrategyBoardSnapshot(project_id=...)`，
    /// 其余字段取模型默认）。
    #[must_use]
    pub fn new(project_id: ProjectId) -> Self {
        Self {
            id: default_snapshot_id(),
            project_id,
            run_id: None,
            source_snapshot_id: None,
            version: default_snapshot_version(),
            domain_profile: default_domain_profile(),
            summary: String::new(),
            ideas: Vec::new(),
            memory: Vec::new(),
            efficiency_reminders: Vec::new(),
            applied_ops: Vec::new(),
            knowledge_card_ids: Vec::new(),
            provider_id: None,
            model_invocation_id: None,
            trigger: default_snapshot_trigger(),
            created_by: default_snapshot_created_by(),
            created_at: crate::common::utcnow(),
            metadata: Map::new(),
        }
    }

    /// 校验 ideas / memory 的 id 唯一性（Python `_ensure_unique_item_ids`）。
    ///
    /// # Errors
    ///
    /// 任一 id 重复时返回对应的 [`StrategyBoardError`]。
    pub fn validate(&self) -> Result<(), StrategyBoardError> {
        let idea_count = self
            .ideas
            .iter()
            .map(|idea| idea.id.as_str())
            .collect::<std::collections::HashSet<_>>()
            .len();
        if idea_count != self.ideas.len() {
            return Err(StrategyBoardError::DuplicateIdeaId);
        }
        let memory_count = self
            .memory
            .iter()
            .map(|item| item.id.as_str())
            .collect::<std::collections::HashSet<_>>()
            .len();
        if memory_count != self.memory.len() {
            return Err(StrategyBoardError::DuplicateMemoryId);
        }
        Ok(())
    }
}

fn default_kcard_id() -> String {
    new_id("kcard")
}

fn default_kcard_priority() -> i64 {
    50
}

/// 注入维护者提示词的小型检索卡（`StrategyBoardKnowledgeCard`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrategyBoardKnowledgeCard {
    /// 卡片标识符。
    #[serde(default = "default_kcard_id")]
    pub id: String,
    /// 标题（Python 侧约束 `[1, 120]`）。
    pub title: String,
    /// 内容（Python 侧约束 `[1, 1200]`）。
    pub content: String,
    /// 适配的领域画像列表。
    #[serde(default)]
    pub domain_profiles: Vec<StrategyBoardDomain>,
    /// 检索标签（Python 侧约束 ≤12）。
    #[serde(default)]
    pub tags: Vec<String>,
    /// 优先级（`[0, 100]`，默认 50）。
    #[serde(default = "default_kcard_priority")]
    pub priority: i64,
}

impl StrategyBoardKnowledgeCard {
    /// 以标题与内容构造（其余字段取模型默认）。
    #[must_use]
    pub fn new(title: String, content: String) -> Self {
        Self {
            id: default_kcard_id(),
            title,
            content,
            domain_profiles: Vec::new(),
            tags: Vec::new(),
            priority: default_kcard_priority(),
        }
    }
}

/// 一批有界的板面操作（`StrategyBoardOps`，单批 ≤4 条）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrategyBoardOps {
    /// 操作列表（Python 侧约束 ≤4）。
    #[serde(default)]
    pub ops: Vec<StrategyBoardOperation>,
}

impl StrategyBoardOps {
    /// 空操作批（`{"ops": []}`——维护者判定无需变更时的合法回执）。
    #[must_use]
    pub fn empty() -> Self {
        Self { ops: Vec::new() }
    }

    /// 校验批量上限（Python `Field(max_length=4)` 的构造期镜像）。
    ///
    /// # Errors
    ///
    /// 超过 4 条时返回 [`StrategyBoardError::TooManyOperations`]。
    pub fn validate(&self) -> Result<(), StrategyBoardError> {
        if self.ops.len() > 4 {
            return Err(StrategyBoardError::TooManyOperations(self.ops.len()));
        }
        Ok(())
    }
}

/// 提示词里的一条 chat 消息（Python `list[dict[str, str]]` 元素的镜像）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrategyBoardPromptMessage {
    /// 角色（`system` / `user`）。
    pub role: String,
    /// 消息内容。
    pub content: String,
}

impl StrategyBoardPromptMessage {
    /// 构造一条消息。
    #[must_use]
    pub fn new(role: &str, content: String) -> Self {
        Self {
            role: role.to_string(),
            content,
        }
    }
}

/// 返回给客户端做外部模型执行的提示词载荷
/// （`StrategyBoardPromptPayload`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrategyBoardPromptPayload {
    /// 所属 Project。
    pub project_id: ProjectId,
    /// 所属 Run。
    #[serde(default)]
    pub run_id: Option<RunId>,
    /// 领域画像。
    #[serde(default = "default_domain_profile")]
    pub domain_profile: StrategyBoardDomain,
    /// 基线快照标识符。
    #[serde(default)]
    pub snapshot_id: Option<StrategyBoardSnapshotId>,
    /// 系统提示词。
    pub system_prompt: String,
    /// 用户载荷（键序 = 插入序）。
    pub user_payload: Map<String, Value>,
    /// 消息序列（system + user）。
    pub messages: Vec<StrategyBoardPromptMessage>,
    /// 参与提示的知识卡 ID。
    #[serde(default)]
    pub knowledge_card_ids: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::utcnow;
    use crate::testutil::assert_wire_values;

    fn project_id() -> ProjectId {
        ProjectId::new("proj_test".to_string())
    }

    #[test]
    fn strategy_board_domain_matches_python_wire_values() {
        assert_wire_values(&[
            (StrategyBoardDomain::General, "general"),
            (StrategyBoardDomain::CtfWeb, "ctf_web"),
            (StrategyBoardDomain::CtfPwn, "ctf_pwn"),
            (StrategyBoardDomain::CtfReverse, "ctf_reverse"),
            (StrategyBoardDomain::CtfCrypto, "ctf_crypto"),
            (StrategyBoardDomain::CtfForensics, "ctf_forensics"),
            (StrategyBoardDomain::CtfMisc, "ctf_misc"),
            (StrategyBoardDomain::CtfBlockchain, "ctf_blockchain"),
            (
                StrategyBoardDomain::VulnerabilityResearch,
                "vulnerability_research",
            ),
            (StrategyBoardDomain::ProjectCodeAudit, "project_code_audit"),
            (StrategyBoardDomain::WebSast, "web_sast"),
            (StrategyBoardDomain::WebDast, "web_dast"),
            (StrategyBoardDomain::WebIast, "web_iast"),
            (StrategyBoardDomain::BinaryStatic, "binary_static"),
            (StrategyBoardDomain::BinaryDynamic, "binary_dynamic"),
            (StrategyBoardDomain::Exploitability, "exploitability"),
            (StrategyBoardDomain::MalwareAnalysis, "malware_analysis"),
            (StrategyBoardDomain::IncidentForensics, "incident_forensics"),
            (StrategyBoardDomain::CloudNative, "cloud_native"),
            (StrategyBoardDomain::SupplyChain, "supply_chain"),
            (StrategyBoardDomain::Remediation, "remediation"),
        ]);
    }

    #[test]
    fn strategy_board_lifecycle_enums_match_python_wire_values() {
        assert_wire_values(&[
            (StrategyBoardIdeaStatus::Pending, "pending"),
            (StrategyBoardIdeaStatus::Testing, "testing"),
            (StrategyBoardIdeaStatus::Verified, "verified"),
            (StrategyBoardIdeaStatus::Failed, "failed"),
            (StrategyBoardIdeaStatus::Skipped, "skipped"),
        ]);
        assert_wire_values(&[
            (StrategyBoardMemoryKind::Fact, "fact"),
            (StrategyBoardMemoryKind::Evidence, "evidence"),
            (StrategyBoardMemoryKind::FailureBoundary, "failure_boundary"),
            (StrategyBoardMemoryKind::Constraint, "constraint"),
            (StrategyBoardMemoryKind::ToolBehavior, "tool_behavior"),
            (StrategyBoardMemoryKind::Hint, "hint"),
            (StrategyBoardMemoryKind::Summary, "summary"),
        ]);
        assert_wire_values(&[
            (StrategyBoardOpType::IdeaAdd, "idea_add"),
            (StrategyBoardOpType::IdeaUpdate, "idea_update"),
            (StrategyBoardOpType::IdeaDelete, "idea_delete"),
            (StrategyBoardOpType::MemoryAdd, "memory_add"),
            (StrategyBoardOpType::MemoryUpdate, "memory_update"),
            (StrategyBoardOpType::MemoryDelete, "memory_delete"),
            (StrategyBoardOpType::BoardMerge, "board_merge"),
            (
                StrategyBoardOpType::EfficiencyReminder,
                "efficiency_reminder",
            ),
        ]);
    }

    #[test]
    fn snapshot_defaults_match_python() {
        let snapshot = StrategyBoardSnapshot {
            id: default_snapshot_id(),
            project_id: project_id(),
            run_id: None,
            source_snapshot_id: None,
            version: 1,
            domain_profile: StrategyBoardDomain::General,
            summary: String::new(),
            ideas: Vec::new(),
            memory: Vec::new(),
            efficiency_reminders: Vec::new(),
            applied_ops: Vec::new(),
            knowledge_card_ids: Vec::new(),
            provider_id: None,
            model_invocation_id: None,
            trigger: "manual".to_string(),
            created_by: "strategy_board_maintainer".to_string(),
            created_at: utcnow(),
            metadata: Map::new(),
        };
        assert!(snapshot.id.as_str().starts_with("board_"));
        assert!(snapshot.validate().is_ok());
    }

    #[test]
    fn snapshot_rejects_duplicate_item_ids() {
        let idea = StrategyBoardIdea {
            id: "idea_dup".to_string(),
            status: StrategyBoardIdeaStatus::Pending,
            content: "c".to_string(),
            reason: None,
            refs: Vec::new(),
            confidence: 0.5,
            created_at: utcnow(),
            updated_at: utcnow(),
        };
        let snapshot = StrategyBoardSnapshot {
            ideas: vec![idea.clone(), idea],
            ..empty_snapshot()
        };
        assert_eq!(
            snapshot.validate(),
            Err(StrategyBoardError::DuplicateIdeaId)
        );
        let memory = StrategyBoardMemory {
            id: "mem_dup".to_string(),
            kind: StrategyBoardMemoryKind::Summary,
            content: "c".to_string(),
            reason: None,
            refs: Vec::new(),
            confidence: 0.7,
            created_at: utcnow(),
            updated_at: utcnow(),
        };
        let snapshot = StrategyBoardSnapshot {
            memory: vec![memory.clone(), memory],
            ..empty_snapshot()
        };
        assert_eq!(
            snapshot.validate(),
            Err(StrategyBoardError::DuplicateMemoryId)
        );
    }

    #[test]
    fn ops_batch_cap_and_wire_form() {
        let empty = StrategyBoardOps::empty();
        assert!(empty.ops.is_empty());
        assert_eq!(
            serde_json::to_string(&empty).expect("序列化不会失败"),
            "{\"ops\":[]}"
        );

        let five: Vec<StrategyBoardOperation> = (0..5)
            .map(|_| StrategyBoardOperation {
                op_type: StrategyBoardOpType::IdeaAdd,
                id: None,
                status: None,
                kind: None,
                content: Some("c".to_string()),
                reason: None,
                refs: Vec::new(),
                metadata: Map::new(),
            })
            .collect();
        let oversized = StrategyBoardOps { ops: five };
        assert_eq!(
            oversized.validate(),
            Err(StrategyBoardError::TooManyOperations(5))
        );
    }

    #[test]
    fn knowledge_card_defaults_match_python() {
        let card = StrategyBoardKnowledgeCard::new("标题".to_string(), "内容".to_string());
        assert_eq!(card.priority, 50);
        assert!(card.domain_profiles.is_empty());
        assert!(card.tags.is_empty());
        assert!(card.id.starts_with("kcard_"));

        let raw = serde_json::json!({
            "title": "t",
            "content": "c",
            "domain_profiles": ["vulnerability_research"],
            "tags": ["xss"],
            "priority": 80
        });
        let parsed: StrategyBoardKnowledgeCard =
            serde_json::from_value(raw).expect("合法 wire JSON 必须可解析");
        assert_eq!(
            parsed.domain_profiles,
            [StrategyBoardDomain::VulnerabilityResearch]
        );
        assert_eq!(parsed.priority, 80);
    }

    #[test]
    fn prompt_payload_roundtrips_with_insertion_order() {
        let mut user_payload = Map::new();
        user_payload.insert("trigger".to_string(), Value::String("manual".to_string()));
        user_payload.insert(
            "response_contract".to_string(),
            serde_json::json!({"max_ops": 4}),
        );
        let payload = StrategyBoardPromptPayload {
            project_id: project_id(),
            run_id: Some(RunId::new("run_1".to_string())),
            domain_profile: StrategyBoardDomain::CtfWeb,
            snapshot_id: Some(default_snapshot_id()),
            system_prompt: "protocol".to_string(),
            user_payload: user_payload.clone(),
            messages: vec![
                StrategyBoardPromptMessage::new("system", "protocol".to_string()),
                StrategyBoardPromptMessage::new("user", "{}".to_string()),
            ],
            knowledge_card_ids: vec!["kcard_1".to_string()],
        };
        let wire = serde_json::to_value(&payload).expect("序列化不会失败");
        let keys: Vec<&str> = wire["user_payload"]
            .as_object()
            .expect("user_payload 必须是对象")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, ["trigger", "response_contract"]);
        let back: StrategyBoardPromptPayload =
            serde_json::from_value(wire).expect("wire 形态必须可往返");
        assert_eq!(back, payload);
    }

    fn empty_snapshot() -> StrategyBoardSnapshot {
        StrategyBoardSnapshot {
            id: default_snapshot_id(),
            project_id: project_id(),
            run_id: None,
            source_snapshot_id: None,
            version: 1,
            domain_profile: StrategyBoardDomain::General,
            summary: String::new(),
            ideas: Vec::new(),
            memory: Vec::new(),
            efficiency_reminders: Vec::new(),
            applied_ops: Vec::new(),
            knowledge_card_ids: Vec::new(),
            provider_id: None,
            model_invocation_id: None,
            trigger: "manual".to_string(),
            created_by: "strategy_board_maintainer".to_string(),
            created_at: utcnow(),
            metadata: Map::new(),
        }
    }
}
