//! 漏洞复测记录 —— 一个漏洞的多次复测尝试，每次一条不可变记录。
//!
//! 复测是**一等公民**：一个漏洞可以发起多次复测，每次复测一条
//! 不可变记录，带独立结论（verdict）、摘要与证据，并和一个会话绑定。
//! Lynceus 这里保持同样的形状，但把"谁来跑复测"落到既有的只读顾问
//! worker（`IntakeService::advise`）上，不新增第二套执行器。
//!
//! 关键不变量（都是踩过坑才加的）：
//! 1. **同一 Finding 同时只能有一条未收口的复测**（`pending` / `running`）。
//!    重复点击返回已有记录，不会并发拉起两个 worker。
//! 2. **结论只能写一次**：第二次同内容写入是幂等重放，不同内容直接拒绝
//!    （`RetestError::NotRunning`），避免把已封存的结论覆盖掉。
//! 3. **只有 `fixed` 会改写漏洞状态**，而且只在复测正常收口时。中断/失败
//!    优先于已暂存的结论——一个被打断的复测不能看起来像"修好了"。
//! 4. `reproduced` / `inconclusive` **不动漏洞状态**：重新打开一个已修复
//!    漏洞是人工决定，不该由一次自动复测替 Operator 拍板。

use serde::Deserialize;
use serde::Serialize;

use crate::common::Timestamp;
use crate::common::utcnow;
use crate::ids::FindingId;
use crate::ids::MissionId;
use crate::ids::ProjectId;
use crate::ids::RetestId;
use crate::lifecycle::FindingStatus;

/// 复测结论（ 的三值集合）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetestVerdict {
    /// 仍然可以复现。
    Reproduced,
    /// 已修复。
    Fixed,
    /// 无法确认（检查了但证据不足/被阻塞）。
    Inconclusive,
}

impl RetestVerdict {
    /// wire 值。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            RetestVerdict::Reproduced => "reproduced",
            RetestVerdict::Fixed => "fixed",
            RetestVerdict::Inconclusive => "inconclusive",
        }
    }

    /// 全部取值。
    pub const ALL: [RetestVerdict; 3] = [
        RetestVerdict::Reproduced,
        RetestVerdict::Fixed,
        RetestVerdict::Inconclusive,
    ];

    /// 从 wire 值解析。
    #[must_use]
    pub fn from_wire(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|verdict| verdict.as_str() == value)
    }
}

/// 复测记录状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetestStatus {
    /// 已创建，worker 还没开始。
    Pending,
    /// worker 正在跑。
    Running,
    /// 正常收口（可能带结论，也可能没有——没有会被判 failed）。
    Completed,
    /// 失败/中断。
    Failed,
    /// 被服务重启或人工中止。
    Stopped,
}

impl RetestStatus {
    /// wire 值。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            RetestStatus::Pending => "pending",
            RetestStatus::Running => "running",
            RetestStatus::Completed => "completed",
            RetestStatus::Failed => "failed",
            RetestStatus::Stopped => "stopped",
        }
    }

    /// 是否未收口（占用"同时只能一条"的名额）。
    #[must_use]
    pub const fn is_open(self) -> bool {
        matches!(self, RetestStatus::Pending | RetestStatus::Running)
    }
}

/// 复测记录（落库实体）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FindingRetest {
    /// 复测记录标识符。
    pub id: RetestId,
    /// 所属 Project。
    pub project_id: ProjectId,
    /// 所属 Mission。
    pub mission_id: MissionId,
    /// 复测目标 Finding。
    pub finding_id: FindingId,
    /// 状态。
    pub status: RetestStatus,
    /// 结论；未判定时为空串。
    #[serde(default)]
    pub verdict: String,
    /// 发起时填的补充说明。
    #[serde(default)]
    pub notes: String,
    /// 结论摘要。
    #[serde(default)]
    pub summary: String,
    /// 结论依据（本次复测实际检查的内容）。
    #[serde(default)]
    pub evidence: String,
    /// 失败/中断原因。
    #[serde(default)]
    pub error: String,
    /// 复测用的模型（顾问 worker 的 runtime 标识）。
    #[serde(default)]
    pub model: Option<String>,
    /// 上下文来源说明。
    ///
    /// 复测 worker 拿到的证据有两条来路：后端从仓储读出来后**内联进 prompt**
    /// 的快照，以及 worker 自己通过只读 MCP grant 去读白板。前者一定可用，
    /// 后者需要 grant 签发成功。这个字段如实记录本次是哪一种——
    /// 只拿到内联快照的结论置信度不同，差别必须留在记录里，不能默默抹掉。
    #[serde(default)]
    pub context_source: Option<String>,
    /// 原始评估文本（未解析出结论时保留原文，便于人工回看）。
    #[serde(default)]
    pub assessment: String,
    /// 发起时间。
    pub created_at: Timestamp,
    /// 开始时间。
    #[serde(default)]
    pub started_at: Option<Timestamp>,
    /// 收口时间。
    #[serde(default)]
    pub finished_at: Option<Timestamp>,
}

impl FindingRetest {
    /// 以默认值构造一条 `pending` 记录。
    #[must_use]
    pub fn new(
        project_id: ProjectId,
        mission_id: MissionId,
        finding_id: FindingId,
        notes: String,
    ) -> Self {
        let now = utcnow();
        Self {
            id: RetestId::new(crate::common::new_id("retest")),
            project_id,
            mission_id,
            finding_id,
            status: RetestStatus::Pending,
            verdict: String::new(),
            notes,
            summary: String::new(),
            evidence: String::new(),
            error: String::new(),
            model: None,
            context_source: None,
            assessment: String::new(),
            created_at: now.clone(),
            started_at: None,
            finished_at: None,
        }
    }

    /// 解析出的结论；未判定返回 `None`。
    #[must_use]
    pub fn parsed_verdict(&self) -> Option<RetestVerdict> {
        RetestVerdict::from_wire(&self.verdict)
    }

    /// 是否已收口。
    #[must_use]
    pub fn is_finished(&self) -> bool {
        !self.status.is_open()
    }
}

/// 复测错误。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RetestError {
    /// 该复测已收口或尚未开始（对应 `ErrRetestNotRunning`）。
    #[error("this retest has already finished or has not started yet")]
    NotRunning,
    /// 结论不是三值之一。
    #[error("verdict must be one of reproduced / fixed / inconclusive")]
    BadVerdict,
    /// 摘要或证据为空。
    #[error("summary and evidence must not be empty")]
    EmptyConclusion,
    /// 终态不合法。
    #[error("invalid terminal retest status")]
    BadTerminalStatus,
}

/// 判定漏洞状态是否需要因复测结论而改写。
///
/// 只有"正常收口 + 结论 fixed"才改写，且目标状态写死为
/// [`FindingStatus::Fixed`]。其余组合一律不动——重新打开漏洞是人工决定。
#[must_use]
pub const fn status_after_retest(status: RetestStatus, verdict: Option<RetestVerdict>) -> Option<FindingStatus> {
    match (status, verdict) {
        (RetestStatus::Completed, Some(RetestVerdict::Fixed)) => Some(FindingStatus::Fixed),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verdict_wire_round_trip() {
        for verdict in RetestVerdict::ALL {
            assert_eq!(RetestVerdict::from_wire(verdict.as_str()), Some(verdict));
        }
        assert_eq!(RetestVerdict::from_wire("maybe"), None);
    }

    #[test]
    fn only_completed_fixed_rewrites_finding_status() {
        assert_eq!(
            status_after_retest(RetestStatus::Completed, Some(RetestVerdict::Fixed)),
            Some(FindingStatus::Fixed)
        );
        // 中断/失败优先于已暂存的结论。
        assert_eq!(
            status_after_retest(RetestStatus::Failed, Some(RetestVerdict::Fixed)),
            None
        );
        assert_eq!(
            status_after_retest(RetestStatus::Stopped, Some(RetestVerdict::Fixed)),
            None
        );
        // reproduced / inconclusive 绝不自动改状态。
        assert_eq!(
            status_after_retest(RetestStatus::Completed, Some(RetestVerdict::Reproduced)),
            None
        );
        assert_eq!(
            status_after_retest(RetestStatus::Completed, Some(RetestVerdict::Inconclusive)),
            None
        );
        assert_eq!(status_after_retest(RetestStatus::Completed, None), None);
    }

    #[test]
    fn open_statuses_occupy_the_single_active_slot() {
        assert!(RetestStatus::Pending.is_open());
        assert!(RetestStatus::Running.is_open());
        assert!(!RetestStatus::Completed.is_open());
        assert!(!RetestStatus::Failed.is_open());
        assert!(!RetestStatus::Stopped.is_open());
    }

    #[test]
    fn retest_record_serializes_with_empty_optional_fields() {
        let retest = FindingRetest::new(
            crate::ids::ProjectId::new("proj_x".to_string()),
            crate::ids::MissionId::new("mission_x".to_string()),
            crate::ids::FindingId::new("find_x".to_string()),
            "用原账号再打一次".to_string(),
        );
        let json = serde_json::to_string(&retest).expect("序列化不会失败");
        assert!(json.contains("\"verdict\":\"\""));
        assert!(json.contains("\"status\":\"pending\""));
        let back: FindingRetest = serde_json::from_str(&json).expect("自身输出必须可解析");
        assert_eq!(back, retest);
        assert_eq!(back.parsed_verdict(), None);
    }
}
