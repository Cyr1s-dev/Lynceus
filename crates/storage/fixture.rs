//! 跨语言对拍 fixture —— `scripts/parity_fixture.py` 的逐操作 Rust 镜像。
//!
//! 同一组固定实体（固定 ID、固定时间戳）经同一序列的 [`Repository`] 方法
//! 写入 SQLite；Python 侧执行同一序列。两侧产出的数据库文件经各自的
//! 规范化 dump（`scripts/dump_db.py` 与 `diff dump`）必须逐字节
//! 一致——这就是仓储层行为对拍（黄金文件：
//! `tests/fixtures/parity_golden.json`）。
//!
//! 确定性约束（与 Python 侧逐条对应）：
//!
//! - 全部实体显式给定全部字段，不依赖构造默认值；
//! - 只调用**原样持久化**的方法（`create_*` / `add_*` / `update_*` /
//!   `get_*` / `list_*`）；`increment_run_steps` / `append_run_task` 内部
//!   打 `utcnow()` 时间戳会破坏确定性，不进入本序列（语义由两侧单元
//!   测试覆盖）；
//! - 实体取值逐字段镜像 `scripts/probe_parity_wire.py`（wire 字节锁定
//!   测试的同款输入），payload 逐字节一致性已由该组测试先行锁定。

use serde_json::Value;

use models::EvidenceKind;
use models::FindingStatus;
use models::MissionStatus;
use models::RunStatus;
use models::Severity;
use models::TaskStatus;
use models::Timestamp;
use models::ToolStatus;
use models::{
    AgentTask, AuditRun, Branch, BranchId, CodeLocation, Evidence, EvidenceId, Finding, FindingId,
    Mission, MissionId, ProjectId, RunId, StrMap, TaskId, ToolInvocation, ToolInvocationId,
};

use crate::error::StorageError;
use crate::repository::Repository;

/// fixture 的固定 project scope。
pub const PROJECT_ID: &str = "proj_parity";

/// 解析 fixture 固定时间戳字面量。
///
/// 字面量是编译期已知的常量字符串，解析失败只可能是本模块自身损坏，
/// 属于程序员错误而非运行时数据错误——按不变式违反处理（panic 条件：
/// 从不，除非 fixture 源码被改坏）。
fn fixed_timestamp(literal: &str) -> Timestamp {
    literal.parse().unwrap_or_else(|error| {
        panic!("fixture 时间戳字面量损坏: {literal}: {error}");
    })
}

/// T1：微秒非零（覆盖六位小数 wire 格式与 `isoformat` 列格式）。
fn t1() -> Timestamp {
    fixed_timestamp("2026-08-24T12:00:00.123456Z")
}

/// T2：update 之后的 `updated_at`（与 T1 不同，锁住 upsert 真实生效）。
fn t2() -> Timestamp {
    fixed_timestamp("2026-08-24T12:30:00.654321Z")
}

/// [`build`] 返回的语义检查快照：读取方法的返回值，供调用方断言。
///
/// 数据库最终状态由 dump 对拍守护；这里只携带行为语义证据。
#[derive(Debug)]
pub struct FixtureSnapshot {
    /// create 后立即 get 的往返结果（update 前状态）。
    pub fetched_mission_after_create: Mission,
    /// update 后 get 的结果（upsert 不得产生第二行，状态为 paused）。
    pub fetched_mission_after_update: Mission,
    /// `list_evidence` 的结果。
    pub listed_evidence: Vec<Evidence>,
    /// `get_finding` 的结果。
    pub fetched_finding: Option<Finding>,
    /// `list_findings` 的结果。
    pub listed_findings: Vec<Finding>,
    /// `list_missions(Some(..))` 行数（upsert 后仍为 1）。
    pub mission_count_in_project: usize,
    /// `list_missions(None)` 行数。
    pub mission_count_total: usize,
}

/// fixture 的 Mission（运行中状态，update 前基准）。
#[must_use]
pub fn mission() -> Mission {
    let mut mission = Mission::new(
        ProjectId::new(PROJECT_ID.to_string()),
        "Find injected sinks".to_string(),
    );
    mission.id = MissionId::new("mission_fix_0001".to_string());
    mission.title = None;
    mission.target = [("url", "https://target.example"), ("note", "中文注释")]
        .into_iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect::<StrMap>();
    mission.constraints = vec!["stay in scope".to_string()];
    mission.success_criteria = vec!["flag captured".to_string()];
    mission.tags = vec!["web".to_string(), "audit".to_string()];
    mission.category = Some("web-audit".to_string());
    mission.archived = false;
    mission.status = MissionStatus::Running;
    mission.active_run_id = None;
    mission.created_at = t1();
    mission.updated_at = t1();
    mission.finished_at = None;
    mission.created_by = "user".to_string();
    mission.metadata = [
        ("zz_meta".to_string(), Value::from("last")),
        ("aa_meta".to_string(), Value::from("first")),
        ("count".to_string(), Value::from(2)),
    ]
    .into_iter()
    .collect();
    mission
}

/// fixture 的 Mission 终态：`update_mission` 写入的 paused 版本。
#[must_use]
pub fn paused_mission() -> Mission {
    let mut paused = mission();
    paused.status = MissionStatus::Paused;
    paused.updated_at = t2();
    paused
}

/// fixture 的 Branch。
#[must_use]
pub fn branch() -> Branch {
    let mut branch = Branch::new(
        ProjectId::new(PROJECT_ID.to_string()),
        MissionId::new("mission_fix_0001".to_string()),
        "Sink reachability".to_string(),
        "User input reaches eval".to_string(),
    );
    branch.id = BranchId::new("branch_fix_0001".to_string());
    branch.run_id = None;
    branch.parent_branch_id = None;
    branch.rationale = String::new();
    branch.status = models::BranchStatus::Active;
    branch.priority = 70;
    branch.confidence = 0.75;
    branch.budget_steps = 6;
    branch.steps_used = 2;
    branch.related_fact_ids = vec!["fact_1".to_string()];
    branch.related_evidence_ids = Vec::new();
    branch.related_finding_ids = Vec::new();
    branch.related_tool_invocation_ids = vec!["tool_1".to_string()];
    branch.assigned_worker_id = None;
    branch.created_by = "branch_generator".to_string();
    branch.created_at = t1();
    branch.updated_at = t1();
    branch.metadata = [
        ("zz".to_string(), Value::from(1)),
        ("aa".to_string(), Value::from(2)),
    ]
    .into_iter()
    .collect();
    branch
}

/// fixture 的 `AuditRun`。
#[must_use]
pub fn audit_run() -> AuditRun {
    let mut run = AuditRun::new(ProjectId::new(PROJECT_ID.to_string()));
    run.id = RunId::new("run_fix_0001".to_string());
    run.mission_id = Some(MissionId::new("mission_fix_0001".to_string()));
    run.status = RunStatus::Running;
    run.config = [("depth".to_string(), Value::from(2))]
        .into_iter()
        .collect();
    run.max_total_steps = 64;
    run.steps_used = 3;
    run.task_ids = vec!["task_fix_0001".to_string()];
    run.note = None;
    run.created_at = t1();
    run.started_at = Some(t1());
    run.finished_at = None;
    run.updated_at = t1();
    run
}

/// fixture 的 `AgentTask`。
#[must_use]
pub fn agent_task() -> AgentTask {
    let mut task = AgentTask::new(
        ProjectId::new(PROJECT_ID.to_string()),
        RunId::new("run_fix_0001".to_string()),
        "web_sast".to_string(),
    );
    task.id = TaskId::new("task_fix_0001".to_string());
    task.mission_id = Some(MissionId::new("mission_fix_0001".to_string()));
    task.branch_id = Some(BranchId::new("branch_fix_0001".to_string()));
    task.intent_id = None;
    task.status = TaskStatus::Succeeded;
    task.payload = [("step".to_string(), Value::from(1))].into_iter().collect();
    task.budget_steps = 8;
    task.produced_fact_ids = vec!["fact_1".to_string()];
    task.produced_evidence_ids = vec!["evd_fix_0001".to_string()];
    task.produced_finding_ids = vec!["find_fix_0001".to_string()];
    task.tool_invocation_ids = vec!["tool_fix_0001".to_string()];
    task.error = None;
    task.created_at = t1();
    task.started_at = Some(t1());
    task.finished_at = Some(t1());
    task
}

/// fixture 的 `ToolInvocation`。
#[must_use]
pub fn tool_invocation() -> ToolInvocation {
    let mut invocation = ToolInvocation::new("semgrep".to_string(), "scan src/".to_string());
    invocation.id = ToolInvocationId::new("tool_fix_0001".to_string());
    invocation.project_id = Some(ProjectId::new(PROJECT_ID.to_string()));
    invocation.mission_id = Some(MissionId::new("mission_fix_0001".to_string()));
    invocation.branch_id = Some(BranchId::new("branch_fix_0001".to_string()));
    invocation.run_id = Some(RunId::new("run_fix_0001".to_string()));
    invocation.task_id = Some(TaskId::new("task_fix_0001".to_string()));
    invocation.module_id = None;
    invocation.output_summary = "3 findings".to_string();
    invocation.status = ToolStatus::Ok;
    invocation.exit_code = Some(0);
    invocation.duration_ms = Some(1234);
    invocation.artifact_paths = vec!["artifacts/semgrep.json".to_string()];
    invocation.error = None;
    invocation.metadata = [
        ("zz".to_string(), Value::from(1)),
        ("aa".to_string(), Value::from(2)),
    ]
    .into_iter()
    .collect();
    invocation.started_at = t1();
    invocation.finished_at = Some(t1());
    invocation
}

/// fixture 的 `Evidence`（含 `CodeLocation`）。
#[must_use]
pub fn evidence() -> Evidence {
    let mut evidence = Evidence::new(
        ProjectId::new(PROJECT_ID.to_string()),
        EvidenceKind::TaintPath,
        "tainted flow to eval".to_string(),
    );
    evidence.id = EvidenceId::new("evd_fix_0001".to_string());
    evidence.mission_id = Some(MissionId::new("mission_fix_0001".to_string()));
    evidence.branch_id = Some(BranchId::new("branch_fix_0001".to_string()));
    evidence.content = [
        ("zz".to_string(), Value::from("last")),
        ("aa".to_string(), Value::from("first")),
    ]
    .into_iter()
    .collect();
    let mut location = CodeLocation::new("src/app.py".to_string());
    location.start_line = Some(10);
    location.end_line = Some(42);
    location.address = None;
    location.symbol = Some("handler".to_string());
    location.snippet = Some("eval(req.data)".to_string());
    evidence.locations = vec![location];
    evidence.supports_fact_ids = vec!["fact_1".to_string()];
    evidence.produced_by_task_id = Some(TaskId::new("task_fix_0001".to_string()));
    evidence.produced_by_tool_invocation_id =
        Some(ToolInvocationId::new("tool_fix_0001".to_string()));
    evidence.run_id = Some(RunId::new("run_fix_0001".to_string()));
    evidence.fingerprint = Some("sha256:abc123".to_string());
    evidence.evidence_path = Some("artifacts/evd.json".to_string());
    evidence.created_at = t1();
    evidence
}

/// fixture 的 Finding。
#[must_use]
pub fn finding() -> Finding {
    let mut finding = Finding::new(
        ProjectId::new(PROJECT_ID.to_string()),
        "Eval injection".to_string(),
    );
    finding.id = FindingId::new("find_fix_0001".to_string());
    finding.mission_id = Some(MissionId::new("mission_fix_0001".to_string()));
    finding.branch_id = Some(BranchId::new("branch_fix_0001".to_string()));
    finding.run_id = Some(RunId::new("run_fix_0001".to_string()));
    finding.description = Some("user input reaches eval".to_string());
    finding.severity = Severity::High;
    finding.status = FindingStatus::Confirmed;
    finding.cwe = Some("CWE-95".to_string());
    finding.rule_id = Some("web_sast.eval_injection".to_string());
    finding.evidence_ids = vec!["evd_fix_0001".to_string()];
    finding.related_fact_ids = vec!["fact_1".to_string()];
    finding.source_label = Some("request.data".to_string());
    finding.sink_label = Some("eval".to_string());
    finding.fingerprint = Some("sha256:def456".to_string());
    finding.dedup_of = None;
    finding.review = [
        ("zz".to_string(), Value::from(1)),
        ("aa".to_string(), Value::from(2)),
    ]
    .into_iter()
    .collect();
    finding.produced_by_task_id = Some(TaskId::new("task_fix_0001".to_string()));
    finding.created_at = t1();
    finding.updated_at = t1();
    finding
}

/// 执行固定操作序列（13 个仓储方法），返回语义检查快照。
///
/// # Errors
///
/// 任一仓储方法失败时返回 [`StorageError`]。
///
/// # Panics
///
/// `get_mission` 在 create/update 之后返回 `None` 属于仓储实现违反契约，
/// 立即 panic（fail-fast，不产出半途 fixture）。
pub fn build(repo: &dyn Repository) -> Result<FixtureSnapshot, StorageError> {
    let original = mission();
    repo.create_mission(&original)?;
    let fetched_after_create = repo
        .get_mission(original.id.as_str())?
        .unwrap_or_else(|| panic!("刚创建的 mission 必须可读取"));

    repo.create_branch(&branch())?;
    repo.create_run(&audit_run())?;
    repo.create_task(&agent_task())?;
    repo.add_tool_invocation(&tool_invocation())?;

    let expected_evidence = evidence();
    repo.add_evidence(&expected_evidence)?;
    let listed_evidence = repo.list_evidence(PROJECT_ID)?;

    let expected_finding = finding();
    repo.add_finding(&expected_finding)?;
    let fetched_finding = repo.get_finding(expected_finding.id.as_str())?;
    let listed_findings = repo.list_findings(PROJECT_ID)?;

    let updated = paused_mission();
    repo.update_mission(&updated)?;
    let fetched_after_update = repo
        .get_mission(updated.id.as_str())?
        .unwrap_or_else(|| panic!("update 后的 mission 必须可读取"));
    let mission_count_in_project = repo.list_missions(Some(PROJECT_ID))?.len();
    let mission_count_total = repo.list_missions(None)?.len();

    Ok(FixtureSnapshot {
        fetched_mission_after_create: fetched_after_create,
        fetched_mission_after_update: fetched_after_update,
        listed_evidence,
        fetched_finding,
        listed_findings,
        mission_count_in_project,
        mission_count_total,
    })
}

/// 断言快照符合 fixture 语义（供集成测试与 CLI 自检共用）。
///
/// 返回 `Err`（携带差异描述）而非 panic，使 CLI 可以打印失败原因后以
/// 非零码退出。
///
/// # Errors
///
/// 任一语义断言失败时返回携带差异描述的 `Err(String)`。
pub fn verify(snapshot: &FixtureSnapshot) -> Result<(), String> {
    let original = mission();
    if snapshot.fetched_mission_after_create != original {
        return Err("get_mission 往返不等（create 后）".to_string());
    }
    let updated = paused_mission();
    if snapshot.fetched_mission_after_update != updated {
        return Err("get_mission 往返不等（update 后，状态应为 paused）".to_string());
    }
    let expected_evidence = evidence();
    if snapshot.listed_evidence != vec![expected_evidence] {
        return Err("list_evidence 语义漂移".to_string());
    }
    let expected_finding = finding();
    if snapshot.fetched_finding.as_ref() != Some(&expected_finding) {
        return Err("get_finding 往返不等".to_string());
    }
    if snapshot.listed_findings != vec![expected_finding] {
        return Err("list_findings 语义漂移".to_string());
    }
    if snapshot.mission_count_in_project != 1 {
        return Err("upsert 不得产生第二行（project 过滤）".to_string());
    }
    if snapshot.mission_count_total != 1 {
        return Err("list_missions(None) 必须列出全部且仅一行".to_string());
    }
    Ok(())
}
