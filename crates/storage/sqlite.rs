//! `SqliteRepository` —— `server/core/storage/sqlite_repository.py` 的 rusqlite 移植。
//!
//! 并发策略与 Python 侧逐条对应：
//!
//! - 单一共享连接包在 [`Mutex`] 里（Python：
//!   `check_same_thread=False` + `threading.RLock` 串行化全部读写）；
//! - WAL journal mode + 5s `busy_timeout` 让跨进程共享同一文件时短暂等锁
//!   而非立即失败；
//! - 每次写包在显式事务里（`BEGIN` / `BEGIN IMMEDIATE` → `COMMIT` /
//!   `ROLLBACK`），失败的写绝不留下半写行。
//!
//! SQL 语句照搬 Python 侧文本（表名插值来自 [`Storable::TABLE`] 静态
//! 常量，无用户输入面），保证物理行为一致。

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Mutex;
use std::sync::MutexGuard;
use std::time::Duration;

use models::utcnow;
use models::{
    AgentNarrativeEvent, AgentTask, ArtifactRecord, AuditEvent, AuditEventType, AuditRun,
    BlackboardEntry, BlackboardEntryKind, Branch, BranchId, ContextCompressionReport, ContextPack,
    CoverageAssessment, CritiqueReport, DecisionAnswer, DecisionGate, DecisionGateStatus,
    EscalationGuardVerdict, Evidence, ExecutionJob, ExecutionStatus, ExitGateDecision, Fact,
    Finding, Hint, IntelEntity, IntelEntityKind, IntelEntityRecord, IntelEntityStatus,
    IntelIngestBatch, IntelIngestOutcome, IntelProvenance, IntelRawRecord, IntelRelation,
    IntelRelationRecord, Intent, KnowledgeCard, KnowledgeCorpusStatus, KnowledgeRetrievalQuery,
    KnowledgeRetrievalResult, MetacognitionAssessment, Mission, MissionAsset,
    MissionAssetSensitivity, MissionAssetType, MissionId, ModelCapability, ModelInvocation,
    ModuleConfig, Observation, Project, ProjectId, ProviderConfig, ProviderRouteBinding,
    ReflectorReport, RetrievalInvocation, RunId, RuntimeSetting,
    StrategyBoardSnapshot, TaskStatus, TerminationAssessment, ToolInvocation, TrajectorySummary,
    UserDirective, WorkerInvocation, WorkerInvocationPurpose, WorkerLease, WorkerLeaseStatus,
    WorkerProfile, WorkerRun, WorkerRunStatus, WorkerRuntimeProfile, WorkerRuntimeType,
};
use models::FindingRetest;
use rusqlite::Connection;
use rusqlite::params;
use rusqlite::params_from_iter;
use rusqlite::types::Value as SqlValue;

use crate::error::StorageError;
use crate::knowledge_aliases::AliasRegistry;
use crate::repository::Repository;
use crate::repository::Storable;
use crate::schema;
use crate::schema::ScopeColumn;

/// stdlib-sqlite 驱动的 [`Repository`] 实现。
///
/// 返回的永远是领域模型；DB 行只在本模块内部存在。
pub struct SqliteRepository {
    connection: Mutex<Connection>,
}

impl SqliteRepository {
    /// 打开（必要时创建）数据库文件并初始化 schema。
    ///
    /// 与 Python `__init__` 相同的副作用序列：建父目录 → 打开连接 →
    /// WAL / `busy_timeout` / `foreign_keys` 三个 PRAGMA → `_init_schema`。
    ///
    /// # Errors
    /// - [`StorageError::Directory`]：父目录无法创建；
    /// - [`StorageError::Open`]：文件无法打开；
    /// - [`StorageError::Pragma`] / [`StorageError::Schema`]：初始化失败。
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        let path = path.as_ref();
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent).map_err(|source| StorageError::Directory {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let connection = Connection::open(path).map_err(|source| StorageError::Open {
            path: path.to_path_buf(),
            source,
        })?;
        // journal_mode 返回结果行（新模式名），必须走 query_row 而非 execute。
        let _mode: String = connection
            .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
            .map_err(|source| StorageError::Pragma {
                pragma: "journal_mode",
                source,
            })?;
        connection
            .busy_timeout(Duration::from_millis(5000))
            .map_err(|source| StorageError::Pragma {
                pragma: "busy_timeout",
                source,
            })?;
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .map_err(|source| StorageError::Pragma {
                pragma: "foreign_keys",
                source,
            })?;
        schema::init(&connection)?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    /// 关闭底层连接（等价于 drop；显式表达生命周期意图，对应 Python `close()`）。
    pub fn close(self) {}

    fn lock(&self) -> Result<MutexGuard<'_, Connection>, StorageError> {
        self.connection.lock().map_err(|_| StorageError::Poisoned)
    }
}

/// `BEGIN`（`immediate` 为真时 `BEGIN IMMEDIATE`）→ `body` → `COMMIT`，
/// body 失败则 `ROLLBACK` 并透传原始错误。
///
/// 显式 SQL 事务镜像 Python 侧 `isolation_level=None` + 手动 BEGIN/COMMIT
/// 的控制流；调用方持有的锁保证整个事务在单次临界区内完成。
fn in_transaction<T>(
    connection: &Connection,
    immediate: bool,
    table: &'static str,
    body: impl FnOnce(&Connection) -> Result<T, StorageError>,
) -> Result<T, StorageError> {
    let begin = if immediate {
        "BEGIN IMMEDIATE"
    } else {
        "BEGIN"
    };
    connection
        .execute_batch(begin)
        .map_err(|source| StorageError::Write {
            table: table.to_string(),
            source,
        })?;
    match body(connection) {
        Ok(value) => {
            connection
                .execute_batch("COMMIT")
                .map_err(|source| StorageError::Write {
                    table: table.to_string(),
                    source,
                })?;
            Ok(value)
        }
        Err(error) => {
            // 回滚失败不能覆盖原始错误。
            let _ = connection.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

/// INSERT 语句（Python `_insert` 的列拼接：run-scoped 表在 `project_id` 后
/// 插入 `run_id` 列）。
fn insert_sql(table: &str, run_scoped: bool) -> String {
    if run_scoped {
        format!(
            "INSERT INTO {table} (id, project_id, run_id, payload, created_at) \
             VALUES (?, ?, ?, ?, ?)"
        )
    } else {
        format!(
            "INSERT INTO {table} (id, project_id, payload, created_at) \
             VALUES (?, ?, ?, ?)"
        )
    }
}

/// UPSERT 语句（Python `_upsert`：`ON CONFLICT(id) DO UPDATE SET` 除 id 外
/// 全列取 excluded 值，顺序与 INSERT 列序一致）。
fn upsert_sql(table: &str, run_scoped: bool) -> String {
    if run_scoped {
        format!(
            "INSERT INTO {table} (id, project_id, run_id, payload, created_at) \
             VALUES (?, ?, ?, ?, ?) \
             ON CONFLICT(id) DO UPDATE SET project_id=excluded.project_id, \
             run_id=excluded.run_id, payload=excluded.payload, \
             created_at=excluded.created_at"
        )
    } else {
        format!(
            "INSERT INTO {table} (id, project_id, payload, created_at) \
             VALUES (?, ?, ?, ?) \
             ON CONFLICT(id) DO UPDATE SET project_id=excluded.project_id, \
             payload=excluded.payload, created_at=excluded.created_at"
        )
    }
}

fn write_error(table: &'static str) -> impl Fn(rusqlite::Error) -> StorageError {
    move |source| StorageError::Write {
        table: table.to_string(),
        source,
    }
}

fn serialize_payload<E: Storable>(entity: &E) -> Result<String, StorageError> {
    serde_json::to_string(entity).map_err(|source| StorageError::Serialize {
        table: E::TABLE.to_string(),
        source,
    })
}

fn decode<E: Storable>(payload: &str) -> Result<E, StorageError> {
    serde_json::from_str(payload).map_err(|source| StorageError::Decode {
        table: E::TABLE.to_string(),
        source,
    })
}

/// 单条 INSERT（须已在事务内）。
fn insert<E: Storable>(connection: &Connection, entity: &E) -> Result<(), StorageError> {
    let payload = serialize_payload(entity)?;
    let created_at = entity.created_at_column();
    let outcome = if E::RUN_SCOPED {
        connection.execute(
            &insert_sql(E::TABLE, true),
            params![
                entity.entity_id(),
                entity.scope_project_id(),
                entity.scope_run_id(),
                payload,
                created_at
            ],
        )
    } else {
        connection.execute(
            &insert_sql(E::TABLE, false),
            params![
                entity.entity_id(),
                entity.scope_project_id(),
                payload,
                created_at
            ],
        )
    };
    outcome.map_err(write_error(E::TABLE)).map(|_| ())
}

/// 单条 UPSERT（须已在事务内）。
fn upsert<E: Storable>(connection: &Connection, entity: &E) -> Result<(), StorageError> {
    let payload = serialize_payload(entity)?;
    let created_at = entity.created_at_column();
    let outcome = if E::RUN_SCOPED {
        connection.execute(
            &upsert_sql(E::TABLE, true),
            params![
                entity.entity_id(),
                entity.scope_project_id(),
                entity.scope_run_id(),
                payload,
                created_at
            ],
        )
    } else {
        connection.execute(
            &upsert_sql(E::TABLE, false),
            params![
                entity.entity_id(),
                entity.scope_project_id(),
                payload,
                created_at
            ],
        )
    };
    outcome.map_err(write_error(E::TABLE)).map(|_| ())
}

/// 按 id 取单个实体（Python `_get`）。
fn get<E: Storable>(connection: &Connection, id: &str) -> Result<Option<E>, StorageError> {
    let sql = format!("SELECT payload FROM {} WHERE id = ?", E::TABLE);
    match connection.query_row(&sql, params![id], |row| row.get::<_, String>(0)) {
        Ok(payload) => decode::<E>(&payload).map(Some),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(source) => Err(StorageError::Query {
            table: E::TABLE.to_string(),
            source,
        }),
    }
}

/// 执行 SELECT payload 查询并解码全部行（列名来自本模块的静态字面量）。
fn query_payloads<E: Storable>(
    connection: &Connection,
    sql: &str,
    bind: impl rusqlite::Params,
) -> Result<Vec<E>, StorageError> {
    let query_error = |source: rusqlite::Error| StorageError::Query {
        table: E::TABLE.to_string(),
        source,
    };
    let mut statement = connection.prepare(sql).map_err(query_error)?;
    let payloads = statement
        .query_map(bind, |row| row.get::<_, String>(0))
        .map_err(query_error)?;
    let mut items = Vec::new();
    for payload in payloads {
        let payload = payload.map_err(query_error)?;
        items.push(decode::<E>(&payload)?);
    }
    Ok(items)
}

/// 按单列等值过滤（Python `_list_where`），`ORDER BY seq ASC`。
///
/// 列名走 [`ScopeColumn`] 枚举：合法过滤列在类型层面封闭，拼写错误
/// 编译期暴露，而非运行期 SQL 失败。
fn list_where<E: Storable>(
    connection: &Connection,
    column: ScopeColumn,
    value: &str,
) -> Result<Vec<E>, StorageError> {
    let sql = format!(
        "SELECT payload FROM {table} WHERE {column} = ? ORDER BY seq ASC",
        table = E::TABLE,
        column = column.as_str()
    );
    query_payloads::<E>(connection, &sql, params![value])
}

/// 全表按 `seq` 升序（Python `_list_all`）。
fn list_all<E: Storable>(connection: &Connection) -> Result<Vec<E>, StorageError> {
    let sql = format!("SELECT payload FROM {} ORDER BY seq ASC", E::TABLE);
    query_payloads::<E>(connection, &sql, [])
}

/// Project + 可选 Run 的 scope 过滤（Python `_list_project_run`）：`run_id`
/// 为 `None` 时退化为单列 `project_id` 过滤，两者 SQL 文本与 Python 一致。
fn list_project_run<E: Storable>(
    connection: &Connection,
    project_id: &str,
    run_id: Option<&str>,
) -> Result<Vec<E>, StorageError> {
    match run_id {
        None => list_where(connection, ScopeColumn::ProjectId, project_id),
        Some(run_id) => {
            let sql = format!(
                "SELECT payload FROM {} WHERE project_id = ? AND run_id = ? ORDER BY seq ASC",
                E::TABLE
            );
            query_payloads::<E>(connection, &sql, params![project_id, run_id])
        }
    }
}

fn blackboard_event(entry: &BlackboardEntry) -> Result<AuditEvent, StorageError> {
    let mut event = AuditEvent::new(
        entry.project_id.clone(),
        AuditEventType::UserNote,
        entry.author_worker_run_id.clone(),
        format!("blackboard:{}", entry.kind.as_str()),
    );
    event.id = entry.entry_id.clone();
    event.run_id = Some(entry.run_id.clone());
    event.task_id = entry.task_id.clone();
    event.created_at = entry.created_at;
    event.data.insert("blackboard".to_string(), true.into());
    event.data.insert(
        "entry".to_string(),
        serde_json::to_value(entry).map_err(|source| StorageError::Serialize {
            table: AuditEvent::TABLE.to_string(),
            source,
        })?,
    );
    Ok(event)
}

fn decode_blackboard_row(
    sequence: i64,
    payload: &str,
) -> Result<Option<BlackboardEntry>, StorageError> {
    let event = decode::<AuditEvent>(payload)?;
    if event.event_type != AuditEventType::UserNote
        || event.data.get("blackboard") != Some(&true.into())
    {
        return Ok(None);
    }
    let Some(value) = event.data.get("entry") else {
        return Err(StorageError::InvalidArgument {
            entity: "blackboard event",
            message: format!("audit event '{}' has no entry payload", event.id),
        });
    };
    let mut entry: BlackboardEntry =
        serde_json::from_value(value.clone()).map_err(|source| StorageError::Decode {
            table: AuditEvent::TABLE.to_string(),
            source,
        })?;
    entry.sequence = sequence;
    entry
        .validate()
        .map_err(|message| StorageError::InvalidArgument {
            entity: "stored blackboard entry",
            message,
        })?;
    Ok(Some(entry))
}

fn query_blackboard_rows(
    connection: &Connection,
    project_id: &str,
    run_id: &str,
    after_sequence: Option<i64>,
) -> Result<Vec<BlackboardEntry>, StorageError> {
    let after = after_sequence.unwrap_or(0);
    let mut statement = connection
        .prepare(
            "SELECT seq, payload FROM audit_events \
             WHERE project_id = ? AND run_id = ? AND seq > ? ORDER BY seq ASC",
        )
        .map_err(write_error(AuditEvent::TABLE))?;
    let rows = statement
        .query_map(params![project_id, run_id, after], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(write_error(AuditEvent::TABLE))?;
    let mut entries = Vec::new();
    for row in rows {
        let (sequence, payload) = row.map_err(write_error(AuditEvent::TABLE))?;
        if let Some(entry) = decode_blackboard_row(sequence, &payload)? {
            entries.push(entry);
        }
    }
    Ok(entries)
}

fn same_blackboard_request(left: &BlackboardEntry, right: &BlackboardEntry) -> bool {
    left.project_id == right.project_id
        && left.mission_id == right.mission_id
        && left.run_id == right.run_id
        && left.branch_id == right.branch_id
        && left.task_id == right.task_id
        && left.intent_id == right.intent_id
        && left.author_worker_run_id == right.author_worker_run_id
        && left.kind == right.kind
        && left.content == right.content
        && left.artifact_id == right.artifact_id
        && left.evidence_id == right.evidence_id
        && left.locator == right.locator
        && left.idempotency_key == right.idempotency_key
}

fn valid_lease_argument(entity: &'static str, value: &str) -> Result<(), StorageError> {
    if value.trim().is_empty() {
        return Err(StorageError::InvalidArgument {
            entity,
            message: "must be non-empty".to_string(),
        });
    }
    Ok(())
}

fn expire_leases_in_scope(
    connection: &Connection,
    project_id: &str,
    mission_id: Option<&str>,
    run_id: Option<&str>,
) -> Result<usize, StorageError> {
    let now = utcnow();
    let leases = list_project_run::<WorkerLease>(connection, project_id, run_id)?;
    let mut changed = 0;
    for mut lease in leases {
        if lease.status != WorkerLeaseStatus::Active
            || mission_id.is_some_and(|id| {
                lease
                    .mission_id
                    .as_ref()
                    .is_none_or(|value| value.as_str() != id)
            })
            || lease.lease_expires_at > now
        {
            continue;
        }
        lease.status = WorkerLeaseStatus::Expired;
        lease.updated_at = now;
        lease.heartbeat_at = now;
        lease.revision = lease.revision.saturating_add(1);
        lease.expires_at = Some(lease.lease_expires_at);
        upsert(connection, &lease)?;
        changed += 1;
    }
    Ok(changed)
}

fn transition_lease(
    connection: &Connection,
    lease_id: &str,
    worker_run_id: &str,
    revision: i64,
    status: WorkerLeaseStatus,
) -> Result<Option<WorkerLease>, StorageError> {
    let Some(mut lease) = get::<WorkerLease>(connection, lease_id)? else {
        return Ok(None);
    };
    let now = utcnow();
    if lease.status != WorkerLeaseStatus::Active
        || lease.worker_run_id.as_deref() != Some(worker_run_id)
        || lease.revision != revision
    {
        return Ok(None);
    }
    if lease.lease_expires_at <= now {
        lease.status = WorkerLeaseStatus::Expired;
        lease.updated_at = now;
        lease.heartbeat_at = now;
        lease.revision = lease.revision.saturating_add(1);
        upsert(connection, &lease)?;
        return Ok(None);
    }
    lease.status = status;
    lease.updated_at = now;
    lease.revision = lease.revision.saturating_add(1);
    if status == WorkerLeaseStatus::Cancelled {
        lease.cancelled_at = Some(now);
    }
    if let Some(task_id) = lease.task_id.as_ref()
        && let Some(mut task) = get::<AgentTask>(connection, task_id.as_str())?
    {
        task.status = match status {
            WorkerLeaseStatus::Completed => TaskStatus::Succeeded,
            WorkerLeaseStatus::Failed => TaskStatus::Failed,
            WorkerLeaseStatus::Cancelled => TaskStatus::Cancelled,
            _ => task.status,
        };
        if task.status != TaskStatus::Running {
            task.finished_at = Some(now);
        }
        upsert(connection, &task)?;
    }
    upsert(connection, &lease)?;
    Ok(Some(lease))
}

/// append-only 插入的约束冲突识别：UNIQUE 违规映射为
/// [`StorageError::AppendOnlyConflict`]（Python `ValueError` 的对应），
/// 其余写错误原样透传。
fn append_only_conflict(error: StorageError, entity: &str, id: &str) -> StorageError {
    if let StorageError::Write {
        source: rusqlite::Error::SqliteFailure(failure, _),
        ..
    } = &error
        && failure.code == rusqlite::ErrorCode::ConstraintViolation
    {
        // Python 报文用 `table[:-1]` 单数化（facts → fact）。
        let singular = entity.strip_suffix('s').unwrap_or(entity);
        return StorageError::AppendOnlyConflict {
            entity: singular.to_string(),
            id: id.to_string(),
        };
    }
    error
}

/// `commit_solver_result` 事务体的约束冲突映射（Python `except
/// sqlite3.IntegrityError: raise ValueError("solver result commit failed")`
/// 的类型化对应）：错误细节被 Python 有意吞掉，报文固定。
fn solver_commit_conflict(error: StorageError) -> StorageError {
    if let StorageError::Write {
        source: rusqlite::Error::SqliteFailure(failure, _),
        ..
    } = &error
        && failure.code == rusqlite::ErrorCode::ConstraintViolation
    {
        return StorageError::SolverCommitConflict;
    }
    error
}

/// `MissionAsset` 的 payload 解码（专用表，不走 [`Storable`]）。
fn decode_mission_asset(payload: &str) -> Result<MissionAsset, StorageError> {
    serde_json::from_str(payload).map_err(|source| StorageError::Decode {
        table: "mission_assets".to_string(),
        source,
    })
}

/// `SELECT payload FROM mission_assets` 全行解码（绑定参数直传）。
fn query_payloads_mission_asset(
    connection: &Connection,
    sql: &str,
    values: Vec<SqlValue>,
) -> Result<Vec<MissionAsset>, StorageError> {
    let mut statement = connection
        .prepare(sql)
        .map_err(|source| StorageError::Query {
            table: "mission_assets".to_string(),
            source,
        })?;
    let payloads = statement
        .query_map(params_from_iter(values), |row| row.get::<_, String>(0))
        .map_err(|source| StorageError::Query {
            table: "mission_assets".to_string(),
            source,
        })?;
    let mut assets = Vec::new();
    for payload in payloads {
        let payload = payload.map_err(|source| StorageError::Query {
            table: "mission_assets".to_string(),
            source,
        })?;
        assets.push(decode_mission_asset(&payload)?);
    }
    Ok(assets)
}

/// Python `_insert_mission_asset_in_current_transaction`：专用列布局
/// （`asset_type` + `normalized_value` 去重键），`upsert` 为真时按 id 冲突
/// 更新全列。
fn insert_mission_asset(
    connection: &Connection,
    asset: &MissionAsset,
    upsert: bool,
) -> Result<(), StorageError> {
    let normalized = models::normalize_mission_asset_value(asset.asset_type, &asset.value);
    let mut sql = "INSERT INTO mission_assets \
                   (id, project_id, mission_id, run_id, asset_type, normalized_value, \
                    payload, created_at, updated_at) \
                   VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)"
        .to_string();
    if upsert {
        sql.push_str(
            " ON CONFLICT(id) DO UPDATE SET \
             project_id=excluded.project_id, mission_id=excluded.mission_id, \
             run_id=excluded.run_id, asset_type=excluded.asset_type, \
             normalized_value=excluded.normalized_value, payload=excluded.payload, \
             created_at=excluded.created_at, updated_at=excluded.updated_at",
        );
    }
    let payload = serialize_payload_mission_asset(asset)?;
    connection
        .execute(
            &sql,
            params![
                asset.id.as_str(),
                asset.project_id.as_str(),
                asset.mission_id.as_str(),
                asset.run_id.as_ref().map(models::RunId::as_str),
                asset.asset_type.as_str(),
                normalized,
                payload,
                asset.created_at.isoformat(),
                asset.updated_at.isoformat(),
            ],
        )
        .map_err(write_error("mission_assets"))
        .map(|_| ())
}

fn serialize_payload_mission_asset(asset: &MissionAsset) -> Result<String, StorageError> {
    serde_json::to_string(asset).map_err(|source| StorageError::Serialize {
        table: "mission_assets".to_string(),
        source,
    })
}

impl Repository for SqliteRepository {
    fn create_mission(&self, mission: &Mission) -> Result<Mission, StorageError> {
        let connection = self.lock()?;
        in_transaction(&connection, false, Mission::TABLE, |tx| insert(tx, mission))?;
        Ok(mission.clone())
    }

    fn get_mission(&self, mission_id: &str) -> Result<Option<Mission>, StorageError> {
        let connection = self.lock()?;
        get::<Mission>(&connection, mission_id)
    }

    fn list_missions(&self, project_id: Option<&str>) -> Result<Vec<Mission>, StorageError> {
        let connection = self.lock()?;
        match project_id {
            None => list_all(&connection),
            Some(project_id) => list_where(&connection, ScopeColumn::ProjectId, project_id),
        }
    }

    fn update_mission(&self, mission: &Mission) -> Result<Mission, StorageError> {
        let connection = self.lock()?;
        in_transaction(&connection, false, Mission::TABLE, |tx| upsert(tx, mission))?;
        Ok(mission.clone())
    }

    fn delete_mission(&self, mission_id: &str) -> Result<(), StorageError> {
        let connection = self.lock()?;
        in_transaction(&connection, false, Mission::TABLE, |tx| {
            tx.execute("DELETE FROM missions WHERE id = ?", params![mission_id])
                .map_err(write_error(Mission::TABLE))?;
            tx.execute(
                "DELETE FROM mission_assets WHERE mission_id = ?",
                params![mission_id],
            )
            .map_err(write_error("mission_assets"))?;
            // 彻底删除：finding / evidence 也按 mission 级联删。它们是 generic 表，
            // mission_id 在 payload 里（非物理列），故用 json_extract 匹配；删
            // mission 不该留下冒名"项目存档"的孤儿 finding/evidence。
            tx.execute(
                "DELETE FROM findings WHERE json_extract(payload, '$.mission_id') = ?",
                params![mission_id],
            )
            .map_err(write_error("findings"))?;
            tx.execute(
                "DELETE FROM evidence WHERE json_extract(payload, '$.mission_id') = ?",
                params![mission_id],
            )
            .map_err(write_error("evidence"))?;
            // 外部 worker 审计链同样按 mission 级联删。worker_runs 的
            // mission_id 在 payload 里（非物理列）；worker_invocations 没有
            // mission_id，只能经 worker_run_id 反查——所以必须先删 invocation，
            // 否则把 worker_runs 删光后子查询就匹配不到任何行了。
            //
            // 不删的后果不只是留孤儿行：仍在跑的 worker 收尾时会把自己那行
            // upsert 回来，于是"已删除的任务"又出现在 Worker 审计页里。
            tx.execute(
                "DELETE FROM worker_invocations WHERE worker_run_id IN (\
                 SELECT id FROM worker_runs WHERE json_extract(payload, '$.mission_id') = ?)",
                params![mission_id],
            )
            .map_err(write_error("worker_invocations"))?;
            tx.execute(
                "DELETE FROM worker_runs WHERE json_extract(payload, '$.mission_id') = ?",
                params![mission_id],
            )
            .map_err(write_error("worker_runs"))?;
            Ok(())
        })
    }

    fn create_branch(&self, branch: &Branch) -> Result<Branch, StorageError> {
        let connection = self.lock()?;
        in_transaction(&connection, false, Branch::TABLE, |tx| insert(tx, branch))?;
        Ok(branch.clone())
    }

    fn get_branch(&self, branch_id: &str) -> Result<Option<Branch>, StorageError> {
        let connection = self.lock()?;
        get::<Branch>(&connection, branch_id)
    }

    fn list_branches(
        &self,
        project_id: Option<&str>,
        mission_id: Option<&str>,
        run_id: Option<&str>,
    ) -> Result<Vec<Branch>, StorageError> {
        let connection = self.lock()?;
        // 条件拼接镜像 Python `_list_filtered`：project_id / run_id 进 SQL，
        // mission_id 是 payload 字段，查询后内存过滤。
        let mut conditions: Vec<String> = Vec::new();
        let mut values: Vec<&str> = Vec::new();
        if let Some(project_id) = project_id {
            conditions.push(format!("{} = ?", ScopeColumn::ProjectId.as_str()));
            values.push(project_id);
        }
        if let Some(run_id) = run_id {
            conditions.push(format!("{} = ?", ScopeColumn::RunId.as_str()));
            values.push(run_id);
        }
        let where_clause = if conditions.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", conditions.join(" AND "))
        };
        let sql = format!(
            "SELECT payload FROM {}{} ORDER BY seq ASC",
            Branch::TABLE,
            where_clause
        );
        let mut branches = query_payloads::<Branch>(&connection, &sql, params_from_iter(values))?;
        if let Some(mission_id) = mission_id {
            branches.retain(|branch| branch.mission_id.as_str() == mission_id);
        }
        Ok(branches)
    }

    fn update_branch(&self, branch: &Branch) -> Result<Branch, StorageError> {
        let connection = self.lock()?;
        in_transaction(&connection, false, Branch::TABLE, |tx| upsert(tx, branch))?;
        Ok(branch.clone())
    }

    fn create_run(&self, run: &AuditRun) -> Result<AuditRun, StorageError> {
        let connection = self.lock()?;
        in_transaction(&connection, false, AuditRun::TABLE, |tx| insert(tx, run))?;
        Ok(run.clone())
    }

    fn get_run(&self, run_id: &str) -> Result<Option<AuditRun>, StorageError> {
        let connection = self.lock()?;
        get::<AuditRun>(&connection, run_id)
    }

    fn list_runs(&self, project_id: &str) -> Result<Vec<AuditRun>, StorageError> {
        let connection = self.lock()?;
        list_where(&connection, ScopeColumn::ProjectId, project_id)
    }

    fn update_run(&self, run: &AuditRun) -> Result<AuditRun, StorageError> {
        let connection = self.lock()?;
        in_transaction(&connection, false, AuditRun::TABLE, |tx| upsert(tx, run))?;
        Ok(run.clone())
    }

    fn increment_run_steps(
        &self,
        run_id: &str,
        delta: i64,
    ) -> Result<Option<AuditRun>, StorageError> {
        if delta < 0 {
            return Err(StorageError::NegativeStepDelta { delta });
        }
        let connection = self.lock()?;
        in_transaction(&connection, true, AuditRun::TABLE, |tx| {
            let Some(mut run) = get::<AuditRun>(tx, run_id)? else {
                return Ok(None);
            };
            run.steps_used += delta;
            run.updated_at = utcnow();
            upsert(tx, &run)?;
            Ok(Some(run))
        })
    }

    fn reserve_run_steps(&self, run_id: &str, amount: i64) -> Result<bool, StorageError> {
        if amount < 1 {
            return Err(StorageError::InvalidArgument {
                entity: "run step reservation",
                message: "amount must be positive".to_string(),
            });
        }
        let connection = self.lock()?;
        in_transaction(&connection, true, AuditRun::TABLE, |tx| {
            let Some(mut run) = get::<AuditRun>(tx, run_id)? else {
                return Ok(false);
            };
            let Some(next) = run.steps_used.checked_add(amount) else {
                return Ok(false);
            };
            if next > run.max_total_steps {
                return Ok(false);
            }
            run.steps_used = next;
            run.updated_at = utcnow();
            upsert(tx, &run)?;
            Ok(true)
        })
    }

    fn append_run_task(
        &self,
        run_id: &str,
        task_id: &str,
    ) -> Result<Option<AuditRun>, StorageError> {
        let connection = self.lock()?;
        in_transaction(&connection, true, AuditRun::TABLE, |tx| {
            let Some(mut run) = get::<AuditRun>(tx, run_id)? else {
                return Ok(None);
            };
            if !run.task_ids.iter().any(|existing| existing == task_id) {
                run.task_ids.push(task_id.to_string());
            }
            run.updated_at = utcnow();
            upsert(tx, &run)?;
            Ok(Some(run))
        })
    }

    fn create_task(&self, task: &AgentTask) -> Result<AgentTask, StorageError> {
        let connection = self.lock()?;
        in_transaction(&connection, false, AgentTask::TABLE, |tx| insert(tx, task))?;
        Ok(task.clone())
    }

    fn get_task(&self, task_id: &str) -> Result<Option<AgentTask>, StorageError> {
        let connection = self.lock()?;
        get::<AgentTask>(&connection, task_id)
    }

    fn list_tasks(&self, run_id: &str) -> Result<Vec<AgentTask>, StorageError> {
        let connection = self.lock()?;
        list_where(&connection, ScopeColumn::RunId, run_id)
    }

    fn update_task(&self, task: &AgentTask) -> Result<AgentTask, StorageError> {
        let connection = self.lock()?;
        in_transaction(&connection, false, AgentTask::TABLE, |tx| upsert(tx, task))?;
        Ok(task.clone())
    }

    #[allow(clippy::too_many_arguments)]
    fn commit_solver_result(
        &self,
        task: &AgentTask,
        intent: &Intent,
        tool_invocations: &[ToolInvocation],
        facts: &[Fact],
        evidence: &[Evidence],
        findings: &[Finding],
        proposed_intents: &[Intent],
        events: &[AuditEvent],
    ) -> Result<(), StorageError> {
        let connection = self.lock()?;
        // 写入顺序照搬 Python：invocations → facts → evidence → findings →
        // proposed intents 依次 INSERT，task 与 intent UPSERT，events 最后
        // INSERT；任一步失败整体回滚。
        let commit_body = |tx: &Connection| -> Result<(), StorageError> {
            for inv in tool_invocations {
                insert(tx, inv)?;
            }
            for fact in facts {
                insert(tx, fact)?;
            }
            for item in evidence {
                insert(tx, item)?;
            }
            for finding in findings {
                insert(tx, finding)?;
            }
            for proposed in proposed_intents {
                insert(tx, proposed)?;
            }
            upsert(tx, task)?;
            upsert(tx, intent)?;
            for event in events {
                insert(tx, event)?;
            }
            Ok(())
        };
        in_transaction(&connection, false, AgentTask::TABLE, commit_body)
            .map_err(solver_commit_conflict)
    }

    fn create_execution_job(&self, job: &ExecutionJob) -> Result<ExecutionJob, StorageError> {
        let connection = self.lock()?;
        in_transaction(&connection, false, ExecutionJob::TABLE, |tx| {
            insert(tx, job)
        })
        .map_err(|error| append_only_conflict(error, ExecutionJob::TABLE, job.id.as_str()))?;
        Ok(job.clone())
    }

    fn update_execution_job(&self, job: &ExecutionJob) -> Result<ExecutionJob, StorageError> {
        let connection = self.lock()?;
        in_transaction(&connection, false, ExecutionJob::TABLE, |tx| {
            if get::<ExecutionJob>(tx, job.id.as_str())?.is_none() {
                return Err(StorageError::NotFound {
                    entity: "execution job",
                    id: job.id.as_str().to_string(),
                });
            }
            upsert(tx, job)
        })?;
        Ok(job.clone())
    }

    fn get_execution_job(&self, execution_id: &str) -> Result<Option<ExecutionJob>, StorageError> {
        let connection = self.lock()?;
        get::<ExecutionJob>(&connection, execution_id)
    }

    fn list_execution_jobs(
        &self,
        project_id: Option<&str>,
        run_id: Option<&str>,
        status: Option<ExecutionStatus>,
    ) -> Result<Vec<ExecutionJob>, StorageError> {
        let connection = self.lock()?;
        let mut jobs = match (project_id, run_id) {
            (Some(project_id), run_id) => {
                list_project_run::<ExecutionJob>(&connection, project_id, run_id)?
            }
            (None, Some(run_id)) => {
                list_where::<ExecutionJob>(&connection, ScopeColumn::RunId, run_id)?
            }
            (None, None) => list_all::<ExecutionJob>(&connection)?,
        };
        if let Some(status) = status {
            jobs.retain(|job| job.status == status);
        }
        Ok(jobs)
    }

    fn add_tool_invocation(&self, inv: &ToolInvocation) -> Result<ToolInvocation, StorageError> {
        let connection = self.lock()?;
        in_transaction(&connection, false, ToolInvocation::TABLE, |tx| {
            insert(tx, inv)
        })?;
        Ok(inv.clone())
    }

    fn update_tool_invocation(&self, inv: &ToolInvocation) -> Result<ToolInvocation, StorageError> {
        let connection = self.lock()?;
        in_transaction(&connection, false, ToolInvocation::TABLE, |tx| {
            upsert(tx, inv)
        })?;
        Ok(inv.clone())
    }

    fn list_tool_invocations(
        &self,
        project_id: Option<&str>,
    ) -> Result<Vec<ToolInvocation>, StorageError> {
        let connection = self.lock()?;
        match project_id {
            None => list_all(&connection),
            Some(project_id) => list_where(&connection, ScopeColumn::ProjectId, project_id),
        }
    }

    fn add_evidence(&self, evidence: &Evidence) -> Result<Evidence, StorageError> {
        let connection = self.lock()?;
        in_transaction(&connection, false, Evidence::TABLE, |tx| {
            insert(tx, evidence)
        })?;
        Ok(evidence.clone())
    }

    fn update_evidence(&self, evidence: &Evidence) -> Result<Evidence, StorageError> {
        let connection = self.lock()?;
        in_transaction(&connection, false, Evidence::TABLE, |tx| {
            upsert(tx, evidence)
        })?;
        Ok(evidence.clone())
    }

    fn list_evidence(&self, project_id: &str) -> Result<Vec<Evidence>, StorageError> {
        let connection = self.lock()?;
        list_where(&connection, ScopeColumn::ProjectId, project_id)
    }

    fn add_finding(&self, finding: &Finding) -> Result<Finding, StorageError> {
        let connection = self.lock()?;
        in_transaction(&connection, false, Finding::TABLE, |tx| insert(tx, finding))?;
        Ok(finding.clone())
    }

    fn get_finding(&self, finding_id: &str) -> Result<Option<Finding>, StorageError> {
        let connection = self.lock()?;
        get::<Finding>(&connection, finding_id)
    }

    fn list_findings(&self, project_id: &str) -> Result<Vec<Finding>, StorageError> {
        let connection = self.lock()?;
        list_where(&connection, ScopeColumn::ProjectId, project_id)
    }

    fn update_finding(&self, finding: &Finding) -> Result<Finding, StorageError> {
        let connection = self.lock()?;
        in_transaction(&connection, false, Finding::TABLE, |tx| upsert(tx, finding))?;
        Ok(finding.clone())
    }

    fn add_finding_retest(&self, retest: &FindingRetest) -> Result<FindingRetest, StorageError> {
        let connection = self.lock()?;
        in_transaction(&connection, false, FindingRetest::TABLE, |tx| {
            insert(tx, retest)
        })?;
        Ok(retest.clone())
    }

    fn get_finding_retest(&self, retest_id: &str) -> Result<Option<FindingRetest>, StorageError> {
        let connection = self.lock()?;
        get::<FindingRetest>(&connection, retest_id)
    }

    fn list_finding_retests(&self, project_id: &str) -> Result<Vec<FindingRetest>, StorageError> {
        let connection = self.lock()?;
        let mut items = list_where::<FindingRetest>(&connection, ScopeColumn::ProjectId, project_id)?;
        // 新建的在前：复测面板要能立刻看到刚发起的那条。
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(items)
    }

    fn update_finding_retest(
        &self,
        retest: &FindingRetest,
    ) -> Result<FindingRetest, StorageError> {
        let connection = self.lock()?;
        in_transaction(&connection, false, FindingRetest::TABLE, |tx| {
            upsert(tx, retest)
        })?;
        Ok(retest.clone())
    }

    fn create_provider(&self, provider: &ProviderConfig) -> Result<ProviderConfig, StorageError> {
        let connection = self.lock()?;
        in_transaction(&connection, false, ProviderConfig::TABLE, |tx| {
            if provider.is_default {
                clear_other_default_providers(tx, provider.entity_id())?;
            }
            insert(tx, provider)
        })?;
        Ok(provider.clone())
    }

    fn get_provider(&self, provider_id: &str) -> Result<Option<ProviderConfig>, StorageError> {
        let connection = self.lock()?;
        get::<ProviderConfig>(&connection, provider_id)
    }

    fn list_providers(&self) -> Result<Vec<ProviderConfig>, StorageError> {
        let connection = self.lock()?;
        list_all(&connection)
    }

    fn update_provider(&self, provider: &ProviderConfig) -> Result<ProviderConfig, StorageError> {
        let connection = self.lock()?;
        in_transaction(&connection, false, ProviderConfig::TABLE, |tx| {
            if provider.is_default {
                clear_other_default_providers(tx, provider.entity_id())?;
            }
            upsert(tx, provider)
        })?;
        Ok(provider.clone())
    }

    fn delete_provider(&self, provider_id: &str) -> Result<(), StorageError> {
        let connection = self.lock()?;
        in_transaction(&connection, false, ProviderConfig::TABLE, |tx| {
            tx.execute("DELETE FROM providers WHERE id = ?", params![provider_id])
                .map_err(write_error(ProviderConfig::TABLE))?;
            Ok(())
        })
    }

    fn upsert_provider_route(
        &self,
        route: &ProviderRouteBinding,
    ) -> Result<ProviderRouteBinding, StorageError> {
        let connection = self.lock()?;
        in_transaction(&connection, false, ProviderRouteBinding::TABLE, |tx| {
            upsert(tx, route)
        })?;
        Ok(route.clone())
    }

    fn get_provider_route(
        &self,
        route_id: &str,
    ) -> Result<Option<ProviderRouteBinding>, StorageError> {
        let connection = self.lock()?;
        get::<ProviderRouteBinding>(&connection, route_id)
    }

    fn list_provider_routes(
        &self,
        purpose: Option<&str>,
    ) -> Result<Vec<ProviderRouteBinding>, StorageError> {
        let connection = self.lock()?;
        let mut routes = list_all::<ProviderRouteBinding>(&connection)?;
        if let Some(purpose) = purpose {
            let normalized = purpose.trim().to_lowercase();
            routes.retain(|route| route.purpose == normalized);
        }
        // Python `sorted(key=(priority, weight), reverse=True)`：降序稳定排序，
        // 相等键保持插入序。
        routes.sort_by_key(|route| std::cmp::Reverse((route.priority, route.weight)));
        Ok(routes)
    }

    fn delete_provider_route(&self, route_id: &str) -> Result<(), StorageError> {
        let connection = self.lock()?;
        in_transaction(&connection, false, ProviderRouteBinding::TABLE, |tx| {
            tx.execute(
                "DELETE FROM provider_routes WHERE id = ?",
                params![route_id],
            )
            .map_err(write_error(ProviderRouteBinding::TABLE))?;
            Ok(())
        })
    }

    fn add_model_invocation(&self, inv: &ModelInvocation) -> Result<ModelInvocation, StorageError> {
        let connection = self.lock()?;
        in_transaction(&connection, false, ModelInvocation::TABLE, |tx| {
            insert(tx, inv)
        })?;
        Ok(inv.clone())
    }

    fn list_model_invocations(
        &self,
        project_id: Option<&str>,
    ) -> Result<Vec<ModelInvocation>, StorageError> {
        let connection = self.lock()?;
        match project_id {
            None => list_all(&connection),
            Some(project_id) => list_where(&connection, ScopeColumn::ProjectId, project_id),
        }
    }

    fn upsert_model_capability(
        &self,
        capability: &ModelCapability,
    ) -> Result<ModelCapability, StorageError> {
        let connection = self.lock()?;
        in_transaction(&connection, false, ModelCapability::TABLE, |tx| {
            upsert(tx, capability)
        })?;
        Ok(capability.clone())
    }

    fn list_model_capabilities(
        &self,
        provider_id: Option<&str>,
    ) -> Result<Vec<ModelCapability>, StorageError> {
        let connection = self.lock()?;
        let capabilities: Vec<ModelCapability> = list_all(&connection)?;
        Ok(provider_id.map_or(capabilities.clone(), |provider_id| {
            capabilities
                .into_iter()
                .filter(|capability| capability.provider_id.as_str() == provider_id)
                .collect()
        }))
    }

    fn create_project(&self, project: &Project) -> Result<Project, StorageError> {
        let connection = self.lock()?;
        in_transaction(&connection, false, Project::TABLE, |tx| insert(tx, project))?;
        Ok(project.clone())
    }

    fn get_project(&self, project_id: &str) -> Result<Option<Project>, StorageError> {
        let connection = self.lock()?;
        get::<Project>(&connection, project_id)
    }

    fn list_projects(&self) -> Result<Vec<Project>, StorageError> {
        let connection = self.lock()?;
        list_all(&connection)
    }

    fn update_project(&self, project: &Project) -> Result<Project, StorageError> {
        let connection = self.lock()?;
        in_transaction(&connection, false, Project::TABLE, |tx| upsert(tx, project))?;
        Ok(project.clone())
    }

    fn delete_project(&self, project_id: &str) -> Result<(), StorageError> {
        // Python：遍历 `_TABLES`，逐表 DELETE 该 project_id 的行，单事务。
        // 表名来自 schema::TABLES 静态常量，无用户输入面。
        let connection = self.lock()?;
        in_transaction(&connection, false, Project::TABLE, |tx| {
            for table in schema::TABLES {
                tx.execute(
                    &format!("DELETE FROM {table} WHERE project_id = ?"),
                    params![project_id],
                )
                .map_err(|source| StorageError::Write {
                    table: table.to_string(),
                    source,
                })?;
            }
            Ok(())
        })
    }

    fn add_fact(&self, fact: &Fact) -> Result<Fact, StorageError> {
        let connection = self.lock()?;
        in_transaction(&connection, false, Fact::TABLE, |tx| insert(tx, fact))
            .map_err(|error| append_only_conflict(error, Fact::TABLE, fact.entity_id()))?;
        Ok(fact.clone())
    }

    fn list_facts(&self, project_id: &str) -> Result<Vec<Fact>, StorageError> {
        let connection = self.lock()?;
        list_where(&connection, ScopeColumn::ProjectId, project_id)
    }

    fn add_intent(&self, intent: &Intent) -> Result<Intent, StorageError> {
        let connection = self.lock()?;
        in_transaction(&connection, false, Intent::TABLE, |tx| insert(tx, intent))?;
        Ok(intent.clone())
    }

    fn get_intent(&self, intent_id: &str) -> Result<Option<Intent>, StorageError> {
        let connection = self.lock()?;
        get::<Intent>(&connection, intent_id)
    }

    fn list_intents(&self, project_id: &str) -> Result<Vec<Intent>, StorageError> {
        let connection = self.lock()?;
        list_where(&connection, ScopeColumn::ProjectId, project_id)
    }

    fn update_intent(&self, intent: &Intent) -> Result<Intent, StorageError> {
        let connection = self.lock()?;
        in_transaction(&connection, false, Intent::TABLE, |tx| upsert(tx, intent))?;
        Ok(intent.clone())
    }

    fn add_event(&self, event: &AuditEvent) -> Result<AuditEvent, StorageError> {
        let connection = self.lock()?;
        in_transaction(&connection, false, AuditEvent::TABLE, |tx| {
            insert(tx, event)
        })?;
        Ok(event.clone())
    }

    fn list_events(
        &self,
        project_id: &str,
        run_id: Option<&str>,
        limit: i64,
        after_id: Option<&str>,
    ) -> Result<Vec<AuditEvent>, StorageError> {
        // Python：limit = max(1, min(limit, 1000))。
        let limit = limit.clamp(1, 1000);
        let connection = self.lock()?;
        let mut conditions = vec![format!("{} = ?", ScopeColumn::ProjectId.as_str())];
        let mut values: Vec<SqlValue> = vec![SqlValue::Text(project_id.to_string())];
        if let Some(run_id) = run_id {
            conditions.push(format!("{} = ?", ScopeColumn::RunId.as_str()));
            values.push(SqlValue::Text(run_id.to_string()));
        }
        if let Some(after_id) = after_id {
            // 以子查询定位 seq；after_id 不存在时子查询为 NULL，seq > NULL
            // 不成立，结果为空——与 Python 逐字节同义。
            conditions.push("seq > (SELECT seq FROM audit_events WHERE id = ?)".to_string());
            values.push(SqlValue::Text(after_id.to_string()));
        }
        let sql = format!(
            "SELECT payload FROM {} WHERE {} ORDER BY seq ASC LIMIT ?",
            AuditEvent::TABLE,
            conditions.join(" AND ")
        );
        values.push(limit.into());
        query_payloads::<AuditEvent>(&connection, &sql, params_from_iter(values))
    }

    fn append_blackboard_entry(
        &self,
        entry: &BlackboardEntry,
    ) -> Result<BlackboardEntry, StorageError> {
        entry
            .validate()
            .map_err(|message| StorageError::InvalidArgument {
                entity: "blackboard entry",
                message,
            })?;
        let connection = self.lock()?;
        in_transaction(&connection, true, AuditEvent::TABLE, |tx| {
            let existing =
                query_blackboard_rows(tx, entry.project_id.as_str(), entry.run_id.as_str(), None)?;
            if let Some(previous) = existing.into_iter().find(|candidate| {
                candidate.mission_id == entry.mission_id
                    && candidate.idempotency_key == entry.idempotency_key
            }) {
                if same_blackboard_request(&previous, entry) {
                    return Ok(previous);
                }
                return Err(StorageError::AppendOnlyConflict {
                    entity: "blackboard idempotency key".to_string(),
                    id: entry.idempotency_key.clone(),
                });
            }

            let mut stored = entry.clone();
            stored.sequence = 0;
            let event = blackboard_event(&stored)?;
            insert(tx, &event).map_err(|error| {
                append_only_conflict(error, AuditEvent::TABLE, event.entity_id())
            })?;
            let sequence = tx
                .query_row(
                    "SELECT seq FROM audit_events WHERE id = ?",
                    params![event.id],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(write_error(AuditEvent::TABLE))?;
            stored.sequence = sequence;
            Ok(stored)
        })
    }

    fn list_blackboard_entries(
        &self,
        project_id: &str,
        mission_id: &str,
        run_id: &str,
        after_sequence: Option<i64>,
        kind: Option<BlackboardEntryKind>,
        limit: usize,
        context_byte_budget: usize,
    ) -> Result<(Vec<BlackboardEntry>, Option<i64>), StorageError> {
        valid_lease_argument("blackboard project_id", project_id)?;
        valid_lease_argument("blackboard mission_id", mission_id)?;
        valid_lease_argument("blackboard run_id", run_id)?;
        if after_sequence.is_some_and(|value| value < 0) {
            return Err(StorageError::InvalidArgument {
                entity: "blackboard cursor",
                message: "must be non-negative".to_string(),
            });
        }
        let limit = limit.clamp(1, 1000);
        let context_byte_budget = context_byte_budget.max(1);
        let connection = self.lock()?;
        let candidates = query_blackboard_rows(&connection, project_id, run_id, after_sequence)?;
        let mut entries = Vec::new();
        let mut bytes = 0usize;
        let mut last_sequence = None;
        let mut stopped = false;
        for candidate in candidates.iter().filter(|candidate| {
            candidate.mission_id.as_str() == mission_id
                && kind.is_none_or(|wanted| candidate.kind == wanted)
        }) {
            if entries.len() >= limit {
                stopped = true;
                break;
            }
            let size = serde_json::to_vec(candidate)
                .map_err(|source| StorageError::Serialize {
                    table: "blackboard_entries".to_string(),
                    source,
                })?
                .len();
            if bytes.saturating_add(size) > context_byte_budget {
                stopped = true;
                break;
            }
            bytes += size;
            last_sequence = Some(candidate.sequence);
            entries.push(candidate.clone());
        }
        let next_cursor = if stopped { last_sequence } else { None };
        Ok((entries, next_cursor))
    }

    fn add_observation(&self, observation: &Observation) -> Result<Observation, StorageError> {
        let connection = self.lock()?;
        in_transaction(&connection, false, Observation::TABLE, |tx| {
            insert(tx, observation)
        })?;
        Ok(observation.clone())
    }

    fn list_observations(
        &self,
        project_id: &str,
        run_id: Option<&str>,
    ) -> Result<Vec<Observation>, StorageError> {
        let connection = self.lock()?;
        list_project_run(&connection, project_id, run_id)
    }

    fn add_coverage_assessment(
        &self,
        assessment: &CoverageAssessment,
    ) -> Result<CoverageAssessment, StorageError> {
        let connection = self.lock()?;
        in_transaction(&connection, false, CoverageAssessment::TABLE, |tx| {
            insert(tx, assessment)
        })?;
        Ok(assessment.clone())
    }

    fn list_coverage_assessments(
        &self,
        project_id: &str,
        run_id: Option<&str>,
    ) -> Result<Vec<CoverageAssessment>, StorageError> {
        let connection = self.lock()?;
        list_project_run(&connection, project_id, run_id)
    }

    fn add_metacognition_assessment(
        &self,
        assessment: &MetacognitionAssessment,
    ) -> Result<MetacognitionAssessment, StorageError> {
        let connection = self.lock()?;
        in_transaction(&connection, false, MetacognitionAssessment::TABLE, |tx| {
            insert(tx, assessment)
        })?;
        Ok(assessment.clone())
    }

    fn list_metacognition_assessments(
        &self,
        project_id: &str,
        run_id: Option<&str>,
    ) -> Result<Vec<MetacognitionAssessment>, StorageError> {
        let connection = self.lock()?;
        list_project_run(&connection, project_id, run_id)
    }

    fn add_exit_gate_decision(
        &self,
        decision: &ExitGateDecision,
    ) -> Result<ExitGateDecision, StorageError> {
        let connection = self.lock()?;
        in_transaction(&connection, false, ExitGateDecision::TABLE, |tx| {
            insert(tx, decision)
        })?;
        Ok(decision.clone())
    }

    fn list_exit_gate_decisions(
        &self,
        project_id: &str,
        run_id: Option<&str>,
    ) -> Result<Vec<ExitGateDecision>, StorageError> {
        let connection = self.lock()?;
        list_project_run(&connection, project_id, run_id)
    }

    fn add_escalation_guard_verdict(
        &self,
        verdict: &EscalationGuardVerdict,
    ) -> Result<EscalationGuardVerdict, StorageError> {
        let connection = self.lock()?;
        in_transaction(&connection, false, EscalationGuardVerdict::TABLE, |tx| {
            insert(tx, verdict)
        })?;
        Ok(verdict.clone())
    }

    fn list_escalation_guard_verdicts(
        &self,
        project_id: &str,
        run_id: Option<&str>,
    ) -> Result<Vec<EscalationGuardVerdict>, StorageError> {
        let connection = self.lock()?;
        list_project_run(&connection, project_id, run_id)
    }

    fn add_critique_report(&self, report: &CritiqueReport) -> Result<CritiqueReport, StorageError> {
        let connection = self.lock()?;
        in_transaction(&connection, false, CritiqueReport::TABLE, |tx| {
            insert(tx, report)
        })?;
        Ok(report.clone())
    }

    fn get_critique_report(&self, report_id: &str) -> Result<Option<CritiqueReport>, StorageError> {
        let connection = self.lock()?;
        get::<CritiqueReport>(&connection, report_id)
    }

    fn list_critique_reports(
        &self,
        project_id: &str,
        run_id: Option<&str>,
        branch_id: Option<&str>,
    ) -> Result<Vec<CritiqueReport>, StorageError> {
        let connection = self.lock()?;
        let mut reports = list_project_run::<CritiqueReport>(&connection, project_id, run_id)?;
        if let Some(branch_id) = branch_id {
            reports.retain(|report| report.branch_id.as_str() == branch_id);
        }
        Ok(reports)
    }

    fn add_trajectory_summary(
        &self,
        summary: &TrajectorySummary,
    ) -> Result<TrajectorySummary, StorageError> {
        let connection = self.lock()?;
        in_transaction(&connection, false, TrajectorySummary::TABLE, |tx| {
            insert(tx, summary)
        })?;
        Ok(summary.clone())
    }

    fn get_trajectory_summary(
        &self,
        summary_id: &str,
    ) -> Result<Option<TrajectorySummary>, StorageError> {
        let connection = self.lock()?;
        get::<TrajectorySummary>(&connection, summary_id)
    }

    fn list_trajectory_summaries(
        &self,
        project_id: &str,
        run_id: Option<&str>,
        branch_id: Option<&str>,
    ) -> Result<Vec<TrajectorySummary>, StorageError> {
        let connection = self.lock()?;
        let mut summaries = list_project_run::<TrajectorySummary>(&connection, project_id, run_id)?;
        if let Some(branch_id) = branch_id {
            summaries.retain(|summary| {
                summary.branch_id.as_ref().map(BranchId::as_str) == Some(branch_id)
            });
        }
        Ok(summaries)
    }

    fn add_agent_narrative_event(
        &self,
        event: &AgentNarrativeEvent,
    ) -> Result<AgentNarrativeEvent, StorageError> {
        let connection = self.lock()?;
        in_transaction(&connection, false, AgentNarrativeEvent::TABLE, |tx| {
            insert(tx, event)
        })?;
        Ok(event.clone())
    }

    fn list_agent_narrative_events(
        &self,
        project_id: &str,
        run_id: Option<&str>,
        mission_id: Option<&str>,
        branch_id: Option<&str>,
        limit: Option<usize>,
    ) -> Result<Vec<AgentNarrativeEvent>, StorageError> {
        let connection = self.lock()?;
        // 条件拼接镜像 Python `_list_filtered`：project_id / run_id 进 SQL，
        // mission_id / branch_id 是 payload 字段，查询后内存过滤。
        let mut events = list_project_run::<AgentNarrativeEvent>(&connection, project_id, run_id)?;
        if let Some(mission_id) = mission_id {
            events.retain(|event| {
                event.mission_id.as_ref().map(MissionId::as_str) == Some(mission_id)
            });
        }
        if let Some(branch_id) = branch_id {
            events
                .retain(|event| event.branch_id.as_ref().map(BranchId::as_str) == Some(branch_id));
        }
        if let Some(limit) = limit {
            events.truncate(limit);
        }
        Ok(events)
    }

    fn add_strategy_board_snapshot(
        &self,
        snapshot: &StrategyBoardSnapshot,
    ) -> Result<StrategyBoardSnapshot, StorageError> {
        let connection = self.lock()?;
        in_transaction(&connection, false, StrategyBoardSnapshot::TABLE, |tx| {
            insert(tx, snapshot)
        })?;
        Ok(snapshot.clone())
    }

    fn get_strategy_board_snapshot(
        &self,
        snapshot_id: &str,
    ) -> Result<Option<StrategyBoardSnapshot>, StorageError> {
        let connection = self.lock()?;
        get::<StrategyBoardSnapshot>(&connection, snapshot_id)
    }

    fn list_strategy_board_snapshots(
        &self,
        project_id: &str,
        run_id: Option<&str>,
    ) -> Result<Vec<StrategyBoardSnapshot>, StorageError> {
        let connection = self.lock()?;
        list_project_run(&connection, project_id, run_id)
    }

    fn create_module(&self, module: &ModuleConfig) -> Result<ModuleConfig, StorageError> {
        let connection = self.lock()?;
        in_transaction(&connection, false, ModuleConfig::TABLE, |tx| {
            insert(tx, module)
        })?;
        Ok(module.clone())
    }

    fn get_module(&self, module_id: &str) -> Result<Option<ModuleConfig>, StorageError> {
        let connection = self.lock()?;
        get::<ModuleConfig>(&connection, module_id)
    }

    fn list_modules(&self) -> Result<Vec<ModuleConfig>, StorageError> {
        let connection = self.lock()?;
        list_all(&connection)
    }

    fn update_module(&self, module: &ModuleConfig) -> Result<ModuleConfig, StorageError> {
        let connection = self.lock()?;
        in_transaction(&connection, false, ModuleConfig::TABLE, |tx| {
            upsert(tx, module)
        })?;
        Ok(module.clone())
    }

    fn delete_module(&self, module_id: &str) -> Result<(), StorageError> {
        let connection = self.lock()?;
        in_transaction(&connection, false, ModuleConfig::TABLE, |tx| {
            tx.execute("DELETE FROM modules WHERE id = ?", params![module_id])
                .map_err(write_error(ModuleConfig::TABLE))?;
            Ok(())
        })
    }

    fn list_enabled_modules(&self) -> Result<Vec<ModuleConfig>, StorageError> {
        let connection = self.lock()?;
        Ok(list_all::<ModuleConfig>(&connection)?
            .into_iter()
            .filter(|module| module.enabled)
            .collect())
    }

    fn add_hint(&self, hint: &Hint) -> Result<Hint, StorageError> {
        let connection = self.lock()?;
        in_transaction(&connection, false, Hint::TABLE, |tx| insert(tx, hint))
            .map_err(|error| append_only_conflict(error, Hint::TABLE, hint.entity_id()))?;
        Ok(hint.clone())
    }

    fn list_hints(&self, project_id: &str) -> Result<Vec<Hint>, StorageError> {
        let connection = self.lock()?;
        list_where(&connection, ScopeColumn::ProjectId, project_id)
    }

    fn add_knowledge_card(&self, card: &KnowledgeCard) -> Result<KnowledgeCard, StorageError> {
        let connection = self.lock()?;
        in_transaction(&connection, false, KnowledgeCard::TABLE, |tx| {
            upsert(tx, card)?;
            // FTS 索引随写维护：写入与索引在同一事务，杜绝静默过期。
            crate::knowledge_fts::upsert_card(tx, card, AliasRegistry::embedded())
        })?;
        Ok(card.clone())
    }

    fn list_knowledge_cards(&self) -> Result<Vec<KnowledgeCard>, StorageError> {
        let connection = self.lock()?;
        list_all(&connection)
    }

    fn search_knowledge_cards(
        &self,
        query: &KnowledgeRetrievalQuery,
    ) -> Result<Vec<KnowledgeRetrievalResult>, StorageError> {
        let connection = self.lock()?;
        let registry = AliasRegistry::embedded();
        let fetch_limit = query.limit.saturating_mul(4).clamp(20, 200);
        let hits = crate::knowledge_fts::search(
            &connection,
            &query.text,
            usize::try_from(fetch_limit).unwrap_or(200),
            registry,
        );
        match hits {
            Ok(hits) if !hits.is_empty() => {
                let cards = crate::knowledge_fts::load_cards_by_ids(&connection, &hits)?;
                Ok(crate::knowledge_fts::assemble_results(
                    &hits, &cards, query, registry,
                ))
            }
            Ok(_) if query.text.trim().is_empty() => {
                // 纯过滤查询（无文本词项）：FTS 不参与，确定性 priority 排序。
                let cards: Vec<KnowledgeCard> = list_all(&connection)?;
                Ok(crate::knowledge_fts::filter_only_results(&cards, query))
            }
            Ok(_) => Ok(Vec::new()),
            Err(_fts_error) => {
                // 检索设施不可用 ≠ 检索失败：回退确定性线性扫描并显式
                // 注明（§41 no hit / unavailable 必须可区分）。
                let cards: Vec<KnowledgeCard> = list_all(&connection)?;
                let mut results = crate::knowledge_search::search_cards(&cards, query);
                for result in &mut results {
                    result.retrieval_reason = Some("fallback_linear_scan".to_string());
                }
                Ok(results)
            }
        }
    }

    fn sync_knowledge_index(&self) -> Result<KnowledgeCorpusStatus, StorageError> {
        let connection = self.lock()?;
        let cards: Vec<KnowledgeCard> = list_all(&connection)?;
        let indexed =
            crate::knowledge_fts::rebuild(&connection, &cards, AliasRegistry::embedded())?;
        let _ = indexed;
        crate::knowledge_fts::corpus_status(&connection)
    }

    fn knowledge_corpus_status(&self) -> Result<KnowledgeCorpusStatus, StorageError> {
        let connection = self.lock()?;
        crate::knowledge_fts::corpus_status(&connection)
    }

    fn add_decision_gate(&self, gate: &DecisionGate) -> Result<DecisionGate, StorageError> {
        let connection = self.lock()?;
        in_transaction(&connection, false, DecisionGate::TABLE, |tx| {
            insert(tx, gate)
        })?;
        Ok(gate.clone())
    }

    fn get_decision_gate(&self, gate_id: &str) -> Result<Option<DecisionGate>, StorageError> {
        let connection = self.lock()?;
        get::<DecisionGate>(&connection, gate_id)
    }

    fn list_decision_gates(
        &self,
        project_id: Option<&str>,
        audit_run_id: Option<&str>,
    ) -> Result<Vec<DecisionGate>, StorageError> {
        // Python：conditions 按参数顺序动态拼接（project_id → run_id）。
        let connection = self.lock()?;
        match (project_id, audit_run_id) {
            (None, None) => list_all(&connection),
            (Some(project_id), None) => list_where(&connection, ScopeColumn::ProjectId, project_id),
            (None, Some(run_id)) => list_where(&connection, ScopeColumn::RunId, run_id),
            (Some(project_id), Some(run_id)) => {
                let sql = format!(
                    "SELECT payload FROM {} WHERE project_id = ? AND run_id = ? ORDER BY seq ASC",
                    DecisionGate::TABLE
                );
                query_payloads::<DecisionGate>(&connection, &sql, params![project_id, run_id])
            }
        }
    }

    fn update_decision_gate(&self, gate: &DecisionGate) -> Result<DecisionGate, StorageError> {
        let connection = self.lock()?;
        in_transaction(&connection, false, DecisionGate::TABLE, |tx| {
            upsert(tx, gate)
        })?;
        Ok(gate.clone())
    }

    fn answer_decision_gate(
        &self,
        gate_id: &str,
        answer: &DecisionAnswer,
    ) -> Result<DecisionGate, StorageError> {
        let connection = self.lock()?;
        let mut gate: DecisionGate =
            get::<DecisionGate>(&connection, gate_id)?.ok_or(StorageError::NotFound {
                entity: "decision gate",
                id: gate_id.to_string(),
            })?;
        gate.answer = Some(answer.clone());
        gate.status = DecisionGateStatus::Answered;
        gate.answered_at = Some(utcnow());
        in_transaction(&connection, false, DecisionGate::TABLE, |tx| {
            upsert(tx, &gate)
        })?;
        Ok(gate)
    }

    fn add_worker_profile(&self, profile: &WorkerProfile) -> Result<WorkerProfile, StorageError> {
        let connection = self.lock()?;
        in_transaction(&connection, false, WorkerProfile::TABLE, |tx| {
            upsert(tx, profile)
        })?;
        Ok(profile.clone())
    }

    fn list_worker_profiles(&self) -> Result<Vec<WorkerProfile>, StorageError> {
        let connection = self.lock()?;
        list_all(&connection)
    }

    fn add_worker_lease(&self, lease: &WorkerLease) -> Result<WorkerLease, StorageError> {
        let connection = self.lock()?;
        in_transaction(&connection, false, WorkerLease::TABLE, |tx| {
            insert(tx, lease)
        })
        .map_err(|error| append_only_conflict(error, WorkerLease::TABLE, lease.entity_id()))?;
        Ok(lease.clone())
    }

    fn update_worker_lease(&self, lease: &WorkerLease) -> Result<WorkerLease, StorageError> {
        let connection = self.lock()?;
        in_transaction(&connection, false, WorkerLease::TABLE, |tx| {
            upsert(tx, lease)
        })?;
        Ok(lease.clone())
    }

    fn list_worker_leases(
        &self,
        project_id: &str,
        run_id: Option<&str>,
    ) -> Result<Vec<WorkerLease>, StorageError> {
        let connection = self.lock()?;
        list_project_run(&connection, project_id, run_id)
    }

    fn claim_worker_lease(
        &self,
        project_id: &str,
        mission_id: &str,
        run_id: &str,
        task_id: &str,
        worker_id: &str,
        worker_run_id: &str,
        lease_seconds: i64,
    ) -> Result<Option<WorkerLease>, StorageError> {
        for (entity, value) in [
            ("lease project_id", project_id),
            ("lease mission_id", mission_id),
            ("lease run_id", run_id),
            ("lease task_id", task_id),
            ("lease worker_id", worker_id),
            ("lease worker_run_id", worker_run_id),
        ] {
            valid_lease_argument(entity, value)?;
        }
        if !(1..=3600).contains(&lease_seconds) {
            return Err(StorageError::InvalidArgument {
                entity: "lease duration",
                message: "must be between 1 and 3600 seconds".to_string(),
            });
        }
        let connection = self.lock()?;
        in_transaction(&connection, true, WorkerLease::TABLE, |tx| {
            let Some(mut task) = get::<AgentTask>(tx, task_id)? else {
                return Ok(None);
            };
            if task.project_id.as_str() != project_id
                || task.run_id.as_str() != run_id
                || task.mission_id.as_ref().map(|value| value.as_str()) != Some(mission_id)
                || matches!(
                    task.status,
                    TaskStatus::Succeeded | TaskStatus::Failed | TaskStatus::Cancelled
                )
            {
                return Ok(None);
            }
            expire_leases_in_scope(tx, project_id, Some(mission_id), Some(run_id))?;
            let now = utcnow();
            let active = list_project_run::<WorkerLease>(tx, project_id, Some(run_id))?
                .into_iter()
                .any(|lease| {
                    lease.status == WorkerLeaseStatus::Active
                        && lease.mission_id.as_ref().map(|value| value.as_str()) == Some(mission_id)
                        && lease.task_id.as_ref().map(|value| value.as_str()) == Some(task_id)
                        && lease.lease_expires_at > now
                });
            if active {
                return Ok(None);
            }
            let expires = now + chrono::Duration::seconds(lease_seconds);
            let intent_id = task
                .intent_id
                .clone()
                .unwrap_or_else(|| format!("lease-intent-{task_id}"));
            let mut lease = WorkerLease::new(
                ProjectId::new(project_id.to_string()),
                RunId::new(run_id.to_string()),
                models::IntentId::new(intent_id),
                worker_id.to_string(),
                expires,
            );
            lease.mission_id = Some(models::MissionId::new(mission_id.to_string()));
            lease.worker_run_id = Some(worker_run_id.to_string());
            lease.task_id = Some(models::TaskId::new(task_id.to_string()));
            lease.acquired_at = now;
            lease.lease_expires_at = expires;
            lease.heartbeat_at = now;
            lease.created_at = now;
            lease.updated_at = now;
            lease.revision = 1;
            lease.leased_at = Some(now);
            lease.expires_at = Some(expires);
            task.status = TaskStatus::Running;
            task.started_at.get_or_insert(now);
            upsert(tx, &task)?;
            insert(tx, &lease).map_err(|error| {
                append_only_conflict(error, WorkerLease::TABLE, lease.entity_id())
            })?;
            Ok(Some(lease))
        })
    }

    fn heartbeat_worker_lease(
        &self,
        lease_id: &str,
        worker_run_id: &str,
        revision: i64,
        lease_seconds: i64,
    ) -> Result<Option<WorkerLease>, StorageError> {
        valid_lease_argument("lease_id", lease_id)?;
        valid_lease_argument("lease worker_run_id", worker_run_id)?;
        if !(1..=3600).contains(&lease_seconds) || revision < 0 {
            return Err(StorageError::InvalidArgument {
                entity: "lease heartbeat",
                message: "revision must be non-negative and duration 1..3600 seconds".to_string(),
            });
        }
        let connection = self.lock()?;
        in_transaction(&connection, true, WorkerLease::TABLE, |tx| {
            let Some(mut lease) = get::<WorkerLease>(tx, lease_id)? else {
                return Ok(None);
            };
            let now = utcnow();
            if lease.status != WorkerLeaseStatus::Active
                || lease.worker_run_id.as_deref() != Some(worker_run_id)
                || lease.revision != revision
            {
                return Ok(None);
            }
            if lease.lease_expires_at <= now {
                lease.status = WorkerLeaseStatus::Expired;
                lease.updated_at = now;
                lease.heartbeat_at = now;
                lease.revision = lease.revision.saturating_add(1);
                upsert(tx, &lease)?;
                return Ok(None);
            }
            let expires = now + chrono::Duration::seconds(lease_seconds);
            lease.lease_expires_at = expires;
            lease.expires_at = Some(expires);
            lease.heartbeat_at = now;
            lease.updated_at = now;
            lease.revision = lease.revision.saturating_add(1);
            upsert(tx, &lease)?;
            Ok(Some(lease))
        })
    }

    fn complete_worker_lease(
        &self,
        lease_id: &str,
        worker_run_id: &str,
        revision: i64,
    ) -> Result<Option<WorkerLease>, StorageError> {
        let connection = self.lock()?;
        in_transaction(&connection, true, WorkerLease::TABLE, |tx| {
            transition_lease(
                tx,
                lease_id,
                worker_run_id,
                revision,
                WorkerLeaseStatus::Completed,
            )
        })
    }

    fn fail_worker_lease(
        &self,
        lease_id: &str,
        worker_run_id: &str,
        revision: i64,
    ) -> Result<Option<WorkerLease>, StorageError> {
        let connection = self.lock()?;
        in_transaction(&connection, true, WorkerLease::TABLE, |tx| {
            transition_lease(
                tx,
                lease_id,
                worker_run_id,
                revision,
                WorkerLeaseStatus::Failed,
            )
        })
    }

    fn cancel_worker_lease(
        &self,
        lease_id: &str,
        worker_run_id: &str,
        revision: i64,
    ) -> Result<Option<WorkerLease>, StorageError> {
        let connection = self.lock()?;
        in_transaction(&connection, true, WorkerLease::TABLE, |tx| {
            transition_lease(
                tx,
                lease_id,
                worker_run_id,
                revision,
                WorkerLeaseStatus::Cancelled,
            )
        })
    }

    fn reclaim_expired_worker_leases(
        &self,
        project_id: &str,
        mission_id: Option<&str>,
        run_id: Option<&str>,
    ) -> Result<usize, StorageError> {
        valid_lease_argument("lease project_id", project_id)?;
        let connection = self.lock()?;
        in_transaction(&connection, true, WorkerLease::TABLE, |tx| {
            expire_leases_in_scope(tx, project_id, mission_id, run_id)
        })
    }

    fn add_termination_assessment(
        &self,
        assessment: &TerminationAssessment,
    ) -> Result<TerminationAssessment, StorageError> {
        let connection = self.lock()?;
        in_transaction(&connection, false, TerminationAssessment::TABLE, |tx| {
            insert(tx, assessment)
        })?;
        Ok(assessment.clone())
    }

    fn list_termination_assessments(
        &self,
        project_id: &str,
        run_id: Option<&str>,
    ) -> Result<Vec<TerminationAssessment>, StorageError> {
        let connection = self.lock()?;
        list_project_run(&connection, project_id, run_id)
    }

    fn add_context_pack(&self, pack: &ContextPack) -> Result<ContextPack, StorageError> {
        let connection = self.lock()?;
        in_transaction(&connection, false, ContextPack::TABLE, |tx| {
            insert(tx, pack)
        })
        .map_err(|error| append_only_conflict(error, ContextPack::TABLE, pack.entity_id()))?;
        Ok(pack.clone())
    }

    fn get_context_pack(&self, pack_id: &str) -> Result<Option<ContextPack>, StorageError> {
        let connection = self.lock()?;
        get::<ContextPack>(&connection, pack_id)
    }

    fn list_context_packs(
        &self,
        project_id: &str,
        run_id: Option<&str>,
    ) -> Result<Vec<ContextPack>, StorageError> {
        let connection = self.lock()?;
        list_project_run(&connection, project_id, run_id)
    }

    fn add_context_compression_report(
        &self,
        report: &ContextCompressionReport,
    ) -> Result<ContextCompressionReport, StorageError> {
        let connection = self.lock()?;
        in_transaction(&connection, false, ContextCompressionReport::TABLE, |tx| {
            insert(tx, report)
        })?;
        Ok(report.clone())
    }

    fn list_context_compression_reports(
        &self,
        project_id: &str,
        run_id: Option<&str>,
    ) -> Result<Vec<ContextCompressionReport>, StorageError> {
        let connection = self.lock()?;
        list_project_run(&connection, project_id, run_id)
    }

    fn add_retrieval_invocation(
        &self,
        invocation: &RetrievalInvocation,
    ) -> Result<RetrievalInvocation, StorageError> {
        let connection = self.lock()?;
        in_transaction(&connection, false, RetrievalInvocation::TABLE, |tx| {
            insert(tx, invocation)
        })
        .map_err(|error| {
            append_only_conflict(error, RetrievalInvocation::TABLE, invocation.entity_id())
        })?;
        Ok(invocation.clone())
    }

    fn upsert_runtime_setting(
        &self,
        setting: &RuntimeSetting,
    ) -> Result<RuntimeSetting, StorageError> {
        let key = setting.key.trim().to_lowercase();
        if key.is_empty() {
            return Err(StorageError::Write {
                table: RuntimeSetting::TABLE.to_string(),
                source: rusqlite::Error::InvalidParameterName(
                    "setting key must not be blank".to_string(),
                ),
            });
        }
        let existing = {
            let connection = self.lock()?;
            list_all::<RuntimeSetting>(&connection)?
                .into_iter()
                .filter(|item| {
                    item.key == key
                        && item.project_id == setting.project_id
                        && item.run_id == setting.run_id
                })
                .collect::<Vec<_>>()
        };
        let connection = self.lock()?;
        in_transaction(&connection, false, RuntimeSetting::TABLE, |tx| {
            for item in &existing {
                tx.execute(
                    "DELETE FROM runtime_settings WHERE id = ?",
                    params![item.id.as_str()],
                )
                .map_err(write_error(RuntimeSetting::TABLE))?;
            }
            upsert(tx, setting)
        })?;
        Ok(setting.clone())
    }

    fn get_runtime_setting(
        &self,
        key: &str,
        project_id: Option<&str>,
        run_id: Option<&str>,
    ) -> Result<Option<RuntimeSetting>, StorageError> {
        let normalized = key.trim().to_lowercase();
        let connection = self.lock()?;
        let mut candidates = list_all::<RuntimeSetting>(&connection)?
            .into_iter()
            .filter(|item| {
                item.key == normalized
                    && ((run_id.is_some() && item.run_id.as_ref().map(RunId::as_str) == run_id)
                        || (project_id.is_some()
                            && item.run_id.is_none()
                            && item.project_id.as_ref().map(ProjectId::as_str) == project_id)
                        || (item.project_id.is_none() && item.run_id.is_none()))
            })
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| {
            let rank = |item: &RuntimeSetting| {
                if item.run_id.is_some() {
                    2
                } else if item.project_id.is_some() {
                    i32::from(item.project_id.is_some())
                } else {
                    0
                }
            };
            rank(left)
                .cmp(&rank(right))
                .then_with(|| left.updated_at.cmp(&right.updated_at))
        });
        Ok(candidates.pop())
    }

    fn list_runtime_settings(
        &self,
        project_id: Option<&str>,
        run_id: Option<&str>,
    ) -> Result<Vec<RuntimeSetting>, StorageError> {
        let connection = self.lock()?;
        Ok(list_all::<RuntimeSetting>(&connection)?
            .into_iter()
            .filter(|item| {
                project_id.is_none_or(|id| {
                    item.project_id
                        .as_ref()
                        .is_none_or(|item_id| item_id.as_str() == id)
                }) && run_id.is_none_or(|id| {
                    item.run_id
                        .as_ref()
                        .is_none_or(|item_id| item_id.as_str() == id)
                })
            })
            .collect())
    }

    fn delete_runtime_setting(
        &self,
        key: &str,
        project_id: Option<&str>,
        run_id: Option<&str>,
    ) -> Result<(), StorageError> {
        let normalized = key.trim().to_lowercase();
        let connection = self.lock()?;
        in_transaction(&connection, false, RuntimeSetting::TABLE, |tx| {
            let settings = list_all::<RuntimeSetting>(tx)?;
            for item in settings {
                if item.key == normalized
                    && item.project_id.as_ref().map(ProjectId::as_str) == project_id
                    && item.run_id.as_ref().map(RunId::as_str) == run_id
                {
                    tx.execute(
                        "DELETE FROM runtime_settings WHERE id = ?",
                        params![item.id.as_str()],
                    )
                    .map_err(write_error(RuntimeSetting::TABLE))?;
                }
            }
            Ok(())
        })
    }

    fn add_artifact_record(
        &self,
        artifact: &ArtifactRecord,
    ) -> Result<ArtifactRecord, StorageError> {
        let connection = self.lock()?;
        in_transaction(&connection, false, ArtifactRecord::TABLE, |tx| {
            insert(tx, artifact)
        })
        .map_err(|error| {
            append_only_conflict(error, ArtifactRecord::TABLE, artifact.entity_id())
        })?;
        Ok(artifact.clone())
    }

    fn get_artifact_record(
        &self,
        artifact_id: &str,
    ) -> Result<Option<ArtifactRecord>, StorageError> {
        let connection = self.lock()?;
        get::<ArtifactRecord>(&connection, artifact_id)
    }

    fn update_artifact_record(
        &self,
        artifact: &ArtifactRecord,
    ) -> Result<ArtifactRecord, StorageError> {
        let connection = self.lock()?;
        in_transaction(&connection, false, ArtifactRecord::TABLE, |tx| {
            upsert(tx, artifact)
        })?;
        Ok(artifact.clone())
    }

    fn list_artifact_records(
        &self,
        project_id: Option<&str>,
        run_id: Option<&str>,
    ) -> Result<Vec<ArtifactRecord>, StorageError> {
        // Python `_list_project_run`：project_id=None 列全表；给定 project 时
        // run_id=None 列整个 Project，否则 SQL 双列过滤。仅当 run_id 给定而
        // project_id 缺失时，查询后内存过滤。
        let connection = self.lock()?;
        let artifacts: Vec<ArtifactRecord> = match project_id {
            None => list_all(&connection)?,
            Some(project_id) => list_project_run(&connection, project_id, run_id)?,
        };
        if run_id.is_some() && project_id.is_none() {
            let run_id = run_id.unwrap_or_default();
            return Ok(artifacts
                .into_iter()
                .filter(|item| item.run_id.as_ref().is_some_and(|id| id.as_str() == run_id))
                .collect());
        }
        Ok(artifacts)
    }

    fn add_user_directive(&self, directive: &UserDirective) -> Result<UserDirective, StorageError> {
        let connection = self.lock()?;
        in_transaction(&connection, false, UserDirective::TABLE, |tx| {
            insert(tx, directive)
        })?;
        Ok(directive.clone())
    }

    fn get_user_directive(
        &self,
        directive_id: &str,
    ) -> Result<Option<UserDirective>, StorageError> {
        let connection = self.lock()?;
        get::<UserDirective>(&connection, directive_id)
    }

    fn list_user_directives(
        &self,
        project_id: Option<&str>,
        mission_id: Option<&str>,
        run_id: Option<&str>,
        branch_id: Option<&str>,
    ) -> Result<Vec<UserDirective>, StorageError> {
        let connection = self.lock()?;
        let mut conditions: Vec<String> = Vec::new();
        let mut values: Vec<SqlValue> = Vec::new();
        if let Some(project_id) = project_id {
            conditions.push(format!("{} = ?", ScopeColumn::ProjectId.as_str()));
            values.push(SqlValue::Text(project_id.to_string()));
        }
        if let Some(run_id) = run_id {
            conditions.push(format!("{} = ?", ScopeColumn::RunId.as_str()));
            values.push(SqlValue::Text(run_id.to_string()));
        }
        let where_clause = if conditions.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", conditions.join(" AND "))
        };
        let sql = format!(
            "SELECT payload FROM {}{} ORDER BY seq ASC",
            UserDirective::TABLE,
            where_clause
        );
        let mut directives =
            query_payloads::<UserDirective>(&connection, &sql, params_from_iter(values))?;
        if let Some(mission_id) = mission_id {
            directives.retain(|directive| directive.mission_id.as_str() == mission_id);
        }
        if let Some(branch_id) = branch_id {
            directives.retain(|directive| {
                directive
                    .branch_id
                    .as_ref()
                    .is_some_and(|id| id.as_str() == branch_id)
            });
        }
        Ok(directives)
    }

    fn update_user_directive(
        &self,
        directive: &UserDirective,
    ) -> Result<UserDirective, StorageError> {
        let connection = self.lock()?;
        in_transaction(&connection, false, UserDirective::TABLE, |tx| {
            upsert(tx, directive)
        })?;
        Ok(directive.clone())
    }

    fn create_mission_asset(&self, asset: &MissionAsset) -> Result<MissionAsset, StorageError> {
        let connection = self.lock()?;
        in_transaction(&connection, false, "mission_assets", |tx| {
            insert_mission_asset(tx, asset, false)
        })?;
        Ok(asset.clone())
    }

    fn get_mission_asset(&self, asset_id: &str) -> Result<Option<MissionAsset>, StorageError> {
        let connection = self.lock()?;
        let sql = "SELECT payload FROM mission_assets WHERE id = ?";
        match connection.query_row(sql, params![asset_id], |row| row.get::<_, String>(0)) {
            Ok(payload) => decode_mission_asset(&payload).map(Some),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(source) => Err(StorageError::Query {
                table: "mission_assets".to_string(),
                source,
            }),
        }
    }

    fn list_mission_assets(
        &self,
        mission_id: Option<&str>,
        project_id: Option<&str>,
        sensitivity: Option<MissionAssetSensitivity>,
        asset_type: Option<MissionAssetType>,
    ) -> Result<Vec<MissionAsset>, StorageError> {
        let connection = self.lock()?;
        let mut conditions: Vec<String> = Vec::new();
        let mut values: Vec<SqlValue> = Vec::new();
        if let Some(mission_id) = mission_id {
            conditions.push("mission_id = ?".to_string());
            values.push(SqlValue::Text(mission_id.to_string()));
        }
        if let Some(project_id) = project_id {
            conditions.push(format!("{} = ?", ScopeColumn::ProjectId.as_str()));
            values.push(SqlValue::Text(project_id.to_string()));
        }
        if let Some(asset_type) = asset_type {
            conditions.push("asset_type = ?".to_string());
            values.push(SqlValue::Text(asset_type.as_str().to_string()));
        }
        let where_clause = if conditions.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", conditions.join(" AND "))
        };
        let sql = format!("SELECT payload FROM mission_assets{where_clause} ORDER BY seq ASC");
        let mut assets = query_payloads_mission_asset(&connection, &sql, values)?;
        if let Some(sensitivity) = sensitivity {
            assets.retain(|asset| asset.sensitivity == sensitivity);
        }
        Ok(assets)
    }

    fn update_mission_asset(&self, asset: &MissionAsset) -> Result<MissionAsset, StorageError> {
        let connection = self.lock()?;
        in_transaction(&connection, false, "mission_assets", |tx| {
            insert_mission_asset(tx, asset, true)
        })?;
        Ok(asset.clone())
    }

    fn upsert_mission_asset(&self, asset: &MissionAsset) -> Result<MissionAsset, StorageError> {
        let connection = self.lock()?;
        in_transaction(&connection, false, "mission_assets", |tx| {
            let normalized = models::normalize_mission_asset_value(asset.asset_type, &asset.value);
            let sql = "SELECT payload FROM mission_assets \
                       WHERE mission_id = ? AND asset_type = ? AND normalized_value = ?";
            let existing = match tx.query_row(
                sql,
                params![
                    asset.mission_id.as_str(),
                    asset.asset_type.as_str(),
                    normalized
                ],
                |row| row.get::<_, String>(0),
            ) {
                Ok(payload) => Some(decode_mission_asset(&payload)?),
                Err(rusqlite::Error::QueryReturnedNoRows) => None,
                Err(source) => {
                    return Err(StorageError::Query {
                        table: "mission_assets".to_string(),
                        source,
                    });
                }
            };
            if let Some(existing) = existing {
                let merged = models::merge_mission_assets(&existing, asset);
                insert_mission_asset(tx, &merged, true)?;
                Ok(merged)
            } else {
                insert_mission_asset(tx, asset, true)?;
                Ok(asset.clone())
            }
        })
    }

    fn add_reflector_report(
        &self,
        report: &ReflectorReport,
    ) -> Result<ReflectorReport, StorageError> {
        let connection = self.lock()?;
        in_transaction(&connection, false, ReflectorReport::TABLE, |tx| {
            insert(tx, report)
        })?;
        Ok(report.clone())
    }

    fn list_reflector_reports(
        &self,
        project_id: &str,
        run_id: Option<&str>,
    ) -> Result<Vec<ReflectorReport>, StorageError> {
        let connection = self.lock()?;
        list_project_run::<ReflectorReport>(&connection, project_id, run_id)
    }

    // ------------------------------------------------------------------
    // Intelligence Hub
    // ------------------------------------------------------------------

    fn ingest_intel_batch(
        &self,
        batch: &IntelIngestBatch,
    ) -> Result<IntelIngestOutcome, StorageError> {
        let connection = self.lock()?;
        in_transaction(&connection, true, "intelligence", |tx| {
            let raw_sources: HashMap<&str, &str> = batch
                .records
                .iter()
                .map(|record| (record.id.as_str(), record.source.as_str()))
                .collect();

            // Provenance root first. Any later failure rolls these rows back.
            for record in &batch.records {
                write_intel_raw_record(tx, record)?;
            }

            let mut entity_ids = HashSet::new();
            for observation in &batch.entities {
                let source = raw_sources
                    .get(observation.raw_record_id.as_str())
                    .ok_or_else(|| StorageError::NotFound {
                        entity: "intel raw record in ingest batch",
                        id: observation.raw_record_id.clone(),
                    })?;
                let mut entity = observation.entity.clone();
                entity.hit_sources = vec![(*source).to_string()];
                entity.source_count = 1;
                let stored = upsert_intel_entity_tx(tx, &entity)?;
                tx.execute(
                    "INSERT OR IGNORE INTO intel_entity_sources (entity_id, raw_record_id) \
                     VALUES (?, ?)",
                    params![stored.id, observation.raw_record_id],
                )
                .map_err(write_error("intel_entity_sources"))?;
                entity_ids.insert(stored.id);
            }

            let mut relation_ids = HashSet::new();
            for observation in &batch.relations {
                let source = raw_sources
                    .get(observation.raw_record_id.as_str())
                    .ok_or_else(|| StorageError::NotFound {
                        entity: "intel raw record in ingest batch",
                        id: observation.raw_record_id.clone(),
                    })?;
                let from = query_intel_entity_by_key(
                    tx,
                    observation.from_kind,
                    &observation.from_normalized_value,
                )?
                .ok_or_else(|| StorageError::NotFound {
                    entity: "intel relation source entity",
                    id: observation.from_normalized_value.clone(),
                })?;
                let to = query_intel_entity_by_key(
                    tx,
                    observation.to_kind,
                    &observation.to_normalized_value,
                )?
                .ok_or_else(|| StorageError::NotFound {
                    entity: "intel relation target entity",
                    id: observation.to_normalized_value.clone(),
                })?;
                let mut relation = IntelRelation::new(
                    from.id,
                    observation.relation,
                    to.id,
                    observation.confidence,
                );
                relation.hit_sources = vec![(*source).to_string()];
                relation.source_count = 1;
                let stored = upsert_intel_relation_tx(tx, &relation)?;
                tx.execute(
                    "INSERT OR IGNORE INTO intel_relation_sources (relation_id, raw_record_id) \
                     VALUES (?, ?)",
                    params![stored.id, observation.raw_record_id],
                )
                .map_err(write_error("intel_relation_sources"))?;
                relation_ids.insert(stored.id);
            }

            let entities = entity_ids
                .into_iter()
                .map(|id| read_intel_entity_record(tx, &id))
                .collect::<Result<Vec<_>, _>>()?;
            let relations = relation_ids
                .into_iter()
                .map(|id| read_intel_relation_record(tx, &id))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(IntelIngestOutcome {
                entities,
                relations,
            })
        })
    }

    fn get_intel_entity_by_key(
        &self,
        kind: IntelEntityKind,
        normalized_value: &str,
    ) -> Result<Option<IntelEntity>, StorageError> {
        let connection = self.lock()?;
        query_intel_entity_by_key(&connection, kind, normalized_value)
    }

    fn get_intel_entity(&self, entity_id: &str) -> Result<Option<IntelEntity>, StorageError> {
        let connection = self.lock()?;
        match connection.query_row(
            "SELECT payload FROM intel_entities WHERE id = ?",
            params![entity_id],
            |row| row.get::<_, String>(0),
        ) {
            Ok(payload) => serde_json::from_str::<IntelEntity>(&payload)
                .map(Some)
                .map_err(|source| StorageError::Serialize {
                    table: "intel_entities".to_string(),
                    source,
                }),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(source) => Err(StorageError::Query {
                table: "intel_entities".to_string(),
                source,
            }),
        }
    }

    fn set_intel_entity_status(
        &self,
        entity_id: &str,
        status: IntelEntityStatus,
    ) -> Result<Option<IntelEntity>, StorageError> {
        let connection = self.lock()?;
        in_transaction(&connection, false, "intel_entities", |tx| {
            let Some(mut entity) = (match tx.query_row(
                "SELECT payload FROM intel_entities WHERE id = ?",
                params![entity_id],
                |row| row.get::<_, String>(0),
            ) {
                Ok(payload) => Some(serde_json::from_str::<IntelEntity>(&payload).map_err(
                    |source| StorageError::Serialize {
                        table: "intel_entities".to_string(),
                        source,
                    },
                )?),
                Err(rusqlite::Error::QueryReturnedNoRows) => None,
                Err(source) => {
                    return Err(StorageError::Query {
                        table: "intel_entities".to_string(),
                        source,
                    });
                }
            }) else {
                return Ok(None);
            };
            entity.status = status;
            write_intel_entity(tx, &entity)?;
            Ok(Some(entity))
        })
    }

    fn list_intel_entities(
        &self,
        kind: Option<IntelEntityKind>,
        query: Option<&str>,
        limit: usize,
    ) -> Result<Vec<IntelEntityRecord>, StorageError> {
        let connection = self.lock()?;
        let (sql, values) = if let Some(kind) = kind {
            (
                "SELECT payload FROM intel_entities WHERE kind = ? ORDER BY seq DESC".to_string(),
                vec![SqlValue::Text(kind.as_str().to_string())],
            )
        } else {
            (
                "SELECT payload FROM intel_entities ORDER BY seq DESC".to_string(),
                Vec::new(),
            )
        };
        let mut statement = connection
            .prepare(&sql)
            .map_err(|source| StorageError::Query {
                table: "intel_entities".to_string(),
                source,
            })?;
        let rows = statement
            .query_map(rusqlite::params_from_iter(values), |row| {
                row.get::<_, String>(0)
            })
            .map_err(|source| StorageError::Query {
                table: "intel_entities".to_string(),
                source,
            })?;
        let needle = query.map(str::to_lowercase);
        let mut entities = Vec::new();
        for row in rows {
            let payload = row.map_err(|source| StorageError::Query {
                table: "intel_entities".to_string(),
                source,
            })?;
            let entity: IntelEntity =
                serde_json::from_str(&payload).map_err(|source| StorageError::Serialize {
                    table: "intel_entities".to_string(),
                    source,
                })?;
            if let Some(needle) = needle.as_ref()
                && !entity.normalized_value.to_lowercase().contains(needle)
                && !entity.value.to_lowercase().contains(needle)
            {
                continue;
            }
            entities.push(read_intel_entity_record(&connection, &entity.id)?);
            if entities.len() >= limit {
                break;
            }
        }
        Ok(entities)
    }

    fn list_intel_relations(
        &self,
        entity_id: Option<&str>,
    ) -> Result<Vec<IntelRelationRecord>, StorageError> {
        let connection = self.lock()?;
        let (sql, values) = if let Some(entity_id) = entity_id {
            (
                "SELECT payload FROM intel_relations \
                 WHERE from_entity_id = ? OR to_entity_id = ? ORDER BY seq DESC"
                    .to_string(),
                vec![
                    SqlValue::Text(entity_id.to_string()),
                    SqlValue::Text(entity_id.to_string()),
                ],
            )
        } else {
            (
                "SELECT payload FROM intel_relations ORDER BY seq DESC".to_string(),
                Vec::new(),
            )
        };
        read_intel_relation_payloads(&connection, &sql, &values)?
            .into_iter()
            .map(|relation| read_intel_relation_record(&connection, &relation.id))
            .collect()
    }

    fn list_intel_raw_records(
        &self,
        source: Option<&str>,
        limit: usize,
    ) -> Result<Vec<IntelRawRecord>, StorageError> {
        let connection = self.lock()?;
        let (sql, values) = if let Some(source) = source {
            (
                "SELECT payload FROM intel_raw_records WHERE source = ? \
                 ORDER BY seq DESC LIMIT ?"
                    .to_string(),
                vec![
                    SqlValue::Text(source.to_string()),
                    SqlValue::Integer(i64::try_from(limit).unwrap_or(100)),
                ],
            )
        } else {
            (
                "SELECT payload FROM intel_raw_records ORDER BY seq DESC LIMIT ?".to_string(),
                vec![SqlValue::Integer(i64::try_from(limit).unwrap_or(100))],
            )
        };
        let mut statement = connection
            .prepare(&sql)
            .map_err(|source| StorageError::Query {
                table: "intel_raw_records".to_string(),
                source,
            })?;
        let rows = statement
            .query_map(rusqlite::params_from_iter(values), |row| {
                row.get::<_, String>(0)
            })
            .map_err(|source| StorageError::Query {
                table: "intel_raw_records".to_string(),
                source,
            })?;
        let mut records = Vec::new();
        for row in rows {
            let payload = row.map_err(|source| StorageError::Query {
                table: "intel_raw_records".to_string(),
                source,
            })?;
            records.push(
                serde_json::from_str::<IntelRawRecord>(&payload).map_err(|source| {
                    StorageError::Serialize {
                        table: "intel_raw_records".to_string(),
                        source,
                    }
                })?,
            );
        }
        Ok(records)
    }

    // ------------------------------------------------------------------
    // 外部 Worker Runtime
    // ------------------------------------------------------------------

    fn upsert_worker_run(&self, run: &WorkerRun) -> Result<(), StorageError> {
        let connection = self.lock()?;
        upsert_worker_run_tx(&connection, run)
    }

    fn get_worker_run(&self, run_id: &str) -> Result<Option<WorkerRun>, StorageError> {
        let connection = self.lock()?;
        query_optional_payload::<WorkerRun>(
            &connection,
            "SELECT payload FROM worker_runs WHERE id = ?",
            params![run_id],
            "worker_runs",
        )
    }

    fn delete_worker_run(&self, run_id: &str) -> Result<bool, StorageError> {
        let connection = self.lock()?;
        let affected = connection
            .execute("DELETE FROM worker_runs WHERE id = ?", params![run_id])
            .map_err(|source| StorageError::Write {
                table: "worker_runs".to_string(),
                source,
            })?;
        Ok(affected > 0)
    }

    fn sum_worker_usage(
        &self,
        project_id: Option<&str>,
    ) -> Result<models::WorkerUsageSummary, StorageError> {
        let connection = self.lock()?;
        let sql = match project_id {
            Some(_) => "SELECT COUNT(*), COALESCE(SUM(input_tokens),0),                         COALESCE(SUM(output_tokens),0), COALESCE(SUM(cached_input_tokens),0),                         COALESCE(SUM(reasoning_tokens),0), COUNT(cost_usd), SUM(cost_usd)                         FROM worker_usage WHERE project_id = ?",
            None => "SELECT COUNT(*), COALESCE(SUM(input_tokens),0),                      COALESCE(SUM(output_tokens),0), COALESCE(SUM(cached_input_tokens),0),                      COALESCE(SUM(reasoning_tokens),0), COUNT(cost_usd), SUM(cost_usd)                      FROM worker_usage",
        };
        let mut statement = connection
            .prepare(sql)
            .map_err(|source| StorageError::Query {
                table: "worker_usage".to_string(),
                source,
            })?;
        let map_row = |row: &rusqlite::Row| -> Result<(i64, i64, i64, i64, i64, i64, Option<f64>), rusqlite::Error> {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
            ))
        };
        let row = match project_id {
            Some(project) => statement
                .query_row(params![project], map_row)
                .map_err(|source| StorageError::Query {
                    table: "worker_usage".to_string(),
                    source,
                })?,
            None => statement
                .query_row([], map_row)
                .map_err(|source| StorageError::Query {
                    table: "worker_usage".to_string(),
                    source,
                })?,
        };
        // 有部分 run 未报成本 → 聚合成本不可信，保持 None（不冒充精确值）。
        let (runs, input, output, cached, reasoning, cost_count, cost_sum) = row;
        Ok(models::WorkerUsageSummary {
            runs,
            input_tokens: input,
            output_tokens: output,
            cached_input_tokens: cached,
            reasoning_tokens: reasoning,
            cost_usd: if cost_count == runs && runs > 0 { cost_sum } else { None },
        })
    }

    fn sum_worker_usage_breakdown(
        &self,
        project_id: Option<&str>,
        days: Option<i64>,
        group_by: Option<models::WorkerUsageDimension>,
    ) -> Result<models::WorkerUsageBreakdown, StorageError> {
        let connection = self.lock()?;
        // 时间窗口在此换算（api 层无 chrono 依赖）；created_at 为 RFC3339
        // 文本，字典序比较即时间比较。
        let since = days.map(|window| {
            (chrono::Utc::now() - chrono::Duration::days(window.max(1)))
                .to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
        });
        let filter =
            "WHERE (?1 IS NULL OR project_id = ?1) AND (?2 IS NULL OR created_at >= ?2)";
        let map_tuple = |row: &rusqlite::Row| -> rusqlite::Result<(i64, i64, i64, i64, i64, i64, Option<f64>)> {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
            ))
        };
        // 组内部分 run 未报成本 → 该组成本保持 None（口径与总量聚合一致）。
        let cost_of = |(runs, _, _, _, _, cost_count, cost_sum): (
            i64,
            i64,
            i64,
            i64,
            i64,
            i64,
            Option<f64>,
        )| {
            if cost_count == runs && runs > 0 {
                cost_sum
            } else {
                None
            }
        };
        let query = |sql: &str| -> Result<rusqlite::Statement<'_>, StorageError> {
            connection
                .prepare(sql)
                .map_err(|source| StorageError::Query {
                    table: "worker_usage".to_string(),
                    source,
                })
        };
        let rows = |statement: &mut rusqlite::Statement<'_>| -> Result<Vec<(i64, i64, i64, i64, i64, i64, Option<f64>)>, StorageError> {
            statement
                .query_map(
                    params![project_id, since],
                    map_tuple,
                )
                .map_err(|source| StorageError::Query {
                    table: "worker_usage".to_string(),
                    source,
                })?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|source| StorageError::Query {
                    table: "worker_usage".to_string(),
                    source,
                })
        };

        let summary_row = rows(&mut query(&format!(
            "SELECT COUNT(*), COALESCE(SUM(input_tokens),0), \
             COALESCE(SUM(output_tokens),0), COALESCE(SUM(cached_input_tokens),0), \
             COALESCE(SUM(reasoning_tokens),0), COUNT(cost_usd), SUM(cost_usd) \
             FROM worker_usage {filter}"
        ))?)?;
        let summary = summary_row
            .first()
            .map(|row| models::WorkerUsageSummary {
                runs: row.0,
                input_tokens: row.1,
                output_tokens: row.2,
                cached_input_tokens: row.3,
                reasoning_tokens: row.4,
                cost_usd: cost_of(*row),
            })
            .unwrap_or_default();

        let daily: Vec<models::WorkerUsageDailyPoint> = {
            let mut statement = query(&format!(
                "SELECT substr(COALESCE(created_at,''),1,10), COUNT(*), \
                 COALESCE(SUM(input_tokens),0), COALESCE(SUM(output_tokens),0), \
                 COALESCE(SUM(cached_input_tokens),0), COUNT(cost_usd), SUM(cost_usd) \
                 FROM worker_usage {filter} GROUP BY 1 ORDER BY 1"
            ))?;
            let mapped = statement
                .query_map(
                    params![project_id, since],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, i64>(2)?,
                            row.get::<_, i64>(3)?,
                            row.get::<_, i64>(4)?,
                            row.get::<_, i64>(5)?,
                            row.get::<_, Option<f64>>(6)?,
                        ))
                    },
                )
                .map_err(|source| StorageError::Query {
                    table: "worker_usage".to_string(),
                    source,
                })?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|source| StorageError::Query {
                    table: "worker_usage".to_string(),
                    source,
                })?;
            mapped
                .into_iter()
                .filter(|row| !row.0.is_empty())
                .map(|(day, runs, input, output, cached, cost_count, cost_sum)| {
                    models::WorkerUsageDailyPoint {
                        day,
                        runs,
                        input_tokens: input,
                        output_tokens: output,
                        cached_input_tokens: cached,
                        cost_usd: cost_of((runs, input, output, cached, 0, cost_count, cost_sum)),
                    }
                })
                .collect()
        };

        // 标签与聚合同一查询取回（同查同序，避免二次查询配对错位）。
        let slice_sql = |label_expr: &'static str, group_by: &'static str| {
            format!(
                "SELECT COUNT(*), COALESCE(SUM(input_tokens),0), \
                 COALESCE(SUM(output_tokens),0), COALESCE(SUM(cached_input_tokens),0), \
                 COALESCE(SUM(reasoning_tokens),0), COUNT(cost_usd), SUM(cost_usd), \
                 {label_expr} \
                 FROM worker_usage {filter} GROUP BY {group_by}"
            )
        };
        let map_slice_row = |row: &rusqlite::Row| -> rusqlite::Result<
            (
                i64,
                i64,
                i64,
                i64,
                i64,
                i64,
                Option<f64>,
                String,
                Option<String>,
                Option<String>,
            ),
        > {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
                row.get(7)?,
                row.get(8)?,
                row.get(9)?,
            ))
        };
        // move 闭包捕获 clone，原 `since` 留给分组每日聚合复用。
        let slice_since = since.clone();
        let slice_project = project_id;
        let collect_slices = move |sql: String| -> Result<Vec<models::WorkerUsageModelSlice>, StorageError> {
            let mut statement = query(&sql)?;
            let mapped = statement
                .query_map(params![slice_project, slice_since], map_slice_row)
                .map_err(|source| StorageError::Query {
                    table: "worker_usage".to_string(),
                    source,
                })?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|source| StorageError::Query {
                    table: "worker_usage".to_string(),
                    source,
                })?;
            let mut slices: Vec<models::WorkerUsageModelSlice> = mapped
                .into_iter()
                .map(
                    |(runs, input, output, cached, _, cost_count, cost_sum, runtime, model, requested)| {
                        let tuple = (runs, input, output, cached, 0, cost_count, cost_sum);
                        models::WorkerUsageModelSlice {
                            runtime,
                            model,
                            requested_model: requested,
                            runs,
                            input_tokens: input,
                            output_tokens: output,
                            cached_input_tokens: cached,
                            cost_usd: cost_of(tuple),
                        }
                    },
                )
                .collect();
            // 总 token 降序，统计页"分析"柱状图直接使用。
            slices.sort_by_key(|slice| std::cmp::Reverse(slice.input_tokens + slice.output_tokens));
            Ok(slices)
        };

        let by_model = collect_slices(slice_sql(
            "runtime, model, requested_model",
            "runtime, model, requested_model",
        ))?;
        let by_runtime = collect_slices(slice_sql("runtime, NULL, NULL", "runtime"))?;

        // 分组每日聚合（group_by 指定维度时；GROUP BY 维度即 tab 语义，
        // 只换分组列，不换口径）。
        let grouped_daily = match group_by {
            None => Vec::new(),
            Some(dimension) => {
                let key_expr = match dimension {
                    models::WorkerUsageDimension::Runtime => "runtime",
                    models::WorkerUsageDimension::Model => "model",
                };
                let sql = format!(
                    "SELECT substr(COALESCE(created_at,''),1,10), {key_expr}, COUNT(*), \
                     COALESCE(SUM(input_tokens),0), COALESCE(SUM(output_tokens),0), \
                     COALESCE(SUM(cached_input_tokens),0), COUNT(cost_usd), SUM(cost_usd) \
                     FROM worker_usage {filter} GROUP BY 1, 2 ORDER BY 1, 2"
                );
                let mut statement = query(&sql)?;
                let grouped_since = since.clone();
                let mapped = statement
                    .query_map(params![project_id, grouped_since], |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, Option<String>>(1)?,
                            row.get::<_, i64>(2)?,
                            row.get::<_, i64>(3)?,
                            row.get::<_, i64>(4)?,
                            row.get::<_, i64>(5)?,
                            row.get::<_, i64>(6)?,
                            row.get::<_, Option<f64>>(7)?,
                        ))
                    })
                    .map_err(|source| StorageError::Query {
                        table: "worker_usage".to_string(),
                        source,
                    })?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|source| StorageError::Query {
                        table: "worker_usage".to_string(),
                        source,
                    })?;
                mapped
                    .into_iter()
                    .filter(|(day, ..)| !day.is_empty())
                    .map(
                        |(day, key, runs, input, output, cached, cost_count, cost_sum)| {
                            models::WorkerUsageGroupedDailyPoint {
                                day,
                                key,
                                runs,
                                input_tokens: input,
                                output_tokens: output,
                                cached_input_tokens: cached,
                                cost_usd: cost_of((runs, input, output, cached, 0, cost_count, cost_sum)),
                            }
                        },
                    )
                    .collect()
            }
        };

        Ok(models::WorkerUsageBreakdown {
            summary,
            daily,
            by_model,
            by_runtime,
            grouped_daily,
        })
    }

    fn dashboard_stats(&self) -> Result<models::DashboardStats, StorageError> {
        let connection = self.lock()?;
        let scalar = |sql: &str| -> Result<i64, StorageError> {
            connection
                .query_row(sql, [], |row| row.get::<_, i64>(0))
                .map_err(|source| StorageError::Query {
                    table: "dashboard".to_string(),
                    source,
                })
        };
        // typed collection 的 payload 内字段用 json_extract 计数
        //（bundled SQLite 含 JSON1；量级小，全扫可接受）。
        let grouped = |sql: &str| -> Result<Vec<(String, i64)>, StorageError> {
            connection
                .prepare(sql)
                .map_err(|source| StorageError::Query {
                    table: "dashboard".to_string(),
                    source,
                })?
                .query_map([], |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?.unwrap_or_default(),
                        row.get::<_, i64>(1)?,
                    ))
                })
                .map_err(|source| StorageError::Query {
                    table: "dashboard".to_string(),
                    source,
                })?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|source| StorageError::Query {
                    table: "dashboard".to_string(),
                    source,
                })
        };

        // 卡片一：活跃任务按状态拆分（missions 为 payload 存储）。
        let mut missions = models::MissionActivityStats::default();
        for (status, count) in grouped(
            "SELECT json_extract(payload,'$.status'), COUNT(*) \
             FROM missions GROUP BY 1",
        )? {
            match status.as_str() {
                "running" => missions.running = count,
                "paused" => missions.paused = count,
                "waiting_for_decision" => missions.waiting_for_decision = count,
                _ => {}
            }
        }
        missions.total = missions.running + missions.paused + missions.waiting_for_decision;

        // 卡片二：Confirmed Finding 按 Severity 拆分（缺省 Medium 由模型
        // 序列化保证，wire 值即 severity 字段）。
        let mut findings = models::ConfirmedFindingStats::default();
        for (severity, count) in grouped(
            "SELECT json_extract(payload,'$.severity'), COUNT(*) FROM findings \
             WHERE json_extract(payload,'$.status') = 'confirmed' GROUP BY 1",
        )? {
            match severity.as_str() {
                "critical" => findings.critical = count,
                "high" => findings.high = count,
                "medium" => findings.medium = count,
                "low" => findings.low = count,
                _ => findings.info += count,
            }
        }
        findings.total =
            findings.critical + findings.high + findings.medium + findings.low + findings.info;

        // 卡片三：资产节点（专用 DDL 表，直接 SQL；全局去重 + 按类型去重）。
        let distinct_count = scalar("SELECT COUNT(DISTINCT normalized_value) FROM mission_assets")?;
        let mut by_type: Vec<models::AssetTypeCount> = grouped(
            "SELECT asset_type, COUNT(DISTINCT normalized_value) \
             FROM mission_assets GROUP BY 1",
        )?
        .into_iter()
        .map(|(asset_type, count)| models::AssetTypeCount { asset_type, count })
        .collect();
        by_type.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.asset_type.cmp(&b.asset_type)));
        let asset_nodes = models::AssetNodeStats { distinct_count, by_type };

        // 卡片四：工具调用总量 + 在途 worker（worker_runs.status 是物理列）。
        let tool_calls = models::ToolCallStats {
            total_invocations: scalar("SELECT COUNT(*) FROM tool_invocations")?,
            active_worker_runs: scalar(
                "SELECT COUNT(*) FROM worker_runs WHERE status IN ('pending','running')",
            )?,
        };

        // 卡片五：Token 用量（worker_usage 只在有真实上报时才有行；
        // 无行时 reported_runs = 0，前端据此显示缺失而非 0）。
        let usage = connection
            .query_row(
                "SELECT COUNT(*), COALESCE(SUM(input_tokens),0), \
                 COALESCE(SUM(output_tokens),0), COALESCE(SUM(cached_input_tokens),0), \
                 COUNT(cost_usd), SUM(cost_usd) FROM worker_usage",
                [],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, Option<f64>>(5)?,
                    ))
                },
            )
            .map_err(|source| StorageError::Query {
                table: "worker_usage".to_string(),
                source,
            })?;
        let (reported_runs, input, output, cached, cost_runs, cost_sum) = usage;
        let token_usage = models::TokenUsageStats {
            reported_runs,
            input_tokens: input,
            output_tokens: output,
            cached_input_tokens: cached,
            cost_reported_runs: cost_runs,
            // 有部分 run 未报成本 → 聚合成本不可信，保持 None。
            cost_usd: if cost_runs == reported_runs && reported_runs > 0 {
                cost_sum
            } else {
                None
            },
        };

        Ok(models::DashboardStats {
            missions,
            confirmed_findings: findings,
            asset_nodes,
            tool_calls,
            token_usage,
        })
    }

    fn list_worker_runs(
        &self,
        project_id: Option<&str>,
        run_id: Option<&str>,
        task_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<WorkerRun>, StorageError> {
        let connection = self.lock()?;
        // 低容量审计表：SQL 只按 project 过滤，其余条件内存过滤
        //（镜像"部分过滤器 SQL 后内存过滤"模式）。
        let (sql, values) = if let Some(project_id) = project_id {
            (
                "SELECT payload FROM worker_runs WHERE project_id = ? ORDER BY seq DESC"
                    .to_string(),
                vec![SqlValue::Text(project_id.to_string())],
            )
        } else {
            (
                "SELECT payload FROM worker_runs ORDER BY seq DESC".to_string(),
                Vec::new(),
            )
        };
        let mut runs =
            query_worker_payloads::<WorkerRun>(&connection, &sql, values, "worker_runs")?;
        runs.retain(|run| {
            run_id.is_none_or(|id| {
                run.run_id
                    .as_ref()
                    .is_some_and(|value| value.as_str() == id)
            }) && task_id.is_none_or(|id| {
                run.task_id
                    .as_ref()
                    .is_some_and(|value| value.as_str() == id)
            })
        });
        runs.truncate(limit);
        Ok(runs)
    }

    fn upsert_worker_invocation(&self, invocation: &WorkerInvocation) -> Result<(), StorageError> {
        let connection = self.lock()?;
        let payload =
            serde_json::to_string(invocation).map_err(|source| StorageError::Serialize {
                table: "worker_invocations".to_string(),
                source,
            })?;
        let project_id = invocation
            .project_id
            .as_ref()
            .map(models::ProjectId::as_str);
        connection
            .execute(
                "INSERT INTO worker_invocations \
                 (id, project_id, worker_run_id, runtime, purpose, status, payload, created_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
                 ON CONFLICT(id) DO UPDATE SET \
                 project_id=excluded.project_id, worker_run_id=excluded.worker_run_id, \
                 runtime=excluded.runtime, purpose=excluded.purpose, status=excluded.status, \
                 payload=excluded.payload",
                params![
                    invocation.id,
                    project_id,
                    invocation.worker_run_id,
                    invocation.runtime.as_str(),
                    invocation.purpose.as_str(),
                    invocation.status.as_str(),
                    payload,
                    invocation.started_at.isoformat(),
                ],
            )
            .map_err(|source| StorageError::Write {
                table: "worker_invocations".to_string(),
                source,
            })?;
        Ok(())
    }

    fn list_worker_invocations(
        &self,
        worker_run_id: Option<&str>,
        project_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<WorkerInvocation>, StorageError> {
        let connection = self.lock()?;
        let (sql, values) = match (worker_run_id, project_id) {
            (Some(worker_run_id), _) => (
                "SELECT payload FROM worker_invocations WHERE worker_run_id = ? \
                 ORDER BY seq ASC"
                    .to_string(),
                vec![SqlValue::Text(worker_run_id.to_string())],
            ),
            (None, Some(project_id)) => (
                "SELECT payload FROM worker_invocations WHERE project_id = ? \
                 ORDER BY seq DESC"
                    .to_string(),
                vec![SqlValue::Text(project_id.to_string())],
            ),
            (None, None) => (
                "SELECT payload FROM worker_invocations ORDER BY seq DESC".to_string(),
                Vec::new(),
            ),
        };
        let mut invocations = query_worker_payloads::<WorkerInvocation>(
            &connection,
            &sql,
            values,
            "worker_invocations",
        )?;
        invocations.truncate(limit);
        Ok(invocations)
    }

    fn upsert_worker_runtime_profile(
        &self,
        profile: &WorkerRuntimeProfile,
    ) -> Result<(), StorageError> {
        let connection = self.lock()?;
        let payload = serde_json::to_string(profile).map_err(|source| StorageError::Serialize {
            table: "worker_runtime_profiles".to_string(),
            source,
        })?;
        connection
            .execute(
                "INSERT INTO worker_runtime_profiles \
                 (id, runtime_type, connection_id, enabled, payload, created_at) \
                 VALUES (?, ?, ?, ?, ?, ?) \
                 ON CONFLICT(id) DO UPDATE SET \
                 runtime_type=excluded.runtime_type, connection_id=excluded.connection_id, \
                 enabled=excluded.enabled, payload=excluded.payload",
                params![
                    profile.id,
                    profile.runtime_type.as_str(),
                    profile.connection_id,
                    i64::from(profile.enabled),
                    payload,
                    profile.created_at.isoformat(),
                ],
            )
            .map_err(|source| StorageError::Write {
                table: "worker_runtime_profiles".to_string(),
                source,
            })?;
        Ok(())
    }

    fn get_worker_runtime_profile(
        &self,
        profile_id: &str,
    ) -> Result<Option<WorkerRuntimeProfile>, StorageError> {
        let connection = self.lock()?;
        query_optional_payload::<WorkerRuntimeProfile>(
            &connection,
            "SELECT payload FROM worker_runtime_profiles WHERE id = ?",
            params![profile_id],
            "worker_runtime_profiles",
        )
    }

    fn list_worker_runtime_profiles(
        &self,
        runtime_type: Option<WorkerRuntimeType>,
    ) -> Result<Vec<WorkerRuntimeProfile>, StorageError> {
        let connection = self.lock()?;
        let (sql, values) = if let Some(runtime_type) = runtime_type {
            (
                "SELECT payload FROM worker_runtime_profiles WHERE runtime_type = ? \
                 ORDER BY seq DESC"
                    .to_string(),
                vec![SqlValue::Text(runtime_type.as_str().to_string())],
            )
        } else {
            (
                "SELECT payload FROM worker_runtime_profiles ORDER BY seq DESC".to_string(),
                Vec::new(),
            )
        };
        query_worker_payloads::<WorkerRuntimeProfile>(
            &connection,
            &sql,
            values,
            "worker_runtime_profiles",
        )
    }

    fn delete_worker_runtime_profile(&self, profile_id: &str) -> Result<bool, StorageError> {
        let connection = self.lock()?;
        let affected = connection
            .execute(
                "DELETE FROM worker_runtime_profiles WHERE id = ?",
                params![profile_id],
            )
            .map_err(|source| StorageError::Write {
                table: "worker_runtime_profiles".to_string(),
                source,
            })?;
        Ok(affected > 0)
    }

    fn upsert_agent_preset(&self, preset: &models::AgentPreset) -> Result<(), StorageError> {
        let connection = self.lock()?;
        let payload = serde_json::to_string(preset).map_err(|source| StorageError::Serialize {
            table: "agent_presets".to_string(),
            source,
        })?;
        connection
            .execute(
                "INSERT INTO agent_presets (id, builtin, enabled, payload, created_at) \
                 VALUES (?, ?, ?, ?, ?) \
                 ON CONFLICT(id) DO UPDATE SET \
                 builtin=excluded.builtin, enabled=excluded.enabled, payload=excluded.payload",
                params![
                    preset.key,
                    i64::from(preset.builtin),
                    i64::from(preset.enabled),
                    payload,
                    preset.created_at.isoformat(),
                ],
            )
            .map_err(|source| StorageError::Write {
                table: "agent_presets".to_string(),
                source,
            })?;
        Ok(())
    }

    fn get_agent_preset(&self, key: &str) -> Result<Option<models::AgentPreset>, StorageError> {
        let connection = self.lock()?;
        query_optional_payload::<models::AgentPreset>(
            &connection,
            "SELECT payload FROM agent_presets WHERE id = ?",
            params![key],
            "agent_presets",
        )
    }

    fn list_agent_presets(
        &self,
        builtin: Option<bool>,
    ) -> Result<Vec<models::AgentPreset>, StorageError> {
        let connection = self.lock()?;
        let (sql, values) = if let Some(builtin) = builtin {
            (
                "SELECT payload FROM agent_presets WHERE builtin = ? ORDER BY seq DESC".to_string(),
                vec![SqlValue::Integer(i64::from(builtin))],
            )
        } else {
            (
                "SELECT payload FROM agent_presets ORDER BY seq DESC".to_string(),
                Vec::new(),
            )
        };
        query_worker_payloads::<models::AgentPreset>(&connection, &sql, values, "agent_presets")
    }

    fn delete_agent_preset(&self, key: &str) -> Result<bool, StorageError> {
        let connection = self.lock()?;
        let affected = connection
            .execute("DELETE FROM agent_presets WHERE id = ?", params![key])
            .map_err(|source| StorageError::Write {
                table: "agent_presets".to_string(),
                source,
            })?;
        Ok(affected > 0)
    }

    fn record_skill_usage(&self, row: &models::skill::SkillUsageRow) -> Result<(), StorageError> {
        let connection = self.lock()?;
        connection
            .execute(
                "INSERT INTO skill_usage (ts, skill, agent_preset, mission_id, run_id, args_len, found)                  VALUES (?, ?, ?, ?, ?, ?, ?)",
                params![
                    row.ts,
                    row.skill,
                    row.agent_preset,
                    row.mission_id,
                    row.run_id,
                    row.args_len,
                    i64::from(row.found),
                ],
            )
            .map_err(|source| StorageError::Write {
                table: "skill_usage".to_string(),
                source,
            })?;
        Ok(())
    }

    fn list_skill_usage(
        &self,
        skill: Option<&str>,
        limit: usize,
    ) -> Result<Vec<models::skill::SkillUsageRow>, StorageError> {
        let connection = self.lock()?;
        let mut statement = connection
            .prepare(
                "SELECT ts, skill, agent_preset, mission_id, run_id, args_len, found                  FROM skill_usage WHERE (?1 IS NULL OR skill = ?1) ORDER BY seq DESC LIMIT ?2",
            )
            .map_err(|source| StorageError::Query {
                table: "skill_usage".to_string(),
                source,
            })?;
        let rows = statement
            .query_map(params![skill, limit as i64], |row| {
                Ok(models::skill::SkillUsageRow {
                    ts: row.get(0)?,
                    skill: row.get(1)?,
                    agent_preset: row.get(2)?,
                    mission_id: row.get(3)?,
                    run_id: row.get(4)?,
                    args_len: row.get(5)?,
                    found: row.get::<_, i64>(6)? != 0,
                })
            })
            .map_err(|source| StorageError::Query {
                table: "skill_usage".to_string(),
                source,
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|source| StorageError::Query {
                table: "skill_usage".to_string(),
                source,
            })?;
        Ok(rows)
    }

    fn skill_missing_report(&self) -> Result<Vec<models::skill::SkillMissingEntry>, StorageError> {
        let connection = self.lock()?;
        let mut statement = connection
            .prepare(
                "SELECT skill, COUNT(*), MAX(ts) FROM skill_usage WHERE found = 0                  GROUP BY skill ORDER BY COUNT(*) DESC, MAX(ts) DESC",
            )
            .map_err(|source| StorageError::Query {
                table: "skill_usage".to_string(),
                source,
            })?;
        let rows = statement
            .query_map([], |row| {
                Ok(models::skill::SkillMissingEntry {
                    skill: row.get(0)?,
                    misses: row.get(1)?,
                    last_ts: row.get(2)?,
                })
            })
            .map_err(|source| StorageError::Query {
                table: "skill_usage".to_string(),
                source,
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|source| StorageError::Query {
                table: "skill_usage".to_string(),
                source,
            })?;
        Ok(rows)
    }
}

/// upsert worker run（事务内外通用 helper）。
fn upsert_worker_run_tx(connection: &Connection, run: &WorkerRun) -> Result<(), StorageError> {
    let payload = serde_json::to_string(run).map_err(|source| StorageError::Serialize {
        table: "worker_runs".to_string(),
        source,
    })?;
    let run_id = run.run_id.as_ref().map(models::RunId::as_str);
    let task_id = run.task_id.as_ref().map(models::TaskId::as_str);
    connection
        .execute(
            "INSERT INTO worker_runs \
             (id, project_id, run_id, task_id, runtime, status, agent_preset_id, payload, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(id) DO UPDATE SET \
             project_id=excluded.project_id, run_id=excluded.run_id, task_id=excluded.task_id, \
             runtime=excluded.runtime, status=excluded.status, \
             agent_preset_id=excluded.agent_preset_id, payload=excluded.payload",
            params![
                run.id,
                run.project_id.as_str(),
                run_id,
                task_id,
                run.runtime.as_str(),
                run.status.as_str(),
                run.agent_preset_id,
                payload,
                run.created_at.isoformat(),
            ],
        )
        .map_err(|source| StorageError::Write {
            table: "worker_runs".to_string(),
            source,
        })?;
    if let Some(usage) = run.usage.as_ref() {
        connection
            .execute(
                "INSERT INTO worker_usage                  (run_id, project_id, runtime, model, requested_model, input_tokens,                   output_tokens, cached_input_tokens, reasoning_tokens, cost_usd,                   num_turns, duration_api_ms, created_at)                  VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)                  ON CONFLICT(run_id) DO UPDATE SET                  project_id=excluded.project_id, runtime=excluded.runtime, model=excluded.model,                  requested_model=excluded.requested_model, input_tokens=excluded.input_tokens,                  output_tokens=excluded.output_tokens, cached_input_tokens=excluded.cached_input_tokens,                  reasoning_tokens=excluded.reasoning_tokens, cost_usd=excluded.cost_usd,                  num_turns=excluded.num_turns, duration_api_ms=excluded.duration_api_ms",
                params![
                    run.id,
                    run.project_id.as_str(),
                    run.runtime.as_str(),
                    run.model,
                    usage.requested_model,
                    usage.input_tokens,
                    usage.output_tokens,
                    usage.cached_input_tokens,
                    usage.reasoning_tokens,
                    usage.cost_usd,
                    usage.num_turns,
                    usage.duration_api_ms,
                    run.created_at.isoformat(),
                ],
            )
            .map_err(|source| StorageError::Write {
                table: "worker_usage".to_string(),
                source,
            })?;
    }
    Ok(())
}

/// 按 id 可选读取一条 payload 实体。
fn query_optional_payload<E: serde::de::DeserializeOwned>(
    connection: &Connection,
    sql: &str,
    params: impl rusqlite::Params,
    table: &'static str,
) -> Result<Option<E>, StorageError> {
    match connection.query_row(sql, params, |row| row.get::<_, String>(0)) {
        Ok(payload) => serde_json::from_str::<E>(&payload)
            .map(Some)
            .map_err(|source| StorageError::Serialize {
                table: table.to_string(),
                source,
            }),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(source) => Err(StorageError::Query {
            table: table.to_string(),
            source,
        }),
    }
}

/// 执行一条 `SELECT payload` 查询并反序列化（worker 表通用）。
fn query_worker_payloads<E: serde::de::DeserializeOwned>(
    connection: &Connection,
    sql: &str,
    values: Vec<SqlValue>,
    table: &'static str,
) -> Result<Vec<E>, StorageError> {
    let mut statement = connection
        .prepare(sql)
        .map_err(|source| StorageError::Query {
            table: table.to_string(),
            source,
        })?;
    let rows = statement
        .query_map(params_from_iter(values), |row| row.get::<_, String>(0))
        .map_err(|source| StorageError::Query {
            table: table.to_string(),
            source,
        })?;
    let mut items = Vec::new();
    for row in rows {
        let payload = row.map_err(|source| StorageError::Query {
            table: table.to_string(),
            source,
        })?;
        items.push(serde_json::from_str::<E>(&payload).map_err(|source| {
            StorageError::Serialize {
                table: table.to_string(),
                source,
            }
        })?);
    }
    Ok(items)
}

/// 按去重键读情报实体（内部 helper，事务内外通用）。
fn query_intel_entity_by_key(
    connection: &Connection,
    kind: IntelEntityKind,
    normalized_value: &str,
) -> Result<Option<IntelEntity>, StorageError> {
    let sql = "SELECT payload FROM intel_entities \
               WHERE kind = ? AND normalized_value = ?";
    match connection.query_row(sql, params![kind.as_str(), normalized_value], |row| {
        row.get::<_, String>(0)
    }) {
        Ok(payload) => serde_json::from_str::<IntelEntity>(&payload)
            .map(Some)
            .map_err(|source| StorageError::Serialize {
                table: "intel_entities".to_string(),
                source,
            }),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(source) => Err(StorageError::Query {
            table: "intel_entities".to_string(),
            source,
        }),
    }
}

fn upsert_intel_entity_tx(
    connection: &Connection,
    entity: &IntelEntity,
) -> Result<IntelEntity, StorageError> {
    let existing = query_intel_entity_by_key(connection, entity.kind, &entity.normalized_value)?;
    let stored = if let Some(existing) = existing {
        let mut hit_sources = existing.hit_sources.clone();
        for source in &entity.hit_sources {
            if !hit_sources.contains(source) {
                hit_sources.push(source.clone());
            }
        }
        let source_count = u32::try_from(hit_sources.len()).unwrap_or(u32::MAX);
        let confidence = if source_count >= 2 {
            existing
                .confidence
                .max(entity.confidence)
                .max(models::IntelConfidence::High)
        } else {
            existing.confidence.max(entity.confidence)
        };
        IntelEntity {
            id: existing.id,
            kind: existing.kind,
            value: existing.value,
            normalized_value: existing.normalized_value,
            confidence,
            status: existing.status,
            first_seen: existing.first_seen.min(entity.first_seen),
            last_seen: existing.last_seen.max(entity.last_seen),
            source_count,
            hit_sources,
        }
    } else {
        entity.clone()
    };
    write_intel_entity(connection, &stored)?;
    Ok(stored)
}

fn upsert_intel_relation_tx(
    connection: &Connection,
    relation: &IntelRelation,
) -> Result<IntelRelation, StorageError> {
    let sql = "SELECT payload FROM intel_relations \
               WHERE from_entity_id = ? AND relation = ? AND to_entity_id = ? \
               ORDER BY seq LIMIT 1";
    let existing = read_intel_relation_payloads(
        connection,
        sql,
        &[
            SqlValue::Text(relation.from_entity_id.clone()),
            SqlValue::Text(relation.relation.as_str().to_string()),
            SqlValue::Text(relation.to_entity_id.clone()),
        ],
    )?
    .into_iter()
    .next();
    let stored = if let Some(existing) = existing {
        let mut hit_sources = existing.hit_sources.clone();
        for source in &relation.hit_sources {
            if !hit_sources.contains(source) {
                hit_sources.push(source.clone());
            }
        }
        IntelRelation {
            id: existing.id,
            from_entity_id: existing.from_entity_id,
            relation: existing.relation,
            to_entity_id: existing.to_entity_id,
            confidence: existing.confidence.max(relation.confidence),
            hit_sources,
            source_count: 0,
            first_seen: existing.first_seen.min(relation.first_seen),
            last_seen: existing.last_seen.max(relation.last_seen),
        }
    } else {
        relation.clone()
    };
    let mut stored = stored;
    stored.source_count = u32::try_from(stored.hit_sources.len()).unwrap_or(u32::MAX);
    write_intel_relation(connection, &stored)?;
    Ok(stored)
}

fn write_intel_raw_record(
    connection: &Connection,
    record: &IntelRawRecord,
) -> Result<(), StorageError> {
    let payload = serde_json::to_string(record).map_err(|source| StorageError::Serialize {
        table: "intel_raw_records".to_string(),
        source,
    })?;
    connection
        .execute(
            "INSERT INTO intel_raw_records \
             (id, source, source_record_id, payload, created_at) \
             VALUES (?, ?, ?, ?, ?)",
            params![
                record.id,
                record.source,
                record.source_record_id,
                payload,
                record.fetched_at.isoformat(),
            ],
        )
        .map_err(write_error("intel_raw_records"))?;
    Ok(())
}

fn read_intel_entity_record(
    connection: &Connection,
    entity_id: &str,
) -> Result<IntelEntityRecord, StorageError> {
    let payload = connection
        .query_row(
            "SELECT payload FROM intel_entities WHERE id = ?",
            params![entity_id],
            |row| row.get::<_, String>(0),
        )
        .map_err(|source| StorageError::Query {
            table: "intel_entities".to_string(),
            source,
        })?;
    let entity = serde_json::from_str(&payload).map_err(|source| StorageError::Decode {
        table: "intel_entities".to_string(),
        source,
    })?;
    let provenance =
        read_intel_provenance(connection, "intel_entity_sources", "entity_id", entity_id)?;
    Ok(IntelEntityRecord { entity, provenance })
}

fn read_intel_relation_record(
    connection: &Connection,
    relation_id: &str,
) -> Result<IntelRelationRecord, StorageError> {
    let payload = connection
        .query_row(
            "SELECT payload FROM intel_relations WHERE id = ?",
            params![relation_id],
            |row| row.get::<_, String>(0),
        )
        .map_err(|source| StorageError::Query {
            table: "intel_relations".to_string(),
            source,
        })?;
    let relation = serde_json::from_str(&payload).map_err(|source| StorageError::Decode {
        table: "intel_relations".to_string(),
        source,
    })?;
    let provenance = read_intel_provenance(
        connection,
        "intel_relation_sources",
        "relation_id",
        relation_id,
    )?;
    Ok(IntelRelationRecord {
        relation,
        provenance,
    })
}

fn read_intel_provenance(
    connection: &Connection,
    association_table: &'static str,
    owner_column: &'static str,
    owner_id: &str,
) -> Result<Vec<IntelProvenance>, StorageError> {
    let sql = format!(
        "SELECT raw.payload FROM intel_raw_records raw \
         JOIN {association_table} links ON links.raw_record_id = raw.id \
         WHERE links.{owner_column} = ? ORDER BY raw.seq"
    );
    let mut statement = connection
        .prepare(&sql)
        .map_err(|source| StorageError::Query {
            table: association_table.to_string(),
            source,
        })?;
    let rows = statement
        .query_map(params![owner_id], |row| row.get::<_, String>(0))
        .map_err(|source| StorageError::Query {
            table: association_table.to_string(),
            source,
        })?;
    let mut provenance = Vec::new();
    for row in rows {
        let payload = row.map_err(|source| StorageError::Query {
            table: association_table.to_string(),
            source,
        })?;
        let raw: IntelRawRecord =
            serde_json::from_str(&payload).map_err(|source| StorageError::Decode {
                table: "intel_raw_records".to_string(),
                source,
            })?;
        provenance.push(IntelProvenance::from(&raw));
    }
    Ok(provenance)
}

/// 写情报实体行（按 id upsert）。
fn write_intel_entity(connection: &Connection, entity: &IntelEntity) -> Result<(), StorageError> {
    let payload = serde_json::to_string(entity).map_err(|source| StorageError::Serialize {
        table: "intel_entities".to_string(),
        source,
    })?;
    connection
        .execute(
            "INSERT INTO intel_entities \
             (id, kind, normalized_value, payload, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?) \
             ON CONFLICT(id) DO UPDATE SET \
             kind=excluded.kind, normalized_value=excluded.normalized_value, \
             payload=excluded.payload, updated_at=excluded.updated_at",
            params![
                entity.id,
                entity.kind.as_str(),
                entity.normalized_value,
                payload,
                entity.first_seen.isoformat(),
                entity.last_seen.isoformat(),
            ],
        )
        .map_err(write_error("intel_entities"))?;
    Ok(())
}

/// 写情报关系行（按 id upsert）。
fn write_intel_relation(
    connection: &Connection,
    relation: &IntelRelation,
) -> Result<(), StorageError> {
    let payload = serde_json::to_string(relation).map_err(|source| StorageError::Serialize {
        table: "intel_relations".to_string(),
        source,
    })?;
    connection
        .execute(
            "INSERT INTO intel_relations \
             (id, from_entity_id, relation, to_entity_id, payload, created_at) \
             VALUES (?, ?, ?, ?, ?, ?) \
             ON CONFLICT(id) DO UPDATE SET \
             payload=excluded.payload",
            params![
                relation.id,
                relation.from_entity_id,
                relation.relation.as_str(),
                relation.to_entity_id,
                payload,
                relation.first_seen.isoformat(),
            ],
        )
        .map_err(write_error("intel_relations"))?;
    Ok(())
}

/// 按任意 WHERE SQL 读关系 payload 集（内部 helper）。
fn read_intel_relation_payloads(
    connection: &Connection,
    sql: &str,
    values: &[SqlValue],
) -> Result<Vec<IntelRelation>, StorageError> {
    let mut statement = connection
        .prepare(sql)
        .map_err(|source| StorageError::Query {
            table: "intel_relations".to_string(),
            source,
        })?;
    let rows = statement
        .query_map(rusqlite::params_from_iter(values.iter().cloned()), |row| {
            row.get::<_, String>(0)
        })
        .map_err(|source| StorageError::Query {
            table: "intel_relations".to_string(),
            source,
        })?;
    let mut relations = Vec::new();
    for row in rows {
        let payload = row.map_err(|source| StorageError::Query {
            table: "intel_relations".to_string(),
            source,
        })?;
        relations.push(
            serde_json::from_str::<IntelRelation>(&payload).map_err(|source| {
                StorageError::Serialize {
                    table: "intel_relations".to_string(),
                    source,
                }
            })?,
        );
    }
    Ok(relations)
}

/// Python `_clear_other_default_providers`：除指定 provider 外，把所有
/// `is_default=true` 的行改为 `false`（payload 整行重写，`updated_at` 不动）。
fn clear_other_default_providers(
    connection: &Connection,
    provider_id: &str,
) -> Result<(), StorageError> {
    let providers = list_all::<ProviderConfig>(connection)?;
    for mut provider in providers {
        if provider.entity_id() == provider_id || !provider.is_default {
            continue;
        }
        provider.is_default = false;
        upsert(connection, &provider)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use models::EvidenceKind;
    use models::FindingStatus;
    use models::MissionStatus;
    use models::ModelInvocationStatus;
    use models::ProviderType;
    use models::RunStatus;
    use models::TaskStatus;
    use models::Timestamp;
    use models::{
        AuditDomain, AuditEventType, CoverageAssessmentId, CritiqueReportId, CritiqueVerdict,
        EscalationGuardVerdictId, ExecutionId, ExecutionRequest, ExitGateDecisionId, FactId,
        IntentId, MetacognitionAssessmentId, ModuleId, ObservationId, RuntimeSetting,
        StrategyBoardSnapshotId, TrajectorySummaryId,
    };
    use models::{BranchId, EvidenceId, FindingId, MissionId, ProjectId, RunId, TaskId};
    use serde_json::Value;
    use tempfile::TempDir;

    use models::{AgentNarrativeEventId, AgentNarrativeEventKind, CreateAgentNarrativeRequest};
    use serde_json::Map as JsonMap;

    fn timestamp() -> Timestamp {
        "2026-08-24T12:00:00.123456Z"
            .parse()
            .unwrap_or_else(|error| panic!("固定时间必须可解析: {error}"))
    }

    fn temp_repo() -> (TempDir, SqliteRepository) {
        let dir = tempfile::tempdir().expect("系统临时目录应可创建");
        let repo = SqliteRepository::open(dir.path().join("test.sqlite3")).expect("库必须可打开");
        (dir, repo)
    }

    #[test]
    fn delete_worker_run_removes_only_the_target_row() {
        let (_dir, repo) = temp_repo();
        let project = ProjectId::new("proj_1".to_string());
        let mut loser = WorkerRun::new(
            project.clone(),
            WorkerRuntimeType::Pi,
            "swarm attempt that lost",
        );
        loser.id = "wkrun_loser".to_string();
        let mut winner = WorkerRun::new(
            project.clone(),
            WorkerRuntimeType::ClaudeCode,
            "swarm attempt that won",
        );
        winner.id = "wkrun_winner".to_string();
        repo.upsert_worker_run(&loser).expect("落败行可写");
        repo.upsert_worker_run(&winner).expect("获胜行可写");

        assert!(
            repo.delete_worker_run("wkrun_loser").expect("删除可执行"),
            "存在的行必须报 true"
        );
        assert!(
            !repo.delete_worker_run("wkrun_loser").expect("重复删除可执行"),
            "已删除的行再删必须报 false（幂等）"
        );
        assert!(
            repo.get_worker_run("wkrun_loser")
                .expect("可读")
                .is_none(),
            "落败行必须真的消失"
        );
        assert!(
            repo.get_worker_run("wkrun_winner")
                .expect("可读")
                .is_some(),
            "获胜行必须不受影响"
        );
    }

    fn mission(id: &str, project_id: &str) -> Mission {
        let mut mission = Mission::new(ProjectId::new(project_id.to_string()), "goal".to_string());
        mission.id = MissionId::new(id.to_string());
        mission.created_at = timestamp();
        mission.updated_at = timestamp();
        mission
    }

    fn task(run_id: &str, task_id: &str) -> AgentTask {
        let mut task = AgentTask::new(
            ProjectId::new("proj_1".to_string()),
            RunId::new(run_id.to_string()),
            "web_sast".to_string(),
        );
        task.id = TaskId::new(task_id.to_string());
        task.created_at = timestamp();
        task
    }

    fn evidence(id: &str, project_id: &str) -> Evidence {
        let mut evidence = Evidence::new(
            ProjectId::new(project_id.to_string()),
            EvidenceKind::ToolOutput,
            "summary".to_string(),
        );
        evidence.id = EvidenceId::new(id.to_string());
        evidence.created_at = timestamp();
        evidence
    }

    fn finding(id: &str, project_id: &str, evidence_id: &str) -> Finding {
        let mut finding = Finding::new(ProjectId::new(project_id.to_string()), "title".to_string());
        finding.id = FindingId::new(id.to_string());
        finding.created_at = timestamp();
        finding.updated_at = timestamp();
        finding.status = FindingStatus::Confirmed;
        finding.evidence_ids = vec![evidence_id.to_string()];
        finding
    }

    #[test]
    fn dashboard_stats_aggregates_five_cards() {
        let (_dir, repo) = temp_repo();

        // 卡片一：missions —— 2 running / 1 paused / 1 waiting + 2 非活跃。
        let mission_cases = [
            ("m_dash_run_1", MissionStatus::Running),
            ("m_dash_run_2", MissionStatus::Running),
            ("m_dash_paused", MissionStatus::Paused),
            ("m_dash_wait", MissionStatus::WaitingForDecision),
            ("m_dash_done", MissionStatus::Completed),
            ("m_dash_draft", MissionStatus::Draft),
        ];
        for (id, status) in mission_cases {
            let mut item = mission(id, "proj_dash");
            item.status = status;
            repo.create_mission(&item).expect("mission 必须可插入");
        }

        // 卡片二：Confirmed findings 按 severity 拆分 + 1 个 candidate 不计入。
        let mut critical = finding("find_dash_crit", "proj_dash", "evd_dash_a");
        critical.severity = models::Severity::Critical;
        let mut high = finding("find_dash_high", "proj_dash", "evd_dash_a");
        high.severity = models::Severity::High;
        let medium = finding("find_dash_medium", "proj_dash", "evd_dash_a");
        let mut candidate = finding("find_dash_cand", "proj_dash", "evd_dash_a");
        candidate.status = FindingStatus::Candidate;
        for f in [critical, high, medium, candidate] {
            repo.add_finding(&f).expect("finding 必须可插入");
        }

        // 卡片三：资产节点 —— url×2 + ip×1，url 一条重复归一值（去重后 1）。
        let url_a = asset("asset_dash_a", "https://Example.COM/a/", MissionAssetSensitivity::Unknown);
        let url_dup = asset("asset_dash_dup", "https://example.com/a/", MissionAssetSensitivity::Unknown);
        let mut ip = asset("asset_dash_ip", "10.0.0.1", MissionAssetSensitivity::Sensitive);
        ip.asset_type = MissionAssetType::Ip;
        repo.upsert_mission_asset(&url_a).expect("资产必须可插入");
        repo.upsert_mission_asset(&url_dup).expect("重复资产必须合并");
        repo.upsert_mission_asset(&ip).expect("资产必须可插入");

        // 卡片四：工具调用 + 在途 worker（1 running + 1 succeeded）。
        repo.add_tool_invocation(&ToolInvocation::new("semgrep".to_string(), "scan".to_string()))
            .expect("inv 必须可插入");
        repo.add_tool_invocation(&ToolInvocation::new("nuclei".to_string(), "scan".to_string()))
            .expect("inv 必须可插入");
        let mut active_run = WorkerRun::new(
            ProjectId::new("proj_dash".to_string()),
            WorkerRuntimeType::ClaudeCode,
            "dash instruction",
        );
        active_run.mark_started();
        repo.upsert_worker_run(&active_run).expect("running run 必须可插入");
        let mut done_run = WorkerRun::new(
            ProjectId::new("proj_dash".to_string()),
            WorkerRuntimeType::Codex,
            "dash instruction",
        );
        done_run.finish(models::WorkerRunStatus::Succeeded, None);
        repo.upsert_worker_run(&done_run).expect("done run 必须可插入");

        // 卡片五：真实 usage —— active_run 附带 input/output/cached。
        active_run.usage = Some(models::WorkerUsage {
            input_tokens: 100,
            output_tokens: 40,
            cached_input_tokens: 60,
            reasoning_tokens: 0,
            cost_usd: Some(0.5),
            num_turns: None,
            duration_api_ms: None,
            requested_model: None,
        });
        repo.upsert_worker_run(&active_run).expect("usage run 必须可更新");

        let stats = repo.dashboard_stats().expect("聚合必须成功");
        assert_eq!(stats.missions.running, 2);
        assert_eq!(stats.missions.paused, 1);
        assert_eq!(stats.missions.waiting_for_decision, 1);
        assert_eq!(stats.missions.total, 4);

        assert_eq!(stats.confirmed_findings.critical, 1);
        assert_eq!(stats.confirmed_findings.high, 1);
        assert_eq!(stats.confirmed_findings.medium, 1);
        assert_eq!(stats.confirmed_findings.total, 3, "candidate 不得计入 confirmed");

        assert_eq!(stats.asset_nodes.distinct_count, 2, "归一化后 url 去重为 1 + ip 1");
        assert_eq!(stats.asset_nodes.by_type.len(), 2);
        assert_eq!(stats.asset_nodes.by_type[0].count, 1);

        assert_eq!(stats.tool_calls.total_invocations, 2);
        assert_eq!(stats.tool_calls.active_worker_runs, 1);

        assert_eq!(stats.token_usage.reported_runs, 1);
        assert_eq!(stats.token_usage.input_tokens, 100);
        assert_eq!(stats.token_usage.output_tokens, 40);
        assert_eq!(stats.token_usage.cached_input_tokens, 60);
        assert_eq!(stats.token_usage.cost_usd, Some(0.5));

        // 成本红线空态：全新空库 reported_runs == 0，聚合仍成功。
        let (empty_dir, empty_repo) = temp_repo();
        drop(empty_dir);
        let empty = empty_repo.dashboard_stats().expect("空库聚合必须成功");
        assert_eq!(empty.token_usage.reported_runs, 0);
        assert_eq!(empty.asset_nodes.distinct_count, 0);
    }

    #[test]
    fn worker_usage_breakdown_grouped_daily_by_dimension() {
        let (_dir, repo) = temp_repo();

        let make_usage_run = |id: &str, runtime: WorkerRuntimeType, model: Option<&str>, input: i64, cached: i64, output: i64| {
            let mut run = WorkerRun::new(
                ProjectId::new("proj_usage".to_string()),
                runtime,
                "usage instruction",
            );
            run.id = id.to_string();
            run.model = model.map(str::to_string);
            run.usage = Some(models::WorkerUsage {
                input_tokens: input,
                output_tokens: output,
                cached_input_tokens: cached,
                reasoning_tokens: 0,
                cost_usd: None,
                num_turns: None,
                duration_api_ms: None,
                requested_model: None,
            });
            run
        };

        repo.upsert_worker_run(&make_usage_run("wkrun_g1", WorkerRuntimeType::ClaudeCode, Some("claude-sonnet-5"), 100, 50, 20))
            .expect("usage run 1 必须可插入");
        repo.upsert_worker_run(&make_usage_run("wkrun_g2", WorkerRuntimeType::ClaudeCode, Some("claude-sonnet-5"), 30, 0, 10))
            .expect("usage run 2 必须可插入");
        repo.upsert_worker_run(&make_usage_run("wkrun_g3", WorkerRuntimeType::Codex, Some("gpt-main"), 70, 10, 5))
            .expect("usage run 3 必须可插入");

        // 按 runtime：claude_code 一组（130/50/30），codex 一组（70/10/5）。
        let by_runtime = repo
            .sum_worker_usage_breakdown(None, None, Some(models::WorkerUsageDimension::Runtime))
            .expect("runtime 分组聚合必须成功");
        assert_eq!(by_runtime.grouped_daily.len(), 2);
        let claude = by_runtime
            .grouped_daily
            .iter()
            .find(|point| point.key.as_deref() == Some("claude_code"))
            .expect("claude_code 分组必须存在");
        assert_eq!(claude.runs, 2);
        assert_eq!(claude.input_tokens, 130);
        assert_eq!(claude.cached_input_tokens, 50);
        assert_eq!(claude.output_tokens, 30);

        // 按模型：同口径换分组列。
        let by_model = repo
            .sum_worker_usage_breakdown(None, None, Some(models::WorkerUsageDimension::Model))
            .expect("model 分组聚合必须成功");
        assert_eq!(by_model.grouped_daily.len(), 2);
        let gpt = by_model
            .grouped_daily
            .iter()
            .find(|point| point.key.as_deref() == Some("gpt-main"))
            .expect("gpt-main 分组必须存在");
        assert_eq!(gpt.runs, 1);
        assert_eq!(gpt.input_tokens, 70);

        // 不分组：grouped_daily 为空，daily 仍为单系列。
        let ungrouped = repo
            .sum_worker_usage_breakdown(None, None, None)
            .expect("不分组聚合必须成功");
        assert!(ungrouped.grouped_daily.is_empty());
        let total_input: i64 = ungrouped.daily.iter().map(|point| point.input_tokens).sum();
        assert_eq!(total_input, 200);
    }

    #[test]
    fn runtime_settings_use_scope_precedence_and_normalized_keys() {
        let (_dir, repo) = temp_repo();
        let global = RuntimeSetting::new(
            " Example_Key ",
            serde_json::json!({"ui_locale": "en-US"})
                .as_object()
                .cloned()
                .expect("object literal"),
        )
        .expect("key must normalize");
        repo.upsert_runtime_setting(&global)
            .expect("global setting must persist");

        let mut project = global.clone();
        project.id = models::RuntimeSettingId::new("setting_project".to_string());
        project.project_id = Some(ProjectId::new("proj_1".to_string()));
        project.scope = models::RuntimeSettingScope::Project;
        project.value = serde_json::json!({"ui_locale": "zh-CN"})
            .as_object()
            .cloned()
            .expect("object literal");
        project.updated_at = timestamp();
        repo.upsert_runtime_setting(&project)
            .expect("project setting must persist");

        let resolved = repo
            .get_runtime_setting("EXAMPLE_KEY", Some("proj_1"), None)
            .expect("lookup must succeed")
            .expect("project setting must resolve");
        assert_eq!(
            resolved.value.get("ui_locale").and_then(Value::as_str),
            Some("zh-CN")
        );
        let fallback = repo
            .get_runtime_setting("example_key", Some("other"), None)
            .expect("fallback lookup must succeed")
            .expect("global setting must resolve");
        assert_eq!(
            fallback.value.get("ui_locale").and_then(Value::as_str),
            Some("en-US")
        );
        repo.delete_runtime_setting("example_key", Some("proj_1"), None)
            .expect("delete must succeed");
        assert!(
            repo.get_runtime_setting("example_key", Some("proj_1"), None)
                .expect("post-delete lookup must succeed")
                .is_some(),
            "global fallback remains after scoped delete"
        );
    }

    #[test]
    fn execution_jobs_roundtrip_filter_and_require_existing_updates() {
        let (_dir, repo) = temp_repo();
        let mut request = ExecutionRequest::new("scanner".to_string());
        request.id = ExecutionId::new("exec_1".to_string());
        request.project_id = Some(ProjectId::new("proj_1".to_string()));
        request.run_id = Some(RunId::new("run_1".to_string()));
        request.created_at = timestamp();
        let mut job = ExecutionJob::queued(request, "digest".to_string());
        job.submitted_at = timestamp();
        repo.create_execution_job(&job)
            .expect("execution job must persist");

        assert_eq!(
            repo.get_execution_job("exec_1")
                .expect("lookup must succeed")
                .expect("job must exist"),
            job
        );
        assert_eq!(
            repo.list_execution_jobs(Some("proj_1"), Some("run_1"), Some(ExecutionStatus::Queued))
                .expect("filtered list must succeed")
                .len(),
            1
        );

        job.status = ExecutionStatus::Running;
        repo.update_execution_job(&job)
            .expect("existing job must update");
        assert!(
            repo.list_execution_jobs(None, None, Some(ExecutionStatus::Queued))
                .expect("status filter must succeed")
                .is_empty()
        );

        let mut missing = job;
        missing.id = ExecutionId::new("exec_missing".to_string());
        let error = repo
            .update_execution_job(&missing)
            .expect_err("update must not insert a missing execution");
        assert!(matches!(error, StorageError::NotFound { .. }));
    }

    #[test]
    fn create_and_get_mission_roundtrips() {
        let (_dir, repo) = temp_repo();
        let created = mission("mission_1", "proj_1");
        repo.create_mission(&created).expect("create 必须成功");
        let fetched = repo
            .get_mission("mission_1")
            .expect("get 必须成功")
            .expect("刚插入的 mission 必须存在");
        assert_eq!(fetched, created, "payload 往返不得改变实体");
    }

    #[test]
    fn get_missing_mission_returns_none() {
        let (_dir, repo) = temp_repo();
        let fetched = repo.get_mission("absent").expect("查询必须成功");
        assert!(fetched.is_none(), "不存在的 id 必须返回 None");
    }

    #[test]
    fn duplicate_mission_insert_fails() {
        let (_dir, repo) = temp_repo();
        let first = mission("mission_dup", "proj_1");
        repo.create_mission(&first).expect("首次插入必须成功");
        let error = repo
            .create_mission(&first)
            .expect_err("重复 id 必须违反 UNIQUE 约束");
        assert!(
            matches!(error, StorageError::Write { .. }),
            "约束冲突必须作为写错误报告: {error:?}"
        );
    }

    #[test]
    fn list_missions_scopes_by_project() {
        let (_dir, repo) = temp_repo();
        for (id, project) in [("m_a", "p1"), ("m_b", "p1"), ("m_c", "p2")] {
            repo.create_mission(&mission(id, project))
                .expect("插入必须成功");
        }
        let p1 = repo
            .list_missions(Some("p1"))
            .expect("查询必须成功")
            .into_iter()
            .map(|mission| mission.id.as_str().to_string())
            .collect::<Vec<_>>();
        assert_eq!(p1, ["m_a", "m_b"], "project 过滤 + seq 升序");
        let all = repo.list_missions(None).expect("查询必须成功");
        assert_eq!(all.len(), 3, "None 列全部");
    }

    #[test]
    fn update_mission_upserts_in_place() {
        let (_dir, repo) = temp_repo();
        let created = mission("mission_up", "proj_1");
        repo.create_mission(&created).expect("create 必须成功");
        let mut updated = created;
        updated.status = MissionStatus::Paused;
        updated.updated_at = timestamp();
        repo.update_mission(&updated).expect("update 必须成功");
        let fetched = repo
            .get_mission("mission_up")
            .expect("get 必须成功")
            .expect("upsert 不得删除行");
        assert_eq!(fetched.status, MissionStatus::Paused);
        assert_eq!(
            repo.list_missions(Some("proj_1"))
                .expect("查询必须成功")
                .len(),
            1,
            "upsert 不得产生第二行"
        );
    }

    #[test]
    fn update_mission_without_prior_row_inserts() {
        let (_dir, repo) = temp_repo();
        let fresh = mission("mission_fresh", "proj_1");
        repo.update_mission(&fresh).expect("upsert 必须可插入新行");
        assert_eq!(
            repo.get_mission("mission_fresh")
                .expect("get 必须成功")
                .expect("行必须存在"),
            fresh
        );
    }

    #[test]
    fn delete_mission_removes_row() {
        let (_dir, repo) = temp_repo();
        repo.create_mission(&mission("mission_del", "proj_1"))
            .expect("create 必须成功");

        // 该 mission 的 evidence / finding 必须被级联删掉。
        let mut ev = evidence("ev_del", "proj_1");
        ev.mission_id = Some(MissionId::new("mission_del".to_string()));
        repo.add_evidence(&ev).expect("evidence 必须可插入");
        let mut f = finding("find_del", "proj_1", "ev_del");
        f.mission_id = Some(MissionId::new("mission_del".to_string()));
        repo.add_finding(&f).expect("finding 必须可插入");
        // 别的 mission 的 finding 不受影响。
        let mut keep = finding("find_keep", "proj_1", "ev_del");
        keep.mission_id = Some(MissionId::new("mission_other".to_string()));
        repo.add_finding(&keep).expect("finding 必须可插入");
        // 外部 worker 审计链：该 mission 的 worker_run + 挂在它下面的
        // invocation 都必须被级联删；别的 mission 的 worker_run 不动。
        let mut loser = WorkerRun::new(
            ProjectId::new("proj_1".to_string()),
            WorkerRuntimeType::Pi,
            "attempt for the deleted mission",
        );
        loser.id = "wkrun_del".to_string();
        loser.mission_id = Some(MissionId::new("mission_del".to_string()));
        repo.upsert_worker_run(&loser).expect("worker_run 必须可插入");
        let mut invocation = WorkerInvocation::new(
            WorkerRuntimeType::Pi,
            WorkerInvocationPurpose::Start,
            Some("wkrun_del".to_string()),
        );
        invocation.id = "wkinv_del".to_string();
        invocation.project_id = Some(ProjectId::new("proj_1".to_string()));
        invocation.status = WorkerRunStatus::Succeeded;
        repo.upsert_worker_invocation(&invocation)
            .expect("worker_invocation 必须可插入");
        let mut other = WorkerRun::new(
            ProjectId::new("proj_1".to_string()),
            WorkerRuntimeType::Codex,
            "attempt for another mission",
        );
        other.id = "wkrun_keep".to_string();
        other.mission_id = Some(MissionId::new("mission_other".to_string()));
        repo.upsert_worker_run(&other).expect("worker_run 必须可插入");

        repo.delete_mission("mission_del").expect("delete 必须成功");

        assert!(
            repo.get_worker_run("wkrun_del")
                .expect("查询必须成功")
                .is_none(),
            "mission 的 worker_run 必须被级联删"
        );
        assert!(
            repo.list_worker_invocations(Some("wkrun_del"), None, 10)
                .expect("查询必须成功")
                .is_empty(),
            "挂在被删 worker_run 上的 invocation 必须被级联删"
        );
        assert!(
            repo.get_worker_run("wkrun_keep")
                .expect("查询必须成功")
                .is_some(),
            "别的 mission 的 worker_run 不受影响"
        );

        repo.delete_mission("mission_del").expect("delete 必须成功");

        assert!(
            repo.get_mission("mission_del")
                .expect("查询必须成功")
                .is_none()
        );
        let findings = repo.list_findings("proj_1").expect("list findings");
        assert!(
            findings.iter().all(|f| f.id.as_str() != "find_del"),
            "mission 的 finding 必须被级联删"
        );
        assert!(
            findings.iter().any(|f| f.id.as_str() == "find_keep"),
            "别的 mission 的 finding 必须保留"
        );
        let evidence = repo.list_evidence("proj_1").expect("list evidence");
        assert!(
            evidence.iter().all(|e| e.id.as_str() != "ev_del"),
            "mission 的 evidence 必须被级联删"
        );
    }

    #[test]
    fn branch_and_task_run_scope_columns() {
        let (_dir, repo) = temp_repo();
        let mut branch = Branch::new(
            ProjectId::new("proj_1".to_string()),
            MissionId::new("mission_1".to_string()),
            "title".to_string(),
            "hypothesis".to_string(),
        );
        branch.id = BranchId::new("br_1".to_string());
        repo.create_branch(&branch).expect("branch 必须可插入");

        repo.create_task(&task("run_a", "task_1"))
            .expect("task_1 必须可插入");
        let mut other = task("run_b", "task_2");
        other.mission_id = Some(MissionId::new("mission_1".to_string()));
        repo.create_task(&other).expect("task_2 必须可插入");

        // run_id 列正确写入的语义证据：list_tasks 按 run_id 列过滤。
        let run_a_tasks = repo.list_tasks("run_a").expect("查询必须成功");
        assert_eq!(run_a_tasks.len(), 1, "run_a 只含 task_1");
        assert_eq!(run_a_tasks[0].id.as_str(), "task_1");
    }

    #[test]
    fn list_branches_filters_mission_id_in_payload() {
        let (_dir, repo) = temp_repo();
        for (id, mission_id) in [("br_1", "mission_a"), ("br_2", "mission_b")] {
            let mut branch = Branch::new(
                ProjectId::new("proj_1".to_string()),
                MissionId::new(mission_id.to_string()),
                format!("title {id}"),
                "hypothesis".to_string(),
            );
            branch.id = BranchId::new(id.to_string());
            repo.create_branch(&branch).expect("branch 必须可插入");
        }
        let filtered = repo
            .list_branches(Some("proj_1"), Some("mission_b"), None)
            .expect("查询必须成功");
        assert_eq!(filtered.len(), 1, "mission_id 过滤只留 mission_b 分支");
        assert_eq!(filtered[0].id.as_str(), "br_2");
    }

    #[test]
    fn increment_run_steps_semantics() {
        let (_dir, repo) = temp_repo();
        let mut run = AuditRun::new(ProjectId::new("proj_1".to_string()));
        run.id = RunId::new("run_steps".to_string());
        run.steps_used = 3;
        repo.create_run(&run).expect("run 必须可插入");

        let updated = repo
            .increment_run_steps("run_steps", 2)
            .expect("递增必须成功")
            .expect("存在的 run 必须返回");
        assert_eq!(updated.steps_used, 5, "3 + 2 = 5");

        let missing = repo.increment_run_steps("absent", 1).expect("查询必须成功");
        assert!(missing.is_none(), "不存在的 run 返回 None");

        let negative = repo
            .increment_run_steps("run_steps", -1)
            .expect_err("负 delta 必须报错");
        assert!(
            matches!(negative, StorageError::NegativeStepDelta { delta: -1 }),
            "负 delta 报 NegativeStepDelta: {negative:?}"
        );
    }

    #[test]
    fn append_run_task_dedupes() {
        let (_dir, repo) = temp_repo();
        let mut run = AuditRun::new(ProjectId::new("proj_1".to_string()));
        run.id = RunId::new("run_append".to_string());
        repo.create_run(&run).expect("run 必须可插入");

        repo.append_run_task("run_append", "task_x")
            .expect("追加必须成功")
            .expect("存在的 run 必须返回");
        repo.append_run_task("run_append", "task_x")
            .expect("重复追加必须成功")
            .expect("存在的 run 必须返回");

        let fetched = repo
            .get_run("run_append")
            .expect("查询必须成功")
            .expect("run 必须存在");
        assert_eq!(fetched.task_ids, ["task_x"], "重复 task 不得二次追加");
    }

    #[test]
    fn tool_invocation_created_at_column_uses_started_at() {
        let (dir, repo) = temp_repo();
        let inv = ToolInvocation::new("semgrep".to_string(), "scan".to_string());
        repo.add_tool_invocation(&inv).expect("inv 必须可插入");
        let inv_id = inv.id.as_str().to_string();
        let expected = inv.started_at.isoformat();
        drop(repo);

        let connection =
            Connection::open(dir.path().join("test.sqlite3")).expect("落盘文件必须可读");
        let created_at: String = connection
            .query_row(
                "SELECT created_at FROM tool_invocations WHERE id = ?",
                params![inv_id],
                |row| row.get(0),
            )
            .expect("created_at 列必须存在");
        assert_eq!(
            created_at, expected,
            "ToolInvocation 无 created_at 字段，列必须回退 started_at.isoformat()"
        );
    }

    #[test]
    fn evidence_and_finding_roundtrip() {
        let (_dir, repo) = temp_repo();
        let evidence = evidence("evd_1", "proj_1");
        repo.add_evidence(&evidence).expect("evidence 必须可插入");
        let listed = repo.list_evidence("proj_1").expect("查询必须成功");
        assert_eq!(listed, [evidence]);

        let finding = finding("find_1", "proj_1", "evd_1");
        repo.add_finding(&finding).expect("finding 必须可插入");
        assert_eq!(
            repo.get_finding("find_1")
                .expect("查询必须成功")
                .expect("finding 必须存在"),
            finding
        );

        let mut updated = finding;
        updated.title = "Eval injection (confirmed)".to_string();
        repo.update_finding(&updated).expect("update 必须成功");
        let findings = repo.list_findings("proj_1").expect("查询必须成功");
        assert_eq!(findings.len(), 1, "upsert 不得产生第二行");
        assert_eq!(findings[0].title, "Eval injection (confirmed)");
    }

    #[test]
    fn run_and_task_status_updates_roundtrip() {
        let (_dir, repo) = temp_repo();
        let mut run = AuditRun::new(ProjectId::new("proj_1".to_string()));
        run.id = RunId::new("run_status".to_string());
        run.status = RunStatus::Running;
        repo.create_run(&run).expect("run 必须可插入");

        let mut task = task("run_status", "task_status");
        task.status = TaskStatus::Succeeded;
        repo.create_task(&task).expect("task 必须可插入");

        let mut run_paused = run;
        run_paused.status = RunStatus::Paused;
        run_paused.updated_at = timestamp();
        repo.update_run(&run_paused).expect("update_run 必须成功");
        assert_eq!(
            repo.get_run("run_status")
                .expect("查询必须成功")
                .expect("run 必须存在")
                .status,
            RunStatus::Paused
        );

        let mut task_failed = task;
        task_failed.status = TaskStatus::Failed;
        task_failed.error = Some("solver crashed".to_string());
        repo.update_task(&task_failed)
            .expect("update_task 必须成功");
        let fetched = repo
            .get_task("task_status")
            .expect("查询必须成功")
            .expect("task 必须存在");
        assert_eq!(fetched.status, TaskStatus::Failed);
        assert_eq!(fetched.error.as_deref(), Some("solver crashed"));
    }

    #[test]
    fn concurrent_access_is_serialized() {
        let (_dir, repo) = temp_repo();
        let repo = std::sync::Arc::new(repo);
        let handles: Vec<_> = (0..8)
            .map(|index| {
                let repo = std::sync::Arc::clone(&repo);
                std::thread::spawn(move || {
                    let id = format!("mission_{index}");
                    repo.create_mission(&mission(&id, "proj_concurrent"))
                        .expect("并发插入必须被串行化而不是死锁/失败");
                })
            })
            .collect();
        for handle in handles {
            handle.join().expect("工作线程不得 panic");
        }
        let all = repo
            .list_missions(Some("proj_concurrent"))
            .expect("查询必须成功");
        assert_eq!(all.len(), 8, "全部并发写入必须落库");
    }

    // -- providers / routes / model invocations（test_provider_runtime_core.py 语义） --

    fn provider(id: &str, name: &str, provider_type: ProviderType) -> ProviderConfig {
        let mut config = ProviderConfig::new(name.to_string(), provider_type);
        config.id = models::ProviderId::new(id.to_string());
        config
    }

    #[test]
    fn provider_crud_and_default_resolution() {
        let (_dir, repo) = temp_repo();
        let mut p1 = provider("provider_1", "one", ProviderType::OpenaiCompatible);
        p1.is_default = true;
        let mut p2 = provider("provider_2", "two", ProviderType::Ollama);
        p2.is_default = true;
        repo.create_provider(&p1).expect("p1 必须可插入");
        repo.create_provider(&p2).expect("p2 必须可插入");

        // Python 语义：新默认位抢占旧默认位。
        let by_id: std::collections::HashMap<String, ProviderConfig> = repo
            .list_providers()
            .expect("列全部必须成功")
            .into_iter()
            .map(|p| (p.id.as_str().to_string(), p))
            .collect();
        assert!(!by_id["provider_1"].is_default, "旧默认位必须被清空");
        assert!(by_id["provider_2"].is_default);
        assert!(
            repo.get_provider("provider_1")
                .expect("查询必须成功")
                .is_some()
        );

        p2.enabled = false;
        repo.update_provider(&p2).expect("更新必须成功");
        assert!(
            !repo
                .get_provider("provider_2")
                .expect("查询必须成功")
                .expect("provider 必须存在")
                .enabled
        );

        repo.delete_provider("provider_1").expect("删除必须成功");
        assert!(
            repo.get_provider("provider_1")
                .expect("查询必须成功")
                .is_none()
        );
    }

    #[test]
    fn model_invocation_roundtrip() {
        let (_dir, repo) = temp_repo();
        let mut inv = ModelInvocation::new(
            models::ProviderId::new("provider_a".to_string()),
            ProviderType::OpenaiCompatible,
            "test".to_string(),
        );
        inv.id = models::ModelInvocationId::new("model_1".to_string());
        inv.project_id = Some(ProjectId::new("proj_a".to_string()));
        inv.prompt_summary = "short".to_string();
        inv.response_summary = "ok".to_string();
        inv.status = ModelInvocationStatus::Ok;
        repo.add_model_invocation(&inv).expect("inv 必须可插入");

        let by_project = repo
            .list_model_invocations(Some("proj_a"))
            .expect("按 project 过滤必须成功");
        assert_eq!(by_project.len(), 1);
        assert_eq!(by_project[0].id, inv.id);
        let all = repo.list_model_invocations(None).expect("列全部必须成功");
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].id, inv.id);
    }

    #[test]
    fn model_capability_upsert_and_provider_filter() {
        let (_dir, repo) = temp_repo();
        let capability = ModelCapability {
            id: models::ModelCapabilityId::new("modelcap_1".to_string()),
            provider_id: models::ProviderId::new("provider_a".to_string()),
            model: "glm-4.5".to_string(),
            context_window: Some(128_000),
            max_output_tokens: Some(8_192),
            supports_json: true,
            supports_tools: true,
            supports_vision: false,
            supports_embeddings: false,
            provider_native_tools: vec!["web_search".to_string()],
            metadata: serde_json::Map::new(),
            created_at: models::utcnow(),
            updated_at: models::utcnow(),
        };
        repo.upsert_model_capability(&capability)
            .expect("capability must be stored");
        assert_eq!(
            repo.list_model_capabilities(Some("provider_a"))
                .expect("provider filter must work"),
            vec![capability.clone()]
        );
        assert!(
            repo.list_model_capabilities(Some("provider_missing"))
                .expect("unknown provider filter must return empty")
                .is_empty()
        );
    }

    #[test]
    fn provider_routes_filter_normalize_and_order() {
        let (_dir, repo) = temp_repo();
        let provider_id = models::ProviderId::new("provider_r".to_string());
        let low =
            ProviderRouteBinding::new("advisor", provider_id.clone()).expect("路由必须可构造");
        let mut low = low;
        low.id = models::ProviderRouteId::new("route_low".to_string());
        low.priority = 10;
        let high =
            ProviderRouteBinding::new("advisor", provider_id.clone()).expect("路由必须可构造");
        let mut high = high;
        high.id = models::ProviderRouteId::new("route_high".to_string());
        high.priority = 100;
        let other = ProviderRouteBinding::new("solver", provider_id).expect("路由必须可构造");
        let mut other = other;
        other.id = models::ProviderRouteId::new("route_other".to_string());

        repo.upsert_provider_route(&low).expect("low 必须可写入");
        repo.upsert_provider_route(&high).expect("high 必须可写入");
        repo.upsert_provider_route(&other)
            .expect("other 必须可写入");

        // purpose 过滤 + (priority, weight) 降序。
        let advisor = repo
            .list_provider_routes(Some("  ADVISOR "))
            .expect("purpose 过滤必须成功");
        assert_eq!(
            advisor.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            ["route_high", "route_low"],
            "过滤归一化 + 降序排序"
        );
        let all = repo.list_provider_routes(None).expect("列全部必须成功");
        assert_eq!(all.len(), 3);

        assert!(
            repo.get_provider_route("route_high")
                .expect("查询必须成功")
                .is_some()
        );
        repo.delete_provider_route("route_high")
            .expect("删除必须成功");
        assert!(
            repo.get_provider_route("route_high")
                .expect("查询必须成功")
                .is_none()
        );
    }

    #[test]
    fn update_provider_promotes_default_and_clears_previous() {
        let (_dir, repo) = temp_repo();
        let p1 = provider("provider_d1", "one", ProviderType::Ollama);
        let mut p2 = provider("provider_d2", "two", ProviderType::Ollama);
        repo.create_provider(&p1).expect("p1 必须可插入");
        repo.create_provider(&p2).expect("p2 必须可插入");

        p2.is_default = true;
        repo.update_provider(&p2).expect("提升默认必须成功");
        let fetched_p1 = repo
            .get_provider("provider_d1")
            .expect("查询必须成功")
            .expect("p1 必须存在");
        assert!(!fetched_p1.is_default, "update 路径同样清空旧默认位");
        let fetched_p2 = repo
            .get_provider("provider_d2")
            .expect("查询必须成功")
            .expect("p2 必须存在");
        assert!(fetched_p2.is_default);
    }

    // ------------------------------------------------------------------
    // M5b：projects / facts / intents / events / 收口链 / modules
    // ------------------------------------------------------------------

    fn project_named(id: &str) -> Project {
        let mut project = Project::new("test project".to_string(), AuditDomain::WebDast);
        project.id = ProjectId::new(id.to_string());
        project.created_at = timestamp();
        project.updated_at = timestamp();
        project
    }

    fn fact_of(id: &str, project_id: &str, statement: &str) -> Fact {
        let mut fact = Fact::new(
            ProjectId::new(project_id.to_string()),
            "discovered".to_string(),
            statement.to_string(),
        );
        fact.id = FactId::new(id.to_string());
        fact.created_at = timestamp();
        fact
    }

    fn intent_of(project_id: &str, title: &str) -> Intent {
        let mut intent = Intent::new(ProjectId::new(project_id.to_string()), title.to_string());
        intent.id = IntentId::new("intent_1".to_string());
        intent.created_at = timestamp();
        intent.updated_at = timestamp();
        intent
    }

    fn event_of(id: &str, project_id: &str, run_id: Option<&str>) -> AuditEvent {
        let mut event = AuditEvent::new(
            ProjectId::new(project_id.to_string()),
            AuditEventType::RunStarted,
            "manager".to_string(),
            "run started".to_string(),
        );
        event.id = id.to_string();
        event.created_at = timestamp();
        if let Some(run_id) = run_id {
            event.run_id = Some(RunId::new(run_id.to_string()));
        }
        event
    }

    #[test]
    fn project_crud_and_cascade_delete() {
        let (_dir, repo) = temp_repo();
        let project = project_named("proj_cascade");
        repo.create_project(&project).expect("create 必须成功");
        assert_eq!(
            repo.get_project("proj_cascade")
                .expect("查询必须成功")
                .expect("project 必须存在"),
            project,
            "payload 往返不得改变实体"
        );
        let other = project_named("proj_other");
        repo.create_project(&other)
            .expect("第二个 project 必须可插入");
        assert_eq!(repo.list_projects().expect("列全部必须成功").len(), 2);

        let mut updated = project.clone();
        updated.name = "renamed".to_string();
        repo.update_project(&updated).expect("upsert 必须成功");
        assert_eq!(
            repo.get_project("proj_cascade")
                .expect("查询必须成功")
                .expect("project 必须存在")
                .name,
            "renamed"
        );

        // 级联：mission / fact / event 都带该 project_id，删除必须一并清空，
        // 其他 project 的行不受影响。
        let m = mission("m_cascade", "proj_cascade");
        repo.create_mission(&m).expect("mission 必须可插入");
        repo.add_fact(&fact_of("fact_c", "proj_cascade", "f"))
            .expect("fact 必须可插入");
        repo.add_event(&event_of("event_c1", "proj_cascade", None))
            .expect("event 必须可插入");
        repo.add_event(&event_of("event_o1", "proj_other", None))
            .expect("其他 project 的 event 必须可插入");

        repo.delete_project("proj_cascade")
            .expect("级联删除必须成功");
        assert!(
            repo.get_project("proj_cascade")
                .expect("查询必须成功")
                .is_none(),
            "project 行本身必须被删除"
        );
        assert!(
            repo.get_mission("m_cascade")
                .expect("查询必须成功")
                .is_none(),
            "mission 必须被级联清空"
        );
        assert!(
            repo.list_facts("proj_cascade")
                .expect("查询必须成功")
                .is_empty(),
            "facts 必须被级联清空"
        );
        assert!(
            repo.list_events("proj_cascade", None, 200, None)
                .expect("查询必须成功")
                .is_empty(),
            "events 必须被级联清空"
        );
        assert_eq!(
            repo.list_events("proj_other", None, 200, None)
                .expect("查询必须成功")
                .len(),
            1,
            "其他 project 的行不受级联影响"
        );
    }

    #[test]
    fn fact_append_only_conflict_and_list() {
        let (_dir, repo) = temp_repo();
        let fact = fact_of("fact_1", "proj_f", "target resolves to 203.0.113.10");
        repo.add_fact(&fact).expect("首次追加必须成功");
        let error = repo
            .add_fact(&fact)
            .expect_err("append-only 实体重复 id 必须报错");
        assert!(
            matches!(
                &error,
                StorageError::AppendOnlyConflict { entity, id }
                    if entity == "fact" && id == "fact_1"
            ),
            "错误必须是 Python ValueError 的类型化对应: {error:?}"
        );

        let other = fact_of("fact_2", "proj_f", "another fact");
        repo.add_fact(&other).expect("不同 id 必须可追加");
        let foreign = fact_of("fact_3", "proj_g", "foreign");
        repo.add_fact(&foreign).expect("其他 project 必须可追加");

        assert_eq!(
            repo.list_facts("proj_f").expect("查询必须成功").len(),
            2,
            "list_facts 按 project 过滤"
        );
    }

    #[test]
    fn intent_crud_roundtrip() {
        let (_dir, repo) = temp_repo();
        let intent = intent_of("proj_i", "verify exposure");
        repo.add_intent(&intent).expect("add 必须成功");
        assert_eq!(
            repo.get_intent("intent_1")
                .expect("查询必须成功")
                .expect("intent 必须存在"),
            intent
        );

        let mut updated = intent.clone();
        updated.title = "verify exposure (revised)".to_string();
        repo.update_intent(&updated).expect("upsert 必须成功");
        assert_eq!(
            repo.list_intents("proj_i").expect("查询必须成功").len(),
            1,
            "upsert 不产生第二行"
        );
        assert_eq!(
            repo.get_intent("intent_1")
                .expect("查询必须成功")
                .expect("intent 必须存在")
                .title,
            "verify exposure (revised)"
        );
    }

    #[test]
    fn commit_solver_result_persists_all_records_atomically() {
        let (_dir, repo) = temp_repo();
        let mut committed = task("run_c", "task_c1");
        committed.status = TaskStatus::Succeeded;
        let intent = intent_of("proj_1", "verify exposure");
        let mut resolved = intent.clone();
        resolved.title = "verify exposure (resolved)".to_string();

        let mut proposed = Intent::new(
            ProjectId::new("proj_1".to_string()),
            "follow-up".to_string(),
        );
        proposed.id = IntentId::new("intent_proposed".to_string());
        proposed.created_at = timestamp();
        proposed.updated_at = timestamp();

        let inv = ToolInvocation::new("semgrep".to_string(), "scan".to_string());

        repo.commit_solver_result(
            &committed,
            &resolved,
            &[inv],
            &[fact_of("fact_c1", "proj_1", "stmt")],
            &[evidence("evd_c1", "proj_1")],
            &[finding("find_c1", "proj_1", "evd_c1")],
            &[proposed],
            &[event_of("event_c1", "proj_1", Some("run_c"))],
        )
        .expect("提交必须成功");

        // task 与 intent 是 upsert，其余是纯插入；全部一次事务落库。
        let tasks = repo.list_tasks("run_c").expect("task 查询必须成功");
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].status, TaskStatus::Succeeded);
        let intents = repo.list_intents("proj_1").expect("intent 查询必须成功");
        assert_eq!(intents.len(), 2, "resolved 原意图 + proposed 新意图");
        assert!(
            intents
                .iter()
                .any(|i| i.id.as_str() == "intent_1" && i.title == "verify exposure (resolved)")
        );
        assert!(repo.get_finding("find_c1").is_ok());
        assert_eq!(
            repo.list_facts("proj_1").expect("fact 查询必须成功").len(),
            1
        );
        assert_eq!(
            repo.list_events("proj_1", Some("run_c"), 100, None)
                .expect("事件查询必须成功")
                .len(),
            1
        );
    }

    #[test]
    fn commit_solver_result_conflict_rolls_back_whole_transaction() {
        let (_dir, repo) = temp_repo();
        let duplicate = fact_of("fact_dup", "proj_1", "first");
        repo.add_fact(&duplicate).expect("首次追加必须成功");

        let failing = task("run_x", "task_x1");
        let inv = ToolInvocation::new("nuclei".to_string(), "scan".to_string());
        let error = repo
            .commit_solver_result(
                &failing,
                &intent_of("proj_1", "irrelevant"),
                std::slice::from_ref(&inv),
                // 批内第二次插入同一 id：触发 UNIQUE 约束冲突。
                std::slice::from_ref(&duplicate),
                &[evidence("evd_x1", "proj_1")],
                &[],
                &[],
                &[event_of("event_x1", "proj_1", Some("run_x"))],
            )
            .expect_err("重复 fact 必须令整次提交失败");
        assert!(matches!(error, StorageError::SolverCommitConflict));
        assert_eq!(error.to_string(), "solver result commit failed");

        // Python 语义：IntegrityError → 回滚全部——先于冲突写入的
        // invocation / evidence / task / intent 一律不得残留。
        assert!(
            repo.list_tool_invocations(None)
                .expect("查询必须成功")
                .is_empty(),
            "冲突前插入的 invocation 必须被回滚"
        );
        assert!(
            repo.list_evidence("proj_1")
                .expect("查询必须成功")
                .is_empty(),
            "证据不得因部分提交而落库"
        );
        assert!(repo.get_task("task_x1").expect("查询必须成功").is_none());
        assert!(
            repo.get_intent("intent_1").expect("查询必须成功").is_none(),
            "intent 的 upsert 也不得残留"
        );

        // 回滚后连接状态干净：去重的重试必须完整成功。
        let ok = task("run_x", "task_retry");
        repo.commit_solver_result(
            &ok,
            &intent_of("proj_1", "retry"),
            &[],
            &[fact_of("fact_retry", "proj_1", "fresh")],
            &[],
            &[],
            &[],
            &[],
        )
        .expect("重试必须成功");
        assert_eq!(
            repo.list_tasks("run_x").expect("查询必须成功").len(),
            1,
            "只有重试批次落库"
        );
    }

    #[test]
    fn event_stream_scope_limit_and_after_id() {
        let (_dir, repo) = temp_repo();
        for i in 1..=5 {
            repo.add_event(&event_of(
                &format!("event_{i}"),
                "proj_e",
                if i <= 3 { Some("run_a") } else { Some("run_b") },
            ))
            .expect("事件必须可追加");
        }

        let run_a = repo
            .list_events("proj_e", Some("run_a"), 200, None)
            .expect("run 过滤必须成功");
        assert_eq!(
            run_a.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            ["event_1", "event_2", "event_3"],
            "run_id 走 SQL 过滤 + seq 升序"
        );

        // limit 夹取：0 → 1，超大 → 全量（≤1000）。
        let clamped_low = repo
            .list_events("proj_e", None, 0, None)
            .expect("limit=0 必须被夹取为 1");
        assert_eq!(clamped_low.len(), 1, "limit 下界夹取到 1");
        let clamped_high = repo
            .list_events("proj_e", None, 100_000, None)
            .expect("超大 limit 必须被夹取为 1000");
        assert_eq!(clamped_high.len(), 5, "limit 上界不影响少于上限的结果");

        // after_id：seq 子查询增量拉取。
        let after = repo
            .list_events("proj_e", None, 200, Some("event_2"))
            .expect("after_id 必须成功");
        assert_eq!(
            after.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            ["event_3", "event_4", "event_5"],
            "after_id 取该 seq 之后的事件"
        );
        // 不存在的 after_id：子查询为 NULL，结果为空（Python 同义）。
        assert!(
            repo.list_events("proj_e", None, 200, Some("missing"))
                .expect("查询必须成功")
                .is_empty(),
            "不存在的 after_id 必须返回空集"
        );
    }

    #[test]
    fn closure_chain_assessments_project_run_scope() {
        let (_dir, repo) = temp_repo();
        let project = ProjectId::new("proj_chain".to_string());
        let run_a = RunId::new("run_a".to_string());
        let run_b = RunId::new("run_b".to_string());

        let mut coverage = CoverageAssessment::new(project.clone(), run_a.clone());
        coverage.id = CoverageAssessmentId::new("cov_1".to_string());
        repo.add_coverage_assessment(&coverage)
            .expect("COV 必须可追加");

        let mut meta = MetacognitionAssessment::new(project.clone(), run_a.clone());
        meta.id = MetacognitionAssessmentId::new("meta_1".to_string());
        repo.add_metacognition_assessment(&meta)
            .expect("META 必须可追加");

        let mut gate = ExitGateDecision::new(project.clone(), run_a.clone());
        gate.id = ExitGateDecisionId::new("gate_1".to_string());
        gate.created_at = timestamp();
        repo.add_exit_gate_decision(&gate)
            .expect("MGATE 必须可追加");

        let mut guard = EscalationGuardVerdict::new(project.clone(), run_a.clone());
        guard.id = EscalationGuardVerdictId::new("guard_1".to_string());
        repo.add_escalation_guard_verdict(&guard)
            .expect("EGUARD 必须可追加");

        // run_b 的评估：验证 scope 过滤。
        let mut coverage_b = CoverageAssessment::new(project.clone(), run_b.clone());
        coverage_b.id = CoverageAssessmentId::new("cov_2".to_string());
        repo.add_coverage_assessment(&coverage_b)
            .expect("run_b 的 COV 必须可追加");

        assert_eq!(
            repo.list_coverage_assessments("proj_chain", Some("run_a"))
                .expect("查询必须成功")
                .len(),
            1,
            "run 过滤只留 run_a"
        );
        assert_eq!(
            repo.list_coverage_assessments("proj_chain", None)
                .expect("查询必须成功")
                .len(),
            2,
            "run=None 列整个 project"
        );
        assert_eq!(
            repo.list_metacognition_assessments("proj_chain", Some("run_a"))
                .expect("查询必须成功")
                .len(),
            1
        );
        assert_eq!(
            repo.list_exit_gate_decisions("proj_chain", Some("run_a"))
                .expect("查询必须成功")
                .len(),
            1
        );
        assert_eq!(
            repo.list_escalation_guard_verdicts("proj_chain", Some("run_a"))
                .expect("查询必须成功")
                .len(),
            1
        );
        // 无 run 过滤时 payload 往返一致性。
        assert_eq!(
            repo.list_exit_gate_decisions("proj_chain", None)
                .expect("查询必须成功"),
            vec![gate]
        );
    }

    #[test]
    fn observation_project_run_scope() {
        let (_dir, repo) = temp_repo();
        let project = ProjectId::new("proj_obs".to_string());
        let mut a1 = Observation::new(
            project.clone(),
            RunId::new("run_a".to_string()),
            "observed login form".to_string(),
        );
        a1.id = ObservationId::new("obs_1".to_string());
        let mut a2 = Observation::new(
            project.clone(),
            RunId::new("run_a".to_string()),
            "observed api".to_string(),
        );
        a2.id = ObservationId::new("obs_2".to_string());
        let mut b1 = Observation::new(
            project.clone(),
            RunId::new("run_b".to_string()),
            "other run".to_string(),
        );
        b1.id = ObservationId::new("obs_3".to_string());

        repo.add_observation(&a1).expect("obs_1 必须可追加");
        repo.add_observation(&a2).expect("obs_2 必须可追加");
        repo.add_observation(&b1).expect("obs_3 必须可追加");

        let run_a = repo
            .list_observations("proj_obs", Some("run_a"))
            .expect("run 过滤必须成功");
        assert_eq!(
            run_a.iter().map(|o| o.id.as_str()).collect::<Vec<_>>(),
            ["obs_1", "obs_2"],
            "run 过滤 + seq 升序"
        );
        assert_eq!(
            repo.list_observations("proj_obs", None)
                .expect("查询必须成功")
                .len(),
            3,
            "run=None 列整个 project"
        );
    }

    #[test]
    fn critique_and_trajectory_branch_payload_filter() {
        let (_dir, repo) = temp_repo();
        let project = ProjectId::new("proj_ct".to_string());
        let run = RunId::new("run_a".to_string());
        let branch_x = BranchId::new("branch_x".to_string());
        let branch_y = BranchId::new("branch_y".to_string());

        let mut critique_x = CritiqueReport::new(
            project.clone(),
            branch_x.clone(),
            CritiqueVerdict::Accepted,
            "hypothesis x".to_string(),
        );
        critique_x.id = CritiqueReportId::new("crit_x".to_string());
        critique_x.run_id = Some(run.clone());
        critique_x.created_at = timestamp();
        let mut critique_y = CritiqueReport::new(
            project.clone(),
            branch_y.clone(),
            CritiqueVerdict::Rejected,
            "hypothesis y".to_string(),
        );
        critique_y.id = CritiqueReportId::new("crit_y".to_string());
        critique_y.run_id = Some(run.clone());
        repo.add_critique_report(&critique_x)
            .expect("crit_x 必须可追加");
        repo.add_critique_report(&critique_y)
            .expect("crit_y 必须可追加");
        assert_eq!(
            repo.get_critique_report("crit_x")
                .expect("查询必须成功")
                .expect("crit_x 必须存在"),
            critique_x,
            "payload 往返不得改变实体"
        );

        let only_x = repo
            .list_critique_reports("proj_ct", Some("run_a"), Some("branch_x"))
            .expect("branch 过滤必须成功");
        assert_eq!(
            only_x.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            ["crit_x"],
            "branch_id 是 payload 字段，查询后内存过滤"
        );
        assert_eq!(
            repo.list_critique_reports("proj_ct", Some("run_a"), None)
                .expect("查询必须成功")
                .len(),
            2,
            "branch=None 不做内存过滤"
        );

        let mut traj_x = TrajectorySummary::new(
            project.clone(),
            run.clone(),
            "segment for branch x".to_string(),
        );
        traj_x.id = TrajectorySummaryId::new("traj_x".to_string());
        traj_x.branch_id = Some(branch_x);
        traj_x.created_at = timestamp();
        let mut traj_y = TrajectorySummary::new(project, run, "segment for branch y".to_string());
        traj_y.id = TrajectorySummaryId::new("traj_y".to_string());
        traj_y.branch_id = Some(branch_y);
        repo.add_trajectory_summary(&traj_x)
            .expect("traj_x 必须可追加");
        repo.add_trajectory_summary(&traj_y)
            .expect("traj_y 必须可追加");
        assert_eq!(
            repo.get_trajectory_summary("traj_x")
                .expect("查询必须成功")
                .expect("traj_x 必须存在"),
            traj_x,
            "payload 往返不得改变实体"
        );

        let only_y = repo
            .list_trajectory_summaries("proj_ct", Some("run_a"), Some("branch_y"))
            .expect("branch 过滤必须成功");
        assert_eq!(
            only_y.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            ["traj_y"]
        );
    }

    #[test]
    fn agent_narrative_events_roundtrip_and_payload_filters() {
        let (_dir, repo) = temp_repo();
        let make_event = |id: &str, branch: Option<&str>, text: &str| {
            let mut event = AgentNarrativeEvent::new(
                ProjectId::new("proj_narr".to_string()),
                CreateAgentNarrativeRequest {
                    audit_run_id: Some(RunId::new("run_a".to_string())),
                    branch_id: branch.map(|value| BranchId::new(value.to_string())),
                    event_kind: AgentNarrativeEventKind::Progress,
                    metadata: JsonMap::new(),
                    mission_id: Some(MissionId::new("mission_a".to_string())),
                    original_language: Some("zh-CN".to_string()),
                    original_text: text.to_string(),
                    source_agent: "orchestrator".to_string(),
                    task_id: None,
                },
            );
            event.id = AgentNarrativeEventId::new(id.to_string());
            event
        };

        let lifecycle = make_event("narr_a", None, "任务已启动");
        let branch_note = make_event("narr_b", Some("branch_x"), "surface mapped");
        repo.add_agent_narrative_event(&lifecycle)
            .expect("narr_a 必须可追加");
        repo.add_agent_narrative_event(&branch_note)
            .expect("narr_b 必须可追加");
        assert_eq!(
            repo.list_agent_narrative_events("proj_narr", Some("run_a"), None, None, None)
                .expect("查询必须成功")
                .iter()
                .map(|event| event.id.as_str())
                .collect::<Vec<_>>(),
            ["narr_a", "narr_b"],
            "seq 升序；payload 往返不得改变实体"
        );

        // run_id 走 SQL 过滤。
        assert!(
            repo.list_agent_narrative_events("proj_narr", Some("run_b"), None, None, None)
                .expect("run 过滤必须成功")
                .is_empty()
        );
        // branch_id 是 payload 字段，查询后内存过滤。
        assert_eq!(
            repo.list_agent_narrative_events("proj_narr", None, None, Some("branch_x"), None)
                .expect("branch 过滤必须成功")
                .iter()
                .map(|event| event.id.as_str())
                .collect::<Vec<_>>(),
            ["narr_b"]
        );
        // limit 在过滤后截断（升序取前 N 条）。
        assert_eq!(
            repo.list_agent_narrative_events("proj_narr", None, Some("mission_a"), None, Some(1))
                .expect("limit 截断必须成功")
                .iter()
                .map(|event| event.id.as_str())
                .collect::<Vec<_>>(),
            ["narr_a"]
        );
        // Project 隔离。
        assert!(
            repo.list_agent_narrative_events("proj_other", None, None, None, None)
                .expect("跨 project 查询必须成功")
                .is_empty()
        );
    }

    #[test]
    fn strategy_board_snapshot_roundtrip() {
        let (_dir, repo) = temp_repo();
        let mut snapshot = StrategyBoardSnapshot::new(ProjectId::new("proj_board".to_string()));
        snapshot.id = StrategyBoardSnapshotId::new("board_1".to_string());
        snapshot.run_id = Some(RunId::new("run_a".to_string()));
        snapshot.summary = "initial board".to_string();
        snapshot.created_at = timestamp();
        repo.add_strategy_board_snapshot(&snapshot)
            .expect("快照必须可追加");
        assert_eq!(
            repo.get_strategy_board_snapshot("board_1")
                .expect("查询必须成功")
                .expect("快照必须存在"),
            snapshot,
            "payload 往返不得改变实体"
        );

        let mut second = StrategyBoardSnapshot::new(ProjectId::new("proj_board".to_string()));
        second.id = StrategyBoardSnapshotId::new("board_2".to_string());
        second.run_id = Some(RunId::new("run_b".to_string()));
        repo.add_strategy_board_snapshot(&second)
            .expect("第二个快照必须可追加");

        assert_eq!(
            repo.list_strategy_board_snapshots("proj_board", Some("run_a"))
                .expect("run 过滤必须成功")
                .len(),
            1
        );
        assert_eq!(
            repo.list_strategy_board_snapshots("proj_board", None)
                .expect("查询必须成功")
                .len(),
            2
        );
    }

    #[test]
    fn module_crud_and_enabled_filter() {
        let (_dir, repo) = temp_repo();
        let mut enabled_module = ModuleConfig::new("nuclei runner".to_string());
        enabled_module.id = ModuleId::new("mod_1".to_string());
        enabled_module.created_at = timestamp();
        enabled_module.updated_at = timestamp();
        let mut disabled_module = ModuleConfig::new("ffuf runner".to_string());
        disabled_module.id = ModuleId::new("mod_2".to_string());
        disabled_module.enabled = false;

        repo.create_module(&enabled_module)
            .expect("mod_1 必须可插入");
        repo.create_module(&disabled_module)
            .expect("mod_2 必须可插入");
        assert_eq!(
            repo.get_module("mod_1")
                .expect("查询必须成功")
                .expect("模块必须存在"),
            enabled_module,
            "payload 往返不得改变实体"
        );
        assert_eq!(repo.list_modules().expect("列全部必须成功").len(), 2);

        let mut renamed = enabled_module.clone();
        renamed.name = "nuclei runner v2".to_string();
        repo.update_module(&renamed).expect("upsert 必须成功");
        assert_eq!(
            repo.list_modules().expect("列全部必须成功").len(),
            2,
            "upsert 不产生第二行"
        );

        let enabled_only = repo.list_enabled_modules().expect("过滤必须成功");
        assert_eq!(
            enabled_only
                .iter()
                .map(|m| m.id.as_str())
                .collect::<Vec<_>>(),
            ["mod_1"],
            "只有 enabled=true 的模块被列出"
        );

        repo.delete_module("mod_1").expect("删除必须成功");
        assert!(repo.get_module("mod_1").expect("查询必须成功").is_none());
    }

    fn user_directive(id: &str, mission_id: &str, run_id: Option<&str>) -> UserDirective {
        let mut directive = UserDirective::new(
            ProjectId::new("proj_dir".to_string()),
            MissionId::new(mission_id.to_string()),
            models::UserDirectiveType::Pause,
            "stop for review".to_string(),
        );
        directive.id = models::UserDirectiveId::new(id.to_string());
        directive.created_at = timestamp();
        if let Some(run_id) = run_id {
            directive.run_id = Some(RunId::new(run_id.to_string()));
        }
        directive
    }

    #[test]
    fn user_directive_crud_and_filters() {
        let (_dir, repo) = temp_repo();
        let d1 = user_directive("directive_1", "mission_1", Some("run_1"));
        let mut d2 = user_directive("directive_2", "mission_2", None);
        d2.branch_id = Some(BranchId::new("branch_2".to_string()));

        repo.add_user_directive(&d1).expect("追加必须成功");
        repo.add_user_directive(&d2).expect("追加必须成功");

        assert_eq!(
            repo.get_user_directive("directive_1")
                .expect("查询必须成功")
                .expect("必须存在"),
            d1,
            "payload 往返不得改变实体"
        );
        assert!(
            repo.get_user_directive("absent")
                .expect("查询必须成功")
                .is_none()
        );

        let mut applied = d1.clone();
        applied.status = models::UserDirectiveStatus::Applied;
        repo.update_user_directive(&applied).expect("更新必须成功");
        assert_eq!(
            repo.get_user_directive("directive_1")
                .expect("查询必须成功")
                .expect("必须存在")
                .status,
            models::UserDirectiveStatus::Applied
        );

        let all = repo
            .list_user_directives(Some("proj_dir"), None, None, None)
            .expect("列全部必须成功");
        assert_eq!(all.len(), 2);
        let mission_scoped = repo
            .list_user_directives(Some("proj_dir"), Some("mission_1"), None, None)
            .expect("mission 过滤必须成功");
        assert_eq!(mission_scoped.len(), 1);
        let branch_scoped = repo
            .list_user_directives(None, None, None, Some("branch_2"))
            .expect("branch 过滤必须成功");
        assert_eq!(branch_scoped.len(), 1);
        let run_scoped = repo
            .list_user_directives(None, None, Some("run_1"), None)
            .expect("run 过滤必须成功");
        assert_eq!(run_scoped.len(), 1);
    }

    fn asset(id: &str, value: &str, sensitivity: MissionAssetSensitivity) -> MissionAsset {
        let mut asset = MissionAsset {
            id: models::MissionAssetId::new(id.to_string()),
            project_id: ProjectId::new("proj_asset".to_string()),
            mission_id: MissionId::new("mission_a".to_string()),
            asset_type: MissionAssetType::Url,
            value: value.to_string(),
            label: None,
            sensitivity,
            confidence: 0.5,
            source: models::MissionAssetSource::Evidence,
            source_id: None,
            branch_id: None,
            run_id: None,
            evidence_ids: Vec::new(),
            finding_ids: Vec::new(),
            tool_invocation_ids: Vec::new(),
            tags: Vec::new(),
            metadata: serde_json::Map::new(),
            created_at: timestamp(),
            updated_at: timestamp(),
        };
        asset.value = value.trim().to_string();
        asset
    }

    #[test]
    fn mission_asset_crud_and_filters() {
        let (_dir, repo) = temp_repo();
        let a1 = asset(
            "asset_1",
            "https://Example.COM/a/",
            MissionAssetSensitivity::Unknown,
        );
        let mut a2 = asset("asset_2", "10.0.0.1", MissionAssetSensitivity::Sensitive);
        a2.asset_type = MissionAssetType::Ip;

        repo.create_mission_asset(&a1).expect("插入必须成功");
        repo.create_mission_asset(&a2).expect("插入必须成功");

        assert_eq!(
            repo.get_mission_asset("asset_1")
                .expect("查询必须成功")
                .expect("必须存在"),
            a1,
            "payload 往返不得改变实体（tags 归一化路径一致）"
        );

        let all = repo
            .list_mission_assets(Some("mission_a"), None, None, None)
            .expect("列全部必须成功");
        assert_eq!(all.len(), 2);
        let urls = repo
            .list_mission_assets(None, None, None, Some(MissionAssetType::Url))
            .expect("类型过滤必须成功");
        assert_eq!(urls.len(), 1);
        let sensitive = repo
            .list_mission_assets(
                Some("mission_a"),
                None,
                Some(MissionAssetSensitivity::Sensitive),
                None,
            )
            .expect("敏感级别过滤必须成功");
        assert_eq!(sensitive.len(), 1);

        let mut renamed = a2.clone();
        renamed.label = Some("gateway".to_string());
        repo.update_mission_asset(&renamed).expect("更新必须成功");
        assert_eq!(
            repo.get_mission_asset("asset_2")
                .expect("查询必须成功")
                .expect("必须存在")
                .label,
            Some("gateway".to_string())
        );
    }

    #[test]
    fn upsert_mission_asset_merges_on_dedupe_key() {
        let (_dir, repo) = temp_repo();
        let first = asset(
            "asset_a",
            "https://Example.COM/a/",
            MissionAssetSensitivity::Unknown,
        );
        let inserted = repo
            .upsert_mission_asset(&first)
            .expect("首次 upsert 必须成功");
        assert_eq!(inserted, first);

        // 同一去重键（大小写/尾斜杠归一）但不同 id：合并而非新增。
        let mut second = asset(
            "asset_b",
            "https://example.com/a",
            MissionAssetSensitivity::Sensitive,
        );
        second.confidence = 0.9;
        second.tags = vec!["edge".to_string()];
        let merged = repo
            .upsert_mission_asset(&second)
            .expect("去重 upsert 必须成功");
        assert_eq!(merged.id, first.id, "合并保留既有 id");
        assert_eq!(merged.sensitivity, MissionAssetSensitivity::Sensitive);
        assert!((merged.confidence - 0.9).abs() < 1e-9);
        assert_eq!(merged.tags, ["edge"]);

        let rows = repo
            .list_mission_assets(Some("mission_a"), None, None, None)
            .expect("查询必须成功");
        assert_eq!(rows.len(), 1, "去重键冲突不产生第二行");
    }

    #[test]
    fn reflector_report_roundtrip_and_scope() {
        let (_dir, repo) = temp_repo();
        let mut report = ReflectorReport::new(
            ProjectId::new("proj_ref".to_string()),
            RunId::new("run_1".to_string()),
        );
        report.id = models::ReflectorReportId::new("reflector_1".to_string());
        report.task_id = Some(TaskId::new("task_1".to_string()));
        report.failure_type = models::ReflectorFailureType::Timeout;
        report.failure_modes = vec!["timeout".to_string()];
        report.created_at = timestamp();

        repo.add_reflector_report(&report).expect("追加必须成功");
        assert_eq!(
            repo.list_reflector_reports("proj_ref", Some("run_1"))
                .expect("run 过滤必须成功"),
            vec![report.clone()],
            "payload 往返不得改变实体"
        );
        let mut other = ReflectorReport::new(
            ProjectId::new("proj_ref".to_string()),
            RunId::new("run_2".to_string()),
        );
        other.id = models::ReflectorReportId::new("reflector_2".to_string());
        repo.add_reflector_report(&other).expect("追加必须成功");
        assert_eq!(
            repo.list_reflector_reports("proj_ref", None)
                .expect("列全部必须成功")
                .len(),
            2
        );
    }
}
