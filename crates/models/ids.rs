//! 强类型实体标识符（newtype）。
//!
//! 每个实体 ID 独立成类型，把“拿 `MissionId` 当 `RunId` 用”这类混用从运行期
//! 数据损坏变成编译错误。ID 在 wire 上是不透明字符串，此层不解释其内部
//! 结构（前缀、生成规则由存储层负责）。

use std::fmt;

use serde::{Deserialize, Serialize};

macro_rules! entity_id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// 包装原始标识符字符串。
            #[must_use]
            pub fn new(value: String) -> Self {
                Self(value)
            }

            /// 原始标识符字符串。
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }
    };
}

entity_id!(
    /// Project 实体标识符（`server/core/models/project.py`）。
    ProjectId
);
entity_id!(
    /// Mission 实体标识符（`server/core/models/mission.py`）。
    MissionId
);
entity_id!(
    /// `AuditRun` 实体标识符（`server/core/models/run.py`）。
    RunId
);
entity_id!(
    /// Branch 实体标识符（`server/core/models/mission.py`）。
    BranchId
);
entity_id!(
    /// `AgentTask` 实体标识符（`server/core/models/execution.py`）。
    TaskId
);
entity_id!(
    /// `ToolInvocation` 实体标识符（`server/core/models/tool.py`）。
    ToolInvocationId
);
entity_id!(
    /// Evidence 实体标识符（`server/core/models/evidence.py`）。
    EvidenceId
);
entity_id!(
    /// Finding 实体标识符（`server/core/models/finding.py`）。
    FindingId
);
entity_id!(
    /// Project 实体标识符之外的图节点：Fact（`server/core/models/fact.py`）。
    FactId
);
entity_id!(
    /// Intent 实体标识符（`server/core/models/intent.py`）。
    IntentId
);
entity_id!(
    /// `DecisionGate` 实体标识符（`server/core/models/decision.py`）。
    DecisionGateId
);
entity_id!(
    /// Observation 实体标识符（`server/core/models/agent.py`）。
    ObservationId
);
entity_id!(
    /// `WorkerLease` 实体标识符（`server/core/models/agent.py`）。
    WorkerLeaseId
);
entity_id!(
    /// `TerminationAssessment` 实体标识符（`server/core/models/agent.py`）。
    TerminationAssessmentId
);
entity_id!(
    /// `CoverageAssessment` 实体标识符（`server/core/models/closure.py`）。
    CoverageAssessmentId
);
entity_id!(
    /// `MetacognitionAssessment` 实体标识符（`server/core/models/closure.py`）。
    MetacognitionAssessmentId
);
entity_id!(
    /// `ExitGateDecision` 实体标识符（`server/core/models/closure.py`）。
    ExitGateDecisionId
);
entity_id!(
    /// `EscalationGuardVerdict` 实体标识符（`server/core/models/closure.py`）。
    EscalationGuardVerdictId
);
entity_id!(
    /// `CritiqueReport` 实体标识符（`server/core/models/critique.py`）。
    CritiqueReportId
);
entity_id!(
    /// `ModuleConfig` 实体标识符（`server/core/models/module.py`）。
    ModuleId
);
entity_id!(
    /// `KnowledgeCard` 实体标识符（`server/core/models/knowledge.py`）。
    KnowledgeCardId
);
entity_id!(
    /// `StrategyBoardSnapshot` 实体标识符（`server/core/models/strategy_board.py`）。
    StrategyBoardSnapshotId
);
entity_id!(
    /// `ProviderConfig` 实体标识符（`server/core/models/provider.py`）。
    ProviderId
);
entity_id!(
    /// `ModelCapability` 实体标识符（`server/core/models/provider.py`）。
    ModelCapabilityId
);
entity_id!(
    /// `ProviderRouteBinding` 实体标识符（`server/core/models/provider.py`）。
    ProviderRouteId
);
entity_id!(
    /// `ModelInvocation` 实体标识符（`server/core/models/provider.py`）。
    ModelInvocationId
);
entity_id!(
    /// `TrajectorySummary` 实体标识符（`server/core/models/trajectory.py`）。
    TrajectorySummaryId
);
entity_id!(
    /// `AgentNarrativeEvent` 实体标识符（`server/core/models/agent.py`）。
    AgentNarrativeEventId
);
entity_id!(
    /// `ContextPack` 实体标识符（`server/core/models/agent.py`）。
    ContextPackId
);
entity_id!(
    /// `ContextCompressionReport` 实体标识符（`server/core/models/agent.py`）。
    ContextCompressionReportId
);
entity_id!(
    /// `WorkerProfile` 实体标识符（`server/core/models/agent.py`）。
    WorkerProfileId
);
entity_id!(
    /// `RetrievalChunk` 实体标识符（`server/core/models/retrieval.py`）。
    RetrievalChunkId
);
entity_id!(
    /// `RetrievalInvocation` 实体标识符（`server/core/models/retrieval.py`）。
    RetrievalInvocationId
);
entity_id!(
    /// `MissionAsset` 实体标识符（`server/core/models/asset.py`）。
    MissionAssetId
);
entity_id!(
    /// `UserDirective` 实体标识符（`server/core/models/mission.py`）。
    UserDirectiveId
);
entity_id!(
    /// `ReflectorReport` 实体标识符（`server/core/models/agent.py`）。
    ReflectorReportId
);
entity_id!(
    /// `ArtifactRecord` 实体标识符（`server/core/models/retrieval.py`）。
    ArtifactRecordId
);
entity_id!(
    /// `RuntimeSetting` 实体标识符（`server/core/models/retrieval.py`）。
    RuntimeSettingId
);
entity_id!(
    /// `ExecutionRequest` / `ExecutionJob` 共享的执行标识符。
    ExecutionId
);
entity_id!(
    /// 执行后端返回结果的标识符。
    ExecutionResultId
);
entity_id!(
    /// 执行过程中产出的工件引用标识符。
    ExecutionArtifactId
);
entity_id!(
    /// `FindingRetest` 实体标识符（漏洞复测记录）。
    RetestId
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_serialize_transparently() {
        let id = MissionId::new("m_123".to_string());
        assert_eq!(
            serde_json::to_string(&id).expect("透明序列化不会失败"),
            "\"m_123\""
        );
        let back: MissionId = serde_json::from_str("\"m_123\"").expect("合法 wire 值总能反序列化");
        assert_eq!(back.as_str(), "m_123");
    }

    #[test]
    fn ids_display_the_raw_value() {
        let id = RunId::new("run-abc".to_string());
        assert_eq!(id.to_string(), "run-abc");
        assert_eq!(RunId::from("run-abc".to_string()), id);
    }
}
