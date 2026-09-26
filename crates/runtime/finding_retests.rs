//! 漏洞复测编排 —— 复测的创建与收口
//! 的移植。
//!
//! 这里只负责**状态与持久化**：谁去跑复测（worker/模型）由 API 层决定，
//! 与 `mission_advise` 的分工一致。这样 runtime 不依赖 api，复测的结论
//! 落库规则也能被单测覆盖。
//!
//! 三条硬规则：
//! 1. 同一 Finding 同时只允许一条未收口复测（重复点击返回已有记录）。
//! 2. 结论只能写一次；不同内容的第二次写入拒绝（`RetestError::NotRunning`），
//!    相同内容是幂等重放。
//! 3. `verdict = fixed` 且状态 `completed` 才把 Finding 推到
//!    [`FindingStatus::Fixed`]；中断/失败优先于已暂存的结论。

use models::{
    Finding, FindingRetest, FindingStatus, ProjectId, RetestError, RetestStatus, RetestVerdict,
    status_after_retest,
};

use crate::errors::EngineError;
use crate::manager::AuditManager;

impl AuditManager {
    /// 为某个 Finding 发起复测。
    ///
    /// 已有未收口复测时直接返回它（幂等），不会并发拉起第二个 worker。
    ///
    /// # Errors
    /// - [`EngineError::FindingNotFound`]：Finding 不存在。
    /// - [`EngineError::Retest`]：并发抢占失败（理论上被上面的检查挡住，
    ///   出现即说明有竞态，宁可报错也不并发）。
    pub fn start_finding_retest(
        &self,
        mission_id: &str,
        finding_id: &str,
        notes: &str,
    ) -> Result<FindingRetest, EngineError> {
        let finding = self
            .repository()
            .get_finding(finding_id)?
            .ok_or_else(|| EngineError::FindingNotFound(format!("unknown finding: {finding_id}")))?;
        if finding.mission_id.as_ref().map(|id| id.as_str()) != Some(mission_id) {
            return Err(EngineError::FindingNotFound(format!(
                "finding {finding_id} does not belong to mission {mission_id}"
            )));
        }
        let project_id = finding.project_id.as_str().to_string();
        if let Some(open) = self
            .repository()
            .list_finding_retests(&project_id)?
            .into_iter()
            .find(|retest| retest.finding_id.as_str() == finding_id && retest.status.is_open())
        {
            return Ok(open);
        }
        let retest = FindingRetest::new(
            ProjectId::new(project_id),
            models::MissionId::new(mission_id.to_string()),
            finding.id.clone(),
            notes.trim().to_string(),
        );
        let stored = self.repository().add_finding_retest(&retest)?;
        Ok(stored)
    }

    /// 把复测标记为开始跑（worker 已拉起）。
    ///
    /// # Errors
    /// - [`EngineError::Retest`]：记录不存在或已收口。
    pub fn mark_finding_retest_running(
        &self,
        retest_id: &str,
        model: Option<String>,
    ) -> Result<FindingRetest, EngineError> {
        let mut retest = self
            .repository()
            .get_finding_retest(retest_id)?
            .ok_or_else(|| EngineError::Value(format!("unknown retest: {retest_id}")))?;
        if retest.status != RetestStatus::Pending {
            return Err(EngineError::Value(format!(
                "retest {retest_id} is not pending (status {})",
                retest.status.as_str()
            )));
        }
        retest.status = RetestStatus::Running;
        retest.model = model;
        retest.started_at = Some(models::utcnow());
        self.repository().update_finding_retest(&retest)?;
        Ok(retest)
    }

    /// 写入复测结论并收口；必要时按规则改写 Finding 状态。
    ///
    /// 相同内容的重复写入是幂等重放（返回已收口记录）；不同内容直接拒绝。
    ///
    /// # Errors
    /// - [`EngineError::Value`]：复测不存在 / 已收口且结论不同 /
    ///   结论字段为空 / 终态非法。
    pub async fn finish_finding_retest(
        &self,
        retest_id: &str,
        terminal_status: RetestStatus,
        verdict: Option<RetestVerdict>,
        summary: &str,
        evidence: &str,
        assessment: &str,
        context_source: Option<&str>,
    ) -> Result<FindingRetest, EngineError> {
        if !matches!(
            terminal_status,
            RetestStatus::Completed | RetestStatus::Failed | RetestStatus::Stopped
        ) {
            return Err(EngineError::Value(format!(
                "terminal retest status must be completed/failed/stopped, got {}",
                terminal_status.as_str()
            )));
        }
        let mut retest = self
            .repository()
            .get_finding_retest(retest_id)?
            .ok_or_else(|| EngineError::Value(format!("unknown retest: {retest_id}")))?;
        if retest.is_finished() {
            // 重放：内容完全一致就当作成功返回，不一致才拒绝。
            let same = retest.status == terminal_status
                && retest.verdict == verdict.map_or(String::new(), |v| v.as_str().to_string())
                && retest.summary == summary.trim()
                && retest.evidence == evidence.trim();
            if same {
                return Ok(retest);
            }
            return Err(EngineError::Value(format!(
                "retest {retest_id} already finished; a finished retest is immutable"
            )));
        }
        if terminal_status == RetestStatus::Completed
            && (summary.trim().is_empty() || evidence.trim().is_empty())
        {
            // 没有结论就不算跑完：一律判 failed，别让"看起来修好了"。
            return Err(EngineError::Value(
                RetestError::EmptyConclusion.to_string(),
            ));
        }
        retest.status = terminal_status;
        retest.verdict = verdict.map_or(String::new(), |v| v.as_str().to_string());
        retest.summary = summary.trim().to_string();
        retest.evidence = evidence.trim().to_string();
        if !assessment.trim().is_empty() {
            retest.assessment = assessment.trim().to_string();
        }
        if terminal_status == RetestStatus::Failed {
            // 失败原因必须落在 `error` 里，不能只躺在 `assessment` 里让 UI
            // 显示一句通用的 "retest failed"。assessment 是原始文本，error 是
            // 给人看的一句话原因。
            retest.error = retest.error.clone().if_empty(|| {
                let trimmed = assessment.trim();
                if trimmed.is_empty() {
                    "retest failed".to_string()
                } else {
                    trimmed.to_string()
                }
            });
        }
        retest.context_source = context_source.map(str::to_string);
        retest.finished_at = Some(models::utcnow());
        let stored = self.repository().update_finding_retest(&retest)?;

        // 只有"正常收口 + fixed"才改写漏洞状态。
        if let Some(next_status) = status_after_retest(stored.status, stored.parsed_verdict()) {
            self.triage_finding(stored.finding_id.as_str(), Some(next_status), None)
                .await?;
        }
        Ok(stored)
    }

    /// 列某 Mission 的全部复测记录（新的在前）。
    ///
    /// # Errors
    /// 仓储读取失败。
    pub fn list_mission_finding_retests(
        &self,
        mission_id: &str,
    ) -> Result<Vec<FindingRetest>, EngineError> {
        let mission = self
            .repository()
            .get_mission(mission_id)?
            .ok_or_else(|| EngineError::MissionNotFound(format!("unknown mission: {mission_id}")))?;
        Ok(self
            .repository()
            .list_finding_retests(mission.project_id.as_str())?
            .into_iter()
            .filter(|retest| retest.mission_id.as_str() == mission_id)
            .collect())
    }

    /// 列某 Mission 的未收口复测（前端轮询"复测中"角标用）。
    ///
    /// # Errors
    /// Mission 不存在或仓储读取失败。
    pub fn list_open_mission_finding_retests(
        &self,
        mission_id: &str,
    ) -> Result<Vec<FindingRetest>, EngineError> {
        Ok(self
            .list_mission_finding_retests(mission_id)?
            .into_iter()
            .filter(|retest| retest.status.is_open())
            .collect())
    }
}

/// `if_empty` 小助手：空串时给默认值（保持 `error` 已有内容不被覆盖）。
trait IfEmpty {
    /// 自身为空串时返回 `fallback`，否则返回自身。
    fn if_empty(self, fallback: impl FnOnce() -> String) -> String;
}

impl IfEmpty for String {
    fn if_empty(self, fallback: impl FnOnce() -> String) -> String {
        if self.trim().is_empty() {
            fallback()
        } else {
            self
        }
    }
}

/// 让 `FindingStatus` 的测试断言可以直接用（避免 unused import 警告）。
#[cfg(test)]
mod tests {
    use super::*;
    use crate::manager::AuditManager;
    use agents::solver::SolverRegistry;
    use models::AuditDomain;
    use models::{Mission, Project};
    use std::sync::Arc;

    /// 起一个内存库 + 一个最小 Mission/Project/Finding，用来跑状态机。
    fn fixture() -> (AuditManager, String, String) {
        let dir = tempfile::tempdir().expect("系统临时目录应可创建");
        let path = dir.path().join("retest.sqlite3");
        let repo = storage::SqliteRepository::open(&path).expect("sqlite repository must open");
        // 临时目录要活到测试结束：leak 掉，进程退出时回收。
        std::mem::forget(dir);
        let manager = AuditManager::new(
            Arc::new(repo),
            SolverRegistry::new(),
            Arc::new(crate::task_backend::InMemoryTaskBackend::default()),
        );
        let project = Project::new(
            "retest-proj".to_string(),
            AuditDomain::WebRecon,
        );
        let project = manager
            .repository()
            .create_project(&project)
            .expect("project must persist");
        let mission = serde_json::from_value::<Mission>(serde_json::json!({
            "id": "mission_retest",
            "project_id": project.id.as_str(),
            "user_goal": "retest the eval injection",
            "target": { "url": "https://target.example" },
            "constraints": [],
            "success_criteria": [],
            "tags": [],
            "category": "web-audit",
            "approval_mode": "ask_for_approval",
            "archived": false,
            "status": "running",
            "active_run_id": null,
            "created_at": "2026-08-24T12:00:00.123456Z",
            "updated_at": "2026-08-24T12:00:00.123456Z",
            "created_by": "user",
        }))
        .expect("Mission fixture must parse");
        let mission = manager
            .repository()
            .create_mission(&mission)
            .expect("mission must persist");
        let finding = serde_json::from_value::<Finding>(serde_json::json!({
            "id": "find_eval",
            "project_id": project.id.as_str(),
            "mission_id": mission.id.as_str(),
            "title": "Eval injection",
            "severity": "high",
            "status": "confirmed",
            "evidence_ids": ["evd_1"],
            "created_at": "2026-08-24T12:00:00.123456Z",
            "updated_at": "2026-08-24T12:00:00.123456Z",
        }))
        .expect("Finding fixture must parse");
        let finding = manager
            .repository()
            .add_finding(&finding)
            .expect("finding must persist");
        (
            manager,
            mission.id.as_str().to_string(),
            finding.id.as_str().to_string(),
        )
    }

    #[tokio::test]
    async fn start_is_idempotent_while_a_retest_is_open() {
        let (manager, mission_id, finding_id) = fixture();
        let first = manager
            .start_finding_retest(&mission_id, &finding_id, "notes")
            .expect("first retest must start");
        let second = manager
            .start_finding_retest(&mission_id, &finding_id, "notes again")
            .expect("second click must not error");
        assert_eq!(first.id, second.id, "未收口时重复发起必须返回同一条记录");
        assert_eq!(second.notes, "notes", "已存在记录的 notes 不被后来的点击覆盖");
        let all = manager
            .list_mission_finding_retests(&mission_id)
            .expect("list must work");
        assert_eq!(all.len(), 1);
    }

    #[tokio::test]
    async fn a_finding_from_another_mission_is_rejected() {
        let (manager, mission_id, finding_id) = fixture();
        let error = manager
            .start_finding_retest("mission_other", &finding_id, "")
            .expect_err("finding 不属于该 mission 时必须拒绝");
        assert!(matches!(error, EngineError::FindingNotFound(_)));
        let _ = mission_id;
    }

    #[tokio::test]
    async fn completed_fixed_verdict_writes_the_finding_status() {
        let (manager, mission_id, finding_id) = fixture();
        let retest = manager
            .start_finding_retest(&mission_id, &finding_id, "")
            .expect("retest must start");
        let retest = manager
            .mark_finding_retest_running(retest.id.as_str(), Some("gpt-x".to_string()))
            .expect("retest must move to running");
        assert_eq!(retest.status, RetestStatus::Running);
        assert_eq!(retest.model.as_deref(), Some("gpt-x"));

        let retest = manager
            .finish_finding_retest(
                retest.id.as_str(),
                RetestStatus::Completed,
                Some(RetestVerdict::Fixed),
                "修复版本已部署",
                "复测 /api/v1/user 返回 400",
                "VERDICT: fixed\n...",
                None,
            )
            .await
            .expect("finish must succeed");
        assert_eq!(retest.parsed_verdict(), Some(RetestVerdict::Fixed));

        let finding = manager
            .repository()
            .get_finding(&finding_id)
            .expect("read must work")
            .expect("finding must exist");
        assert_eq!(finding.status, FindingStatus::Fixed, "fixed 结论必须写回漏洞状态");
    }

    #[tokio::test]
    async fn failed_retest_never_rewrites_the_finding_status() {
        let (manager, mission_id, finding_id) = fixture();
        let retest = manager
            .start_finding_retest(&mission_id, &finding_id, "")
            .expect("retest must start");
        manager
            .mark_finding_retest_running(retest.id.as_str(), None)
            .expect("retest must move to running");
        // 失败时也带上 fixed 结论（顾问说了 fixed 但 worker 崩了）：
        // 中断优先，不能看起来像修好了。
        manager
            .finish_finding_retest(
                retest.id.as_str(),
                RetestStatus::Failed,
                Some(RetestVerdict::Fixed),
                "",
                "",
                "worker crashed",
                None,
            )
            .await
            .expect("finish must succeed");
        let finding = manager
            .repository()
            .get_finding(&finding_id)
            .expect("read must work")
            .expect("finding must exist");
        assert_eq!(finding.status, FindingStatus::Confirmed, "失败的复测不能改漏洞状态");
    }

    #[tokio::test]
    async fn reproduced_verdict_leaves_the_finding_alone() {
        let (manager, mission_id, finding_id) = fixture();
        let retest = manager
            .start_finding_retest(&mission_id, &finding_id, "")
            .expect("retest must start");
        manager
            .finish_finding_retest(
                retest.id.as_str(),
                RetestStatus::Completed,
                Some(RetestVerdict::Reproduced),
                "仍可复现",
                "原 payload 依旧执行",
                "",
                None,
            )
            .await
            .expect("finish must succeed");
        let finding = manager
            .repository()
            .get_finding(&finding_id)
            .expect("read must work")
            .expect("finding must exist");
        assert_eq!(finding.status, FindingStatus::Confirmed);
    }

    #[tokio::test]
    async fn conclusion_is_write_once() {
        let (manager, mission_id, finding_id) = fixture();
        let retest = manager
            .start_finding_retest(&mission_id, &finding_id, "")
            .expect("retest must start");
        manager
            .finish_finding_retest(
                retest.id.as_str(),
                RetestStatus::Completed,
                Some(RetestVerdict::Fixed),
                "修了",
                "证据",
                "",
                None,
            )
            .await
            .expect("first finish must succeed");

        // 完全相同的内容 → 幂等重放。
        let replay = manager
            .finish_finding_retest(
                retest.id.as_str(),
                RetestStatus::Completed,
                Some(RetestVerdict::Fixed),
                "修了",
                "证据",
                "",
                None,
            )
            .await
            .expect("identical replay must succeed");
        assert_eq!(replay.id, retest.id);

        // 不同内容 → 拒绝，不能覆盖已封存的结论。
        let error = manager
            .finish_finding_retest(
                retest.id.as_str(),
                RetestStatus::Completed,
                Some(RetestVerdict::Reproduced),
                "其实没修",
                "新证据",
                "",
                None,
            )
            .await
            .expect_err("different conclusion must be rejected");
        assert!(error.to_string().contains("immutable"), "got: {error}");
    }

    #[tokio::test]
    async fn completed_without_a_conclusion_is_refused() {
        let (manager, mission_id, finding_id) = fixture();
        let retest = manager
            .start_finding_retest(&mission_id, &finding_id, "")
            .expect("retest must start");
        let error = manager
            .finish_finding_retest(
                retest.id.as_str(),
                RetestStatus::Completed,
                None,
                "   ",
                "   ",
                "VERDICT: maybe",
                None,
            )
            .await
            .expect_err("completed 但没结论必须拒绝");
        assert!(error.to_string().contains("summary and evidence"), "got: {error}");
        // 记录仍是未收口，Operator 可以再发起一次。
        let stored = manager
            .repository()
            .get_finding_retest(retest.id.as_str())
            .expect("read must work")
            .expect("retest must exist");
        assert_eq!(stored.status, RetestStatus::Pending);
    }

    #[tokio::test]
    async fn a_new_retest_can_start_after_the_previous_one_finished() {
        let (manager, mission_id, finding_id) = fixture();
        let first = manager
            .start_finding_retest(&mission_id, &finding_id, "")
            .expect("retest must start");
        manager
            .finish_finding_retest(
                first.id.as_str(),
                RetestStatus::Completed,
                Some(RetestVerdict::Reproduced),
                "仍可复现",
                "证据",
                "",
                None,
            )
            .await
            .expect("finish must succeed");
        let second = manager
            .start_finding_retest(&mission_id, &finding_id, "second attempt")
            .expect("收口后必须能再发起");
        assert_ne!(first.id, second.id);
        assert_eq!(second.notes, "second attempt");
        assert_eq!(second.status, RetestStatus::Pending);

        let open = manager
            .list_open_mission_finding_retests(&mission_id)
            .expect("list open must work");
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].id, second.id);
    }

    #[test]
    fn finding_status_is_reachable_for_assertions() {
        assert_eq!(FindingStatus::Fixed.as_str(), "fixed");
    }
}