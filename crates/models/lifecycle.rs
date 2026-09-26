//! Run / Task / Finding / Evidence / `ToolInvocation` 生命周期枚举 ——
//! `server/core/models/common.py` 枚举的移植。

use serde::{Deserialize, Serialize};

/// `AuditRun` 生命周期（`RunStatus`），支持暂停/恢复/评审/报告。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    /// 已创建，尚未启动。
    Pending,
    /// 运行中。
    Running,
    /// 等待人工决策。
    WaitingForDecision,
    /// 已暂停。
    Paused,
    /// 人工评审阶段。
    Reviewing,
    /// 报告生成阶段。
    Reporting,
    /// 已完成。
    Completed,
    /// 已失败。
    Failed,
    /// 已取消。
    Cancelled,
}

impl RunStatus {
    /// wire 值（Python `.value` 镜像，用于文本拼接）。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            RunStatus::Pending => "pending",
            RunStatus::Running => "running",
            RunStatus::WaitingForDecision => "waiting_for_decision",
            RunStatus::Paused => "paused",
            RunStatus::Reviewing => "reviewing",
            RunStatus::Reporting => "reporting",
            RunStatus::Completed => "completed",
            RunStatus::Failed => "failed",
            RunStatus::Cancelled => "cancelled",
        }
    }
}

/// `AgentTask` 生命周期（`TaskStatus`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    /// 已入队。
    Queued,
    /// 执行中。
    Running,
    /// 等待人工决策。
    WaitingForDecision,
    /// 已成功。
    Succeeded,
    /// 已失败。
    Failed,
    /// 已取消。
    Cancelled,
}

impl TaskStatus {
    /// wire 值（Python `.value` 镜像，用于文本拼接）。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            TaskStatus::Queued => "queued",
            TaskStatus::Running => "running",
            TaskStatus::WaitingForDecision => "waiting_for_decision",
            TaskStatus::Succeeded => "succeeded",
            TaskStatus::Failed => "failed",
            TaskStatus::Cancelled => "cancelled",
        }
    }
}

/// Finding 的 Observer 评审生命周期（`FindingStatus`）。
///
/// `Fixed` 由漏洞复测（verdict=fixed 且会话正常收口）或人工 triage 写入，
/// 是唯一表示"已修复"的终态；其余状态保持 Python 侧原始集合不变。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingStatus {
    /// Solver 提出，尚未评审。
    Candidate,
    /// Observer 已确认：具备证据与 source/sink。
    Confirmed,
    /// Observer 需要更多上下文/验证。
    NeedsReview,
    /// 误报。
    FalsePositive,
    /// 与已有 Finding 重复。
    Duplicate,
    /// 观察到异常但尚未确认为漏洞（README：降级不丢弃）。
    Gap,
    /// 观察到行为但尚未定性。
    Phenomenon,
    /// 已修复（复测结论 fixed 或人工 triage）。
    Fixed,
}

impl FindingStatus {
    /// wire 值（Python `.value` 镜像，用于文本拼接）。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            FindingStatus::Candidate => "candidate",
            FindingStatus::Confirmed => "confirmed",
            FindingStatus::NeedsReview => "needs_review",
            FindingStatus::FalsePositive => "false_positive",
            FindingStatus::Duplicate => "duplicate",
            FindingStatus::Gap => "gap",
            FindingStatus::Phenomenon => "phenomenon",
            FindingStatus::Fixed => "fixed",
        }
    }

    /// 全部取值（人工 triage 下拉与契约校验共用同一份集合）。
    pub const ALL: [FindingStatus; 8] = [
        FindingStatus::Candidate,
        FindingStatus::Confirmed,
        FindingStatus::NeedsReview,
        FindingStatus::FalsePositive,
        FindingStatus::Duplicate,
        FindingStatus::Gap,
        FindingStatus::Phenomenon,
        FindingStatus::Fixed,
    ];

    /// 从 wire 值解析；未知值返回 `None`（调用方转 422）。
    #[must_use]
    pub fn from_wire(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|status| status.as_str() == value)
    }

    /// 是否终态（不再参与复测/继续跟踪）。
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            FindingStatus::FalsePositive | FindingStatus::Duplicate | FindingStatus::Fixed
        )
    }
}

/// 归一化严重级别（`Severity`），可直接映射 SARIF level。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// 信息级。
    Info,
    /// 低危。
    Low,
    /// 中危。
    Medium,
    /// 高危。
    High,
    /// 严重。
    Critical,
}

impl Severity {
    /// wire 值（Python `.value` 镜像，用于文本拼接）。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Severity::Info => "info",
            Severity::Low => "low",
            Severity::Medium => "medium",
            Severity::High => "high",
            Severity::Critical => "critical",
        }
    }

    /// 全部取值（SARIF level 映射与 triage 校验共用）。
    pub const ALL: [Severity; 5] = [
        Severity::Info,
        Severity::Low,
        Severity::Medium,
        Severity::High,
        Severity::Critical,
    ];

    /// 从 wire 值解析；未知值返回 `None`（调用方转 422）。
    #[must_use]
    pub fn from_wire(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|severity| severity.as_str() == value)
    }
}

/// Evidence 记录承载的内容类型（`EvidenceKind`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    /// 源码片段。
    SourceSnippet,
    /// 调用链。
    CallChain,
    /// 污点路径。
    TaintPath,
    /// 反编译伪代码。
    DecompiledPseudocode,
    /// 工具原始输出。
    ToolOutput,
    /// 崩溃输入。
    CrashInput,
    /// `PoC` 描述。
    PocDescription,
    /// SARIF 位置。
    SarifLocation,
}

impl EvidenceKind {
    /// wire 值（Python `.value` 镜像，用于文本拼接）。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            EvidenceKind::SourceSnippet => "source_snippet",
            EvidenceKind::CallChain => "call_chain",
            EvidenceKind::TaintPath => "taint_path",
            EvidenceKind::DecompiledPseudocode => "decompiled_pseudocode",
            EvidenceKind::ToolOutput => "tool_output",
            EvidenceKind::CrashInput => "crash_input",
            EvidenceKind::PocDescription => "poc_description",
            EvidenceKind::SarifLocation => "sarif_location",
        }
    }
}

/// 单次 `ToolInvocation` 的结果（`ToolStatus`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus {
    /// 成功。
    Ok,
    /// 等待人工确认（审批模式触发）。
    WaitingForConfirmation,
    /// 工具执行出错。
    Error,
    /// 执行超时。
    Timeout,
    /// 被适配器策略 / 沙箱拒绝。
    Denied,
}

impl ToolStatus {
    /// wire 值（Python `.value` 镜像，用于文本拼接）。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            ToolStatus::Ok => "ok",
            ToolStatus::WaitingForConfirmation => "waiting_for_confirmation",
            ToolStatus::Error => "error",
            ToolStatus::Timeout => "timeout",
            ToolStatus::Denied => "denied",
        }
    }
}

/// wire 值冻结守护：与 `server/core/models/common.py` 逐值比对。
#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{assert_roundtrip, assert_wire_values};

    #[test]
    fn lifecycle_enums_match_python_wire_values() {
        assert_wire_values(&[
            (RunStatus::Pending, "pending"),
            (RunStatus::Running, "running"),
            (RunStatus::WaitingForDecision, "waiting_for_decision"),
            (RunStatus::Paused, "paused"),
            (RunStatus::Reviewing, "reviewing"),
            (RunStatus::Reporting, "reporting"),
            (RunStatus::Completed, "completed"),
            (RunStatus::Failed, "failed"),
            (RunStatus::Cancelled, "cancelled"),
        ]);
        assert_wire_values(&[
            (TaskStatus::Queued, "queued"),
            (TaskStatus::Running, "running"),
            (TaskStatus::WaitingForDecision, "waiting_for_decision"),
            (TaskStatus::Succeeded, "succeeded"),
            (TaskStatus::Failed, "failed"),
            (TaskStatus::Cancelled, "cancelled"),
        ]);
        assert_wire_values(&[
            (FindingStatus::Candidate, "candidate"),
            (FindingStatus::Confirmed, "confirmed"),
            (FindingStatus::NeedsReview, "needs_review"),
            (FindingStatus::FalsePositive, "false_positive"),
            (FindingStatus::Duplicate, "duplicate"),
            (FindingStatus::Gap, "gap"),
            (FindingStatus::Phenomenon, "phenomenon"),
        ]);
        assert_wire_values(&[
            (Severity::Info, "info"),
            (Severity::Low, "low"),
            (Severity::Medium, "medium"),
            (Severity::High, "high"),
            (Severity::Critical, "critical"),
        ]);
        assert_wire_values(&[
            (EvidenceKind::SourceSnippet, "source_snippet"),
            (EvidenceKind::CallChain, "call_chain"),
            (EvidenceKind::TaintPath, "taint_path"),
            (EvidenceKind::DecompiledPseudocode, "decompiled_pseudocode"),
            (EvidenceKind::ToolOutput, "tool_output"),
            (EvidenceKind::CrashInput, "crash_input"),
            (EvidenceKind::PocDescription, "poc_description"),
            (EvidenceKind::SarifLocation, "sarif_location"),
        ]);
        assert_wire_values(&[
            (ToolStatus::Ok, "ok"),
            (
                ToolStatus::WaitingForConfirmation,
                "waiting_for_confirmation",
            ),
            (ToolStatus::Error, "error"),
            (ToolStatus::Timeout, "timeout"),
            (ToolStatus::Denied, "denied"),
        ]);
    }

    #[test]
    fn lifecycle_enums_roundtrip_through_json() {
        for status in [
            RunStatus::Pending,
            RunStatus::Running,
            RunStatus::WaitingForDecision,
            RunStatus::Paused,
            RunStatus::Reviewing,
            RunStatus::Reporting,
            RunStatus::Completed,
            RunStatus::Failed,
            RunStatus::Cancelled,
        ] {
            assert_roundtrip(&status);
        }
        for status in [
            FindingStatus::Candidate,
            FindingStatus::Confirmed,
            FindingStatus::NeedsReview,
            FindingStatus::FalsePositive,
            FindingStatus::Duplicate,
            FindingStatus::Gap,
            FindingStatus::Phenomenon,
        ] {
            assert_roundtrip(&status);
        }
        for kind in [
            EvidenceKind::SourceSnippet,
            EvidenceKind::CallChain,
            EvidenceKind::TaintPath,
            EvidenceKind::DecompiledPseudocode,
            EvidenceKind::ToolOutput,
            EvidenceKind::CrashInput,
            EvidenceKind::PocDescription,
            EvidenceKind::SarifLocation,
        ] {
            assert_roundtrip(&kind);
        }
    }
}
