//! 外部 `Worker Runtime` 存储往返：`WorkerRun` / `WorkerInvocation` /
//! `WorkerRuntimeProfile` 的 upsert-get-list-delete 语义。

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use models::{
    AgentTask, AuditRun, BlackboardEntry, BlackboardEntryKind, ProjectId, RunId, TaskStatus,
    WorkerExecutionEnvironment, WorkerInvocation, WorkerInvocationPurpose, WorkerRun,
    WorkerRunStatus, WorkerRuntimeProfile, WorkerRuntimeType, utcnow,
};
use storage::Repository;
use storage::SqliteRepository;

fn open_repo() -> (SqliteRepository, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("temp dir");
    let repo = SqliteRepository::open(dir.path().join("worker.sqlite3")).expect("open");
    (repo, dir)
}

#[test]
fn worker_run_roundtrip_and_filters() {
    let (repo, _dir) = open_repo();
    let project = ProjectId::new("proj_worker_store".to_string());
    let mut run = WorkerRun::new(
        project.clone(),
        WorkerRuntimeType::ClaudeCode,
        "audit target",
    );
    run.mission_id = None;
    run.mark_started();
    run.record_event(models::WorkerEventKind::Output, "step 1 done");
    repo.upsert_worker_run(&run).expect("insert worker run");

    // 幂等 upsert：同 id 整体替换。
    run.finish(WorkerRunStatus::Succeeded, Some("all done".to_string()));
    repo.upsert_worker_run(&run).expect("upsert worker run");

    let stored = repo
        .get_worker_run(&run.id)
        .expect("query")
        .expect("run exists");
    assert_eq!(stored.status, WorkerRunStatus::Succeeded);
    assert_eq!(stored.summary.as_deref(), Some("all done"));
    assert!(!stored.events.is_empty());

    // project 过滤命中。
    let listed = repo
        .list_worker_runs(Some(project.as_str()), None, None, 10)
        .expect("list by project");
    assert_eq!(listed.len(), 1);

    // 不匹配的过滤条件返回空。
    let none = repo
        .list_worker_runs(Some(project.as_str()), Some("run_missing"), None, 10)
        .expect("list by run id");
    assert!(none.is_empty());

    // 全表倒排。
    let all = repo
        .list_worker_runs(None, None, None, 10)
        .expect("list all");
    assert_eq!(all.len(), 1);
}

#[test]
fn worker_invocation_roundtrip() {
    let (repo, _dir) = open_repo();
    let project = ProjectId::new("proj_worker_inv".to_string());
    let mut invocation = WorkerInvocation::new(
        WorkerRuntimeType::Codex,
        WorkerInvocationPurpose::Start,
        None,
    );
    invocation.project_id = Some(project.clone());
    invocation.finish(WorkerRunStatus::Succeeded);
    repo.upsert_worker_invocation(&invocation)
        .expect("insert invocation");

    let listed = repo
        .list_worker_invocations(None, Some(project.as_str()), 10)
        .expect("list by project");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].status, WorkerRunStatus::Succeeded);

    let by_run = repo
        .list_worker_invocations(Some("wkrun_missing"), None, 10)
        .expect("list by run");
    assert!(by_run.is_empty());
}

#[test]
fn worker_runtime_profile_crud() {
    let (repo, _dir) = open_repo();
    let profile = WorkerRuntimeProfile::new(
        WorkerRuntimeType::ClaudeCode,
        "prov_anthropic",
        WorkerExecutionEnvironment::Local,
        2,
        900,
    )
    .expect("valid profile");
    repo.upsert_worker_runtime_profile(&profile)
        .expect("insert profile");

    let listed = repo
        .list_worker_runtime_profiles(Some(WorkerRuntimeType::ClaudeCode))
        .expect("list by runtime");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].connection_id, "prov_anthropic");

    let by_other = repo
        .list_worker_runtime_profiles(Some(WorkerRuntimeType::Codex))
        .expect("list other runtime");
    assert!(by_other.is_empty());

    // 更新。
    let mut updated = profile.clone();
    updated.enabled = false;
    updated.max_concurrency = 4;
    repo.upsert_worker_runtime_profile(&updated)
        .expect("upsert profile");
    let fetched = repo
        .get_worker_runtime_profile(&profile.id)
        .expect("get")
        .expect("exists");
    assert!(!fetched.enabled);
    assert_eq!(fetched.max_concurrency, 4);

    // 删除。
    let deleted = repo
        .delete_worker_runtime_profile(&profile.id)
        .expect("delete");
    assert!(deleted);
    let missing = repo
        .get_worker_runtime_profile(&profile.id)
        .expect("get after delete");
    assert!(missing.is_none());
}

fn scoped_task(repo: &SqliteRepository, suffix: &str) -> (ProjectId, String, String) {
    let project = ProjectId::new(format!("proj_scope_{suffix}"));
    let mission = models::MissionId::new(format!("mission_scope_{suffix}"));
    let mut run = AuditRun::new(project.clone());
    run.id = RunId::new(format!("run_scope_{suffix}"));
    run.mission_id = Some(mission.clone());
    repo.create_run(&run).expect("run");
    let mut task = AgentTask::new(project.clone(), run.id.clone(), "fake_worker".to_string());
    task.id = models::TaskId::new(format!("task_scope_{suffix}"));
    task.mission_id = Some(mission);
    repo.create_task(&task).expect("task");
    (project, run.id.to_string(), task.id.to_string())
}

#[test]
fn blackboard_is_append_only_mission_scoped_and_idempotent() {
    let (repo, _dir) = open_repo();
    let (project, run_id, task_id) = scoped_task(&repo, "one");
    let mission = models::MissionId::new("mission_scope_one".to_string());
    let mut entry = BlackboardEntry::new(
        project.clone(),
        mission.clone(),
        RunId::new(run_id.clone()),
        "worker-run-one".to_string(),
        BlackboardEntryKind::Observation,
        Some("first observation".to_string()),
        None,
        None,
        None,
        "observation-1".to_string(),
    );
    entry.task_id = Some(models::TaskId::new(task_id));
    let stored = repo.append_blackboard_entry(&entry).expect("append");
    assert!(stored.sequence > 0);
    assert_eq!(
        repo.append_blackboard_entry(&entry)
            .expect("idempotent retry"),
        stored
    );

    let mut mismatch = entry.clone();
    mismatch.content = Some("tampered retry".to_string());
    assert!(matches!(
        repo.append_blackboard_entry(&mismatch),
        Err(storage::StorageError::AppendOnlyConflict { .. })
    ));

    let (entries, next) = repo
        .list_blackboard_entries(
            project.as_str(),
            mission.as_str(),
            &run_id,
            None,
            None,
            10,
            16 * 1024,
        )
        .expect("read");
    assert_eq!(entries, vec![stored.clone()]);
    assert!(next.is_none());
    let (empty, _) = repo
        .list_blackboard_entries(
            project.as_str(),
            "different-mission",
            &run_id,
            None,
            None,
            10,
            16 * 1024,
        )
        .expect("cross mission read is empty");
    assert!(empty.is_empty());
}

#[test]
fn run_reservation_and_worker_lease_are_atomic_cas_operations() {
    let (repo, _dir) = open_repo();
    let (project, run_id, task_id) = scoped_task(&repo, "lease");
    let run = repo.get_run(&run_id).expect("run").expect("run exists");
    let mut limited = run.clone();
    limited.max_total_steps = 1;
    repo.update_run(&limited).expect("set budget");
    assert!(repo.reserve_run_steps(&run_id, 1).expect("reserve"));
    assert!(!repo.reserve_run_steps(&run_id, 1).expect("budget is full"));

    let repo = std::sync::Arc::new(repo);
    let claims = (0..2).map(|index| {
        let repo = std::sync::Arc::clone(&repo);
        let project = project.clone();
        let run_id = run_id.clone();
        let task_id = task_id.clone();
        std::thread::spawn(move || {
            repo.claim_worker_lease(
                project.as_str(),
                "mission_scope_lease",
                &run_id,
                &task_id,
                &format!("worker-{index}"),
                &format!("worker-run-{index}"),
                60,
            )
            .expect("claim")
        })
    });
    let claimed: Vec<_> = claims
        .map(|handle| handle.join().expect("claim thread"))
        .collect();
    assert_eq!(claimed.iter().filter(|lease| lease.is_some()).count(), 1);
    let lease = claimed
        .into_iter()
        .flatten()
        .next()
        .expect("one lease wins");
    assert_eq!(lease.revision, 1);
    assert!(
        repo.heartbeat_worker_lease(lease.id.as_str(), "wrong-worker-run", lease.revision, 60,)
            .expect("non-owner CAS")
            .is_none()
    );
    let heartbeated = repo
        .heartbeat_worker_lease(
            lease.id.as_str(),
            lease.worker_run_id.as_deref().expect("owner"),
            lease.revision,
            60,
        )
        .expect("heartbeat")
        .expect("heartbeat wins");
    assert_eq!(heartbeated.revision, 2);
    assert!(
        repo.complete_worker_lease(
            lease.id.as_str(),
            lease.worker_run_id.as_deref().expect("owner"),
            lease.revision,
        )
        .expect("stale completion")
        .is_none()
    );
    let completed = repo
        .complete_worker_lease(
            lease.id.as_str(),
            heartbeated.worker_run_id.as_deref().expect("owner"),
            heartbeated.revision,
        )
        .expect("completion")
        .expect("completion wins");
    assert_eq!(completed.status, models::WorkerLeaseStatus::Completed);
    assert_eq!(
        repo.get_task(&task_id).expect("task").expect("task").status,
        TaskStatus::Succeeded
    );
    assert!(
        repo.claim_worker_lease(
            project.as_str(),
            "mission_scope_lease",
            &run_id,
            &task_id,
            "worker-3",
            "worker-run-3",
            60,
        )
        .expect("completed task cannot claim")
        .is_none()
    );

    let (fail_project, fail_run, fail_task) = scoped_task(&repo, "fail");
    let failed = repo
        .claim_worker_lease(
            fail_project.as_str(),
            "mission_scope_fail",
            &fail_run,
            &fail_task,
            "worker-fail",
            "worker-run-fail",
            60,
        )
        .expect("failure claim")
        .expect("failure lease");
    let failed = repo
        .fail_worker_lease(&failed.id.to_string(), "worker-run-fail", failed.revision)
        .expect("failure transition")
        .expect("failure transition wins");
    assert_eq!(failed.status, models::WorkerLeaseStatus::Failed);
    assert_eq!(
        repo.get_task(&fail_task)
            .expect("task")
            .expect("task")
            .status,
        TaskStatus::Failed
    );

    let (cancel_project, cancel_run, cancel_task) = scoped_task(&repo, "cancel");
    let cancelled = repo
        .claim_worker_lease(
            cancel_project.as_str(),
            "mission_scope_cancel",
            &cancel_run,
            &cancel_task,
            "worker-cancel",
            "worker-run-cancel",
            60,
        )
        .expect("cancel claim")
        .expect("cancel lease");
    let cancelled = repo
        .cancel_worker_lease(
            &cancelled.id.to_string(),
            "worker-run-cancel",
            cancelled.revision,
        )
        .expect("cancel transition")
        .expect("cancel transition wins");
    assert_eq!(cancelled.status, models::WorkerLeaseStatus::Cancelled);

    let (expire_project, expire_run, expire_task) = scoped_task(&repo, "expire");
    let expiring = repo
        .claim_worker_lease(
            expire_project.as_str(),
            "mission_scope_expire",
            &expire_run,
            &expire_task,
            "worker-expire",
            "worker-run-expire",
            60,
        )
        .expect("expiry claim")
        .expect("expiry lease");
    let mut expired = expiring.clone();
    expired.lease_expires_at = utcnow();
    expired.expires_at = Some(expired.lease_expires_at);
    repo.update_worker_lease(&expired).expect("expire lease");
    assert_eq!(
        repo.reclaim_expired_worker_leases(
            expire_project.as_str(),
            Some("mission_scope_expire"),
            Some(&expire_run),
        )
        .expect("reclaim"),
        1
    );
    let reclaimed = repo
        .claim_worker_lease(
            expire_project.as_str(),
            "mission_scope_expire",
            &expire_run,
            &expire_task,
            "worker-reclaim",
            "worker-run-reclaim",
            60,
        )
        .expect("reclaim claim")
        .expect("expired lease can be reclaimed");
    assert_ne!(reclaimed.id, expiring.id);
}
