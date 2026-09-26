//! Lynceus 领域模型 —— `server/core/models/` 的 Rust 移植。
//!
//! 移植顺序（阶段 1）：枚举先行，随后是核心对象链实体（Mission → Run →
//! Branch → Task → `ToolInvocation` → Evidence / Finding）。每个 Python
//! `(str, Enum)` 对应一个 Rust 枚举，serde wire 值与 Python 侧逐字节一致，
//! 因此穷尽 `match` 会让“漏处理某个状态”在编译期暴露。
//!
//! # 为什么契约枚举不加 `#[non_exhaustive]`
//!
//! 这些枚举镜像阶段 0 冻结的 API 契约。新增变体是一次明确的契约变更，
//! 此时期望整个 workspace 编译失败、强制所有 match 点更新——穷尽性检查
//! 正是这一保障的来源。`#[non_exhaustive]` 会取消该保障，仅保留给第三方
//! 可自行扩展的枚举（当前不存在）。
//!
//! # 稳定性守护
//!
//! 每个枚举都有 wire 值守护测试：序列化结果与 Python 枚举值逐一比对，
//! 变体数量或取值漂移会让测试先于运行期失败。

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod agent;
pub mod agent_preset;
pub mod asset;
pub mod closure;
pub mod common;
pub mod critique;
pub mod dashboard;
pub mod decision;
pub mod domain;
pub mod event;
pub mod evidence;
pub mod execution;
pub mod fact;
pub mod finding;
pub mod hint;
pub mod ids;
pub mod intake;
pub mod intel;
pub mod intent;
pub mod knowledge;
pub mod lifecycle;
pub mod mission;
pub mod module;
pub mod narrative;
pub mod notification;
pub mod project;
pub mod provider;
pub mod retrieval;
pub mod retest;
pub mod run;
pub mod skill;
pub mod strategy_board;
pub mod tool_invocation;
pub mod tool_retrieval;
pub mod trajectory;
pub mod worker;

pub use agent_preset::{AgentPreset, AgentPresetSource};
pub use agent::{
    ContextCompressionReport, ContextPack, Observation, ObservationType, ReflectorFailureType,
    ReflectorReport, TerminationAssessment, TerminationStatus, WorkerLease, WorkerLeaseStatus,
    WorkerProfile, WorkerType,
};
pub use asset::{
    MissionAsset, MissionAssetSensitivity, MissionAssetSource, MissionAssetType,
    merge_mission_assets, normalize_mission_asset_value,
};
pub use skill::{SkillMissingEntry, SkillUsageRow};
pub use closure::{
    CoverageAssessment, CoverageCategoryStatus, CoverageDomainEntry, CoverageSignalSource,
    EscalationGuardRejection, EscalationGuardVerdict, ExitGateDecision, ExitGateDecisionValue,
    MetacognitionAssessment, MetacognitionDirection, MetacognitionFramework, MetacognitionMode,
    MetacognitionTrigger,
};
pub use common::{StrMap, Timestamp, TimestampParseError, new_id, utcnow};
pub use critique::{ContractElement, CritiqueReport, CritiqueVerdict, ExecutableContract};
pub use dashboard::{
    AssetNodeStats, AssetTypeCount, ConfirmedFindingStats, DashboardStats, MissionActivityStats,
    TokenUsageStats, ToolCallStats,
};
pub use decision::{
    DecisionAnswer, DecisionGate, DecisionGateError, DecisionGateKind, DecisionGateStatus,
    DecisionOption, DecisionSeverity,
};
pub use domain::{AuditDomain, AuditDomainParseError, normalize_audit_domain};
pub use event::{AuditEvent, AuditEventType};
pub use evidence::{CodeLocation, Evidence};
pub use execution::{
    ExecutionArtifact, ExecutionBackendType, ExecutionJob, ExecutionRequest, ExecutionResult,
    ExecutionStatus, ResourceLimits,
};
pub use fact::{Fact, GraphNodeType};
pub use finding::{Finding, FindingError};
pub use hint::Hint;
pub use ids::{
    AgentNarrativeEventId, ArtifactRecordId, BranchId, ContextCompressionReportId, ContextPackId,
    CoverageAssessmentId, CritiqueReportId, DecisionGateId, EscalationGuardVerdictId, EvidenceId,
    ExecutionArtifactId, ExecutionId, ExecutionResultId, ExitGateDecisionId, FactId, FindingId,
    IntentId, KnowledgeCardId, MetacognitionAssessmentId, MissionAssetId, MissionId,
    ModelCapabilityId, ModelInvocationId, ModuleId, ObservationId, ProjectId, ProviderId,
    ProviderRouteId, ReflectorReportId, RetrievalChunkId, RetrievalInvocationId, RetestId, RunId,
    RuntimeSettingId, StrategyBoardSnapshotId, TaskId, TerminationAssessmentId, ToolInvocationId,
    TrajectorySummaryId, UserDirectiveId, WorkerLeaseId, WorkerProfileId,
};
pub use intake::{IntakeError, MaxIntrusiveness, RawInputEnvelope, StructuredConstraintContract};
pub use intel::{
    IntelConfidence, IntelEntity, IntelEntityKind, IntelEntityObservation, IntelEntityRecord,
    IntelEntityStatus, IntelIngestBatch, IntelIngestOutcome, IntelProvenance, IntelQuery,
    IntelQueryType, IntelRawRecord, IntelRelation, IntelRelationKind, IntelRelationObservation,
    IntelRelationRecord, IntelSourceCapabilities, IntelSourceResult,
};
pub use intent::{Intent, IntentStatus};
pub use knowledge::{
    KnowledgeCard, KnowledgeCardDraft, KnowledgeCardKind, KnowledgeCorpusState,
    KnowledgeCorpusStatus, KnowledgeRetrievalQuery, KnowledgeRetrievalResult, MAX_BODY_CHARS,
    MAX_SOURCE_LOCATOR_CHARS, MAX_STRUCTURED_TERMS, MAX_SUMMARY_CHARS,
};
pub use lifecycle::{EvidenceKind, FindingStatus, RunStatus, Severity, TaskStatus, ToolStatus};
pub use mission::Branch;
pub use mission::Mission;
pub use mission::{
    ApprovalMode, BranchStatus, CapabilityDispatch, CapabilityGapSeverity, GoalContractSource,
    GoalContractStatus, GoalOutcomeType, MissionGoalContract, MissionStartResult, MissionStatus,
    UserDirective, UserDirectiveStatus, UserDirectiveType,
};
pub use module::{
    ModuleConfig, ModuleConfigError, ModuleDomain, ModuleProfile, ModuleTransport, ModuleType,
    filter_readonly_tools, is_readonly_denied_tool, normalize_module_domain,
};
pub use narrative::{AgentNarrativeEvent, AgentNarrativeEventKind, CreateAgentNarrativeRequest};
pub use notification::{MissionNotification, NotificationKind};
pub use project::Project;
pub use provider::{
    ModelCapability, ModelInvocation, ModelInvocationStatus, ProviderConfig, ProviderConfigError,
    ProviderHealthResult, ProviderModelDiscoveryResult, ProviderRouteBinding, ProviderRouteError,
    ProviderType,
};
pub use retrieval::{
    ArtifactKind, ArtifactRecord, RetrievalInvocation, RetrievalSourceKind,
    RetrievalStatus, RetrievedEvidence, RuntimeSetting, RuntimeSettingScope,
};
pub use retest::{
    FindingRetest, RetestError, RetestStatus, RetestVerdict, status_after_retest,
};
pub use run::{AgentTask, AuditRun};
pub use strategy_board::{
    BlackboardEntry, BlackboardEntryKind, StrategyBoardDomain, StrategyBoardError,
    StrategyBoardIdea, StrategyBoardIdeaStatus, StrategyBoardMemory, StrategyBoardMemoryKind,
    StrategyBoardOpType, StrategyBoardOperation, StrategyBoardSnapshot,
};
pub use tool_invocation::ToolInvocation;
pub use tool_retrieval::{
    CONFIG_KNOWLEDGE_HINTS, CONFIG_VISIBLE_TOOL_IDS, CompactToolDescriptor, ToolCandidate,
    ToolCandidateAvailability, ToolRetrievalQuery, ToolRetrievalResult, ToolsetSelection,
    ToolsetSelectionMethod,
};
pub use trajectory::{
    CRITICAL_PRESSURE_RATIO, ELEVATED_PRESSURE_RATIO, TokenPressure, TrajectorySummary,
};
pub use worker::{
    MAX_WORKER_EVENT_CHARS, MAX_WORKER_EVENTS_PER_RUN, MAX_WORKER_INSTRUCTION_CHARS,
    MAX_WORKER_SUMMARY_CHARS, ResolvedWorkerConnection, WorkerAvailability, WorkerConnectionView,
    WorkerEvent, WorkerEventKind, WorkerExecutionEnvironment, WorkerInvocation,
    WorkerInvocationPurpose, WorkerProbe, WorkerProfileError, WorkerRun, WorkerRunStatus,
    WorkerRuntimeProfile, WorkerRuntimeType, WorkerUsage, WorkerUsageBreakdown,
    WorkerUsageDailyPoint, WorkerUsageDimension, WorkerUsageGroupedDailyPoint,
    WorkerUsageModelSlice, WorkerUsageSummary,
};

#[cfg(test)]
pub(crate) mod testutil {
    use serde::Serialize;
    use serde::de::DeserializeOwned;
    use std::fmt::Debug;

    /// 断言枚举的 serde wire 值与 Python 枚举完全一致（含顺序与数量）。
    pub(crate) fn assert_wire_values<T: Serialize>(values: &[(T, &str)]) {
        let actual: Vec<String> = values
            .iter()
            .map(|(value, _)| {
                serde_json::to_string(value).expect("枚举序列化在 serde_json 中不会失败")
            })
            .collect();
        let expected: Vec<String> = values
            .iter()
            .map(|(_, wire)| format!("\"{wire}\""))
            .collect();
        assert_eq!(actual, expected, "wire 值与 Python 枚举发生漂移");
    }

    /// 断言序列化 → 反序列化返回同一变体。
    pub(crate) fn assert_roundtrip<T: Serialize + DeserializeOwned + PartialEq + Debug>(value: &T) {
        let json = serde_json::to_string(value).expect("枚举序列化在 serde_json 中不会失败");
        let back: T = serde_json::from_str(&json).expect("已注册的枚举 wire 值总能反序列化");
        assert_eq!(&back, value);
    }
}
