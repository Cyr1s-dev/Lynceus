//! 物理模式 —— `sqlite_repository.py` `_init_schema` 的逐句镜像。
//!
//! 双跑期间 Rust 与 Python 引擎打开同一个数据库文件，因此建表语句必须
//! 产出完全相同的物理结构：表集合（46 张）、列名、列序、约束与索引全部
//! 对齐 `server/core/storage/sqlite_repository.py` 的 `_TABLES` /
//! `_RUN_SCOPED` / DDL f-string。
//!
//! WP7 起新增 `finding_retests`（漏洞复测记录，第 47 张表）：它是 Rust
//! 侧独有的实体，Python 侧没有对应表，因此走通用 JSON-blob 布局，列集合
//! 与其余 `storable_project!` 表完全一致——不需要迁移分支，老库首次
//! `init` 时由 `CREATE TABLE IF NOT EXISTS` 补齐。
//!
//! 通用表布局（`mission_assets` 除外）：整实体 JSON 存 `payload` 列，
//! `id` + `project_id`（部分表加 `run_id`）做索引列，`created_at` 存
//! ISO 时间字符串。`sqlite_master` 存的是 DDL 原文，但 dump 规范只读
//! 表名不读 DDL 文本，因此两侧 DDL 的空白差异不影响对拍。

use rusqlite::Connection;

use crate::error::StorageError;

/// 全部实体表，顺序即建表顺序（Python `_TABLES` + WP7 新增 `finding_retests`）。
pub const TABLES: [&str; 46] = [
    "projects",
    "missions",
    "mission_assets",
    "branches",
    "user_directives",
    "checkpoints",
    "providers",
    "model_capabilities",
    "provider_routes",
    "modules",
    "module_discovery_sessions",
    "module_config_proposals",
    "facts",
    "intents",
    "hints",
    "knowledge_cards",
    "artifact_records",
    "retrieval_invocations",
    "runtime_settings",
    "agent_narrative_events",
    "evidence",
    "findings",
    "finding_retests",
    "audit_runs",
    "agent_tasks",
    "decision_gates",
    "context_packs",
    "context_compression_reports",
    "critique_reports",
    "trajectory_summaries",
    "model_race_results",
    "worker_profiles",
    "worker_leases",
    "observations",
    "advisor_reviews",
    "reflector_reports",
    "termination_assessments",
    "coverage_assessments",
    "metacognition_assessments",
    "exit_gate_decisions",
    "escalation_guard_verdicts",
    "strategy_board_snapshots",
    "execution_jobs",
    "tool_invocations",
    "model_invocations",
    "audit_events",
];

/// 带 `run_id` scope 列的表（Python `_RUN_SCOPED`）。
const RUN_SCOPED: [&str; 26] = [
    "agent_tasks",
    "audit_events",
    "decision_gates",
    "branches",
    "user_directives",
    "checkpoints",
    "context_packs",
    "context_compression_reports",
    "critique_reports",
    "trajectory_summaries",
    "model_race_results",
    "artifact_records",
    "retrieval_invocations",
    "runtime_settings",
    "agent_narrative_events",
    "worker_leases",
    "observations",
    "advisor_reviews",
    "reflector_reports",
    "termination_assessments",
    "coverage_assessments",
    "metacognition_assessments",
    "exit_gate_decisions",
    "escalation_guard_verdicts",
    "strategy_board_snapshots",
    "execution_jobs",
];

/// `mission_assets` 的专用 DDL：去重列（`asset_type` + `normalized_value`）需要
/// 独立索引与 `updated_at` 列，不套通用布局。
const MISSION_ASSETS_DDL: &str = "CREATE TABLE IF NOT EXISTS mission_assets (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    id TEXT NOT NULL UNIQUE,
    project_id TEXT NOT NULL,
    mission_id TEXT NOT NULL,
    run_id TEXT,
    asset_type TEXT NOT NULL,
    normalized_value TEXT NOT NULL,
    payload TEXT NOT NULL,
    created_at TEXT,
    updated_at TEXT
)";

/// 二级索引，顺序与 Python `_init_schema` 一致。
const INDEXES: [(&str, &str); 7] = [
    (
        "idx_audit_events_project_seq",
        "CREATE INDEX IF NOT EXISTS idx_audit_events_project_seq \
         ON audit_events (project_id, seq)",
    ),
    (
        "idx_audit_events_project_run_seq",
        "CREATE INDEX IF NOT EXISTS idx_audit_events_project_run_seq \
         ON audit_events (project_id, run_id, seq)",
    ),
    (
        "idx_agent_narratives_project_seq",
        "CREATE INDEX IF NOT EXISTS idx_agent_narratives_project_seq \
         ON agent_narrative_events (project_id, seq)",
    ),
    (
        "idx_agent_narratives_project_run_seq",
        "CREATE INDEX IF NOT EXISTS idx_agent_narratives_project_run_seq \
         ON agent_narrative_events (project_id, run_id, seq)",
    ),
    (
        "idx_mission_assets_project",
        "CREATE INDEX IF NOT EXISTS idx_mission_assets_project \
         ON mission_assets (project_id)",
    ),
    (
        "idx_mission_assets_mission",
        "CREATE INDEX IF NOT EXISTS idx_mission_assets_mission \
         ON mission_assets (mission_id)",
    ),
    (
        "idx_mission_assets_dedupe",
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_mission_assets_dedupe \
         ON mission_assets (mission_id, asset_type, normalized_value)",
    ),
];

/// 表是否带 `run_id` scope 列。
#[must_use]
pub fn is_run_scoped(table: &str) -> bool {
    RUN_SCOPED.contains(&table)
}

/// 仓储层允许用于 WHERE 等值过滤的物理列。
///
/// 列名以 `&str` 传参时（`list_where(conn, "project_id", ...)`），拼写
/// 错误要到运行期 SQL 失败才暴露；枚举 + 穷尽 match 把非法列名在编译
/// 期消灭，新增列（如未来的 `mission_id` 物理列）也必须显式经过这里。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScopeColumn {
    /// `project_id` scope 列。
    ProjectId,
    /// `run_id` scope 列。
    RunId,
}

impl ScopeColumn {
    /// SQL 列名文本。
    ///
    /// 穷尽 match：新增变体会让这里编译失败，强制同步提供列名。
    #[must_use]
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::ProjectId => "project_id",
            Self::RunId => "run_id",
        }
    }
}

/// 通用表 DDL（Python 侧 f-string 的等价文本）。
#[must_use]
pub fn generic_table_ddl(table: &str) -> String {
    let run_column = if is_run_scoped(table) {
        "    run_id TEXT,\n"
    } else {
        ""
    };
    format!(
        "CREATE TABLE IF NOT EXISTS {table} (\n    \
         seq INTEGER PRIMARY KEY AUTOINCREMENT,\n    \
         id TEXT NOT NULL UNIQUE,\n    \
         project_id TEXT,\n    \
         {run_column}\
         payload TEXT NOT NULL,\n    \
         created_at TEXT\n)"
    )
}

/// 在已有连接上执行全部 DDL（幂等：`IF NOT EXISTS`）。
///
/// 表名来自 [`TABLES`] 静态常量，插值进 SQL 的是编译期已知标识符，
/// 不是用户输入，无注入面。
///
/// # Errors
/// - [`StorageError::Schema`]：任一 DDL 执行失败。
pub fn init(connection: &Connection) -> Result<(), StorageError> {
    for &table in &TABLES {
        let ddl = if table == "mission_assets" {
            MISSION_ASSETS_DDL.to_string()
        } else {
            generic_table_ddl(table)
        };
        connection
            .execute(&ddl, [])
            .map_err(|source| StorageError::Schema {
                table: table.to_string(),
                source,
            })?;
    }
    for (name, ddl) in INDEXES {
        connection
            .execute(ddl, [])
            .map_err(|source| StorageError::Schema {
                table: name.to_string(),
                source,
            })?;
    }
    init_intelligence(connection)?;
    init_worker(connection)
}

// ---------------------------------------------------------------------------
// Intelligence Hub 表（Rust 原生新增，无 Python 对拍义务）
// ---------------------------------------------------------------------------

/// 情报实体表：整实体 JSON 存 `payload`，`(kind, normalized_value)` 是
/// 全局去重键（唯一索引），`source_count`/`first_seen`/`last_seen` 在
/// payload 内随合并更新。
const INTEL_ENTITIES_DDL: &str = "CREATE TABLE IF NOT EXISTS intel_entities (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    id TEXT NOT NULL UNIQUE,
    kind TEXT NOT NULL,
    normalized_value TEXT NOT NULL,
    payload TEXT NOT NULL,
    created_at TEXT,
    updated_at TEXT
)";

/// 情报关系表：`from_entity_id`/`to_entity_id` 做索引列便于图遍历，
/// 整关系 JSON 存 `payload`。
const INTEL_RELATIONS_DDL: &str = "CREATE TABLE IF NOT EXISTS intel_relations (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    id TEXT NOT NULL UNIQUE,
    from_entity_id TEXT NOT NULL,
    relation TEXT NOT NULL,
    to_entity_id TEXT NOT NULL,
    payload TEXT NOT NULL,
    created_at TEXT
)";

/// 情报原始记录表：source 原生 payload 的留痕（provenance 链起点）。
const INTEL_RAW_RECORDS_DDL: &str = "CREATE TABLE IF NOT EXISTS intel_raw_records (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    id TEXT NOT NULL UNIQUE,
    source TEXT NOT NULL,
    source_record_id TEXT,
    payload TEXT NOT NULL,
    created_at TEXT
)";

/// 逻辑实体与支持它的原始记录是多对多关系。
const INTEL_ENTITY_SOURCES_DDL: &str = "CREATE TABLE IF NOT EXISTS intel_entity_sources (
    entity_id TEXT NOT NULL,
    raw_record_id TEXT NOT NULL,
    PRIMARY KEY (entity_id, raw_record_id),
    FOREIGN KEY (entity_id) REFERENCES intel_entities(id) ON DELETE CASCADE,
    FOREIGN KEY (raw_record_id) REFERENCES intel_raw_records(id) ON DELETE CASCADE
)";

/// 逻辑关系与支持它的原始记录是多对多关系。
const INTEL_RELATION_SOURCES_DDL: &str = "CREATE TABLE IF NOT EXISTS intel_relation_sources (
    relation_id TEXT NOT NULL,
    raw_record_id TEXT NOT NULL,
    PRIMARY KEY (relation_id, raw_record_id),
    FOREIGN KEY (relation_id) REFERENCES intel_relations(id) ON DELETE CASCADE,
    FOREIGN KEY (raw_record_id) REFERENCES intel_raw_records(id) ON DELETE CASCADE
)";

/// Intelligence Hub 二级索引。
const INTEL_INDEXES: [(&str, &str); 7] = [
    (
        "idx_intel_entities_dedupe",
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_intel_entities_dedupe \
         ON intel_entities (kind, normalized_value)",
    ),
    (
        "idx_intel_relations_from",
        "CREATE INDEX IF NOT EXISTS idx_intel_relations_from \
         ON intel_relations (from_entity_id)",
    ),
    (
        "idx_intel_relations_to",
        "CREATE INDEX IF NOT EXISTS idx_intel_relations_to \
         ON intel_relations (to_entity_id)",
    ),
    (
        "idx_intel_relations_dedupe",
        "CREATE INDEX IF NOT EXISTS idx_intel_relations_dedupe \
         ON intel_relations (from_entity_id, relation, to_entity_id)",
    ),
    (
        "idx_intel_raw_records_source",
        "CREATE INDEX IF NOT EXISTS idx_intel_raw_records_source \
         ON intel_raw_records (source)",
    ),
    (
        "idx_intel_entity_sources_raw",
        "CREATE INDEX IF NOT EXISTS idx_intel_entity_sources_raw \
         ON intel_entity_sources (raw_record_id)",
    ),
    (
        "idx_intel_relation_sources_raw",
        "CREATE INDEX IF NOT EXISTS idx_intel_relation_sources_raw \
         ON intel_relation_sources (raw_record_id)",
    ),
];

/// 外部 Worker 会话表（Rust-native，非 parity；dump 按 `worker_runs` 排除）。
const WORKER_RUNS_DDL: &str = "CREATE TABLE IF NOT EXISTS worker_runs (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    id TEXT NOT NULL UNIQUE,
    project_id TEXT NOT NULL,
    run_id TEXT,
    task_id TEXT,
    runtime TEXT NOT NULL,
    status TEXT NOT NULL,
    agent_preset_id TEXT,
    payload TEXT NOT NULL,
    created_at TEXT
)";

/// 外部 Worker 调用审计表（Rust-native，非 parity）。
const WORKER_INVOCATIONS_DDL: &str = "CREATE TABLE IF NOT EXISTS worker_invocations (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    id TEXT NOT NULL UNIQUE,
    project_id TEXT,
    worker_run_id TEXT,
    runtime TEXT NOT NULL,
    purpose TEXT NOT NULL,
    status TEXT NOT NULL,
    payload TEXT NOT NULL,
    created_at TEXT
)";

/// Worker Runtime Profile 表：runtime → Connection 绑定（Rust-native）。
const WORKER_RUNTIME_PROFILES_DDL: &str = "CREATE TABLE IF NOT EXISTS worker_runtime_profiles (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    id TEXT NOT NULL UNIQUE,
    runtime_type TEXT NOT NULL,
    connection_id TEXT NOT NULL,
    enabled INTEGER NOT NULL,
    payload TEXT NOT NULL,
    created_at TEXT
)";

/// Skill 调用台账（WP6；故意不做外键——统计比任务活得久）。
/// `skill` 是模型点名的名字：不存在也记一行（found=0），构成缺口清单。
const SKILL_USAGE_DDL: &str = "CREATE TABLE IF NOT EXISTS skill_usage (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    ts TEXT NOT NULL,
    skill TEXT NOT NULL,
    agent_preset TEXT,
    mission_id TEXT,
    run_id TEXT,
    args_len INTEGER NOT NULL DEFAULT 0,
    found INTEGER NOT NULL DEFAULT 0
)";

/// Agent 预设表：三处硬编码提示词的可编辑收编（Rust-native）。
/// `id` 即稳定 key；payload 存整实体（含版本历史）。
const AGENT_PRESETS_DDL: &str = "CREATE TABLE IF NOT EXISTS agent_presets (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    id TEXT NOT NULL UNIQUE,
    builtin INTEGER NOT NULL,
    enabled INTEGER NOT NULL,
    payload TEXT NOT NULL,
    created_at TEXT
)";

/// Worker 结构化用量表（agent 事件流上报；cost 仅真实报出时非空）。
const WORKER_USAGE_DDL: &str = "CREATE TABLE IF NOT EXISTS worker_usage (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id TEXT NOT NULL UNIQUE,
    project_id TEXT,
    runtime TEXT NOT NULL,
    model TEXT,
    requested_model TEXT,
    input_tokens INTEGER NOT NULL DEFAULT 0,
    output_tokens INTEGER NOT NULL DEFAULT 0,
    cached_input_tokens INTEGER NOT NULL DEFAULT 0,
    reasoning_tokens INTEGER NOT NULL DEFAULT 0,
    cost_usd REAL,
    num_turns INTEGER,
    duration_api_ms INTEGER,
    created_at TEXT
)";

/// Worker 外部执行二级索引。
const WORKER_INDEXES: [(&str, &str); 6] = [
    (
        "idx_worker_runs_project",
        "CREATE INDEX IF NOT EXISTS idx_worker_runs_project \
         ON worker_runs (project_id)",
    ),
    (
        "idx_worker_runs_run",
        "CREATE INDEX IF NOT EXISTS idx_worker_runs_run \
         ON worker_runs (run_id)",
    ),
    (
        "idx_worker_invocations_run",
        "CREATE INDEX IF NOT EXISTS idx_worker_invocations_run \
         ON worker_invocations (worker_run_id)",
    ),
    (
        "idx_worker_runtime_profiles_runtime",
        "CREATE INDEX IF NOT EXISTS idx_worker_runtime_profiles_runtime \
         ON worker_runtime_profiles (runtime_type)",
    ),
    (
        "idx_worker_runtime_profiles_connection",
        "CREATE INDEX IF NOT EXISTS idx_worker_runtime_profiles_connection \
         ON worker_runtime_profiles (connection_id)",
    ),
    (
        "idx_agent_presets_builtin",
        "CREATE INDEX IF NOT EXISTS idx_agent_presets_builtin \
         ON agent_presets (builtin)",
    ),
];

/// Intelligence Hub DDL（幂等）。
///
/// # Errors
/// - [`StorageError::Schema`]：任一 DDL 执行失败。
pub fn init_intelligence(connection: &Connection) -> Result<(), StorageError> {
    for (table, ddl) in [
        ("intel_entities", INTEL_ENTITIES_DDL),
        ("intel_relations", INTEL_RELATIONS_DDL),
        ("intel_raw_records", INTEL_RAW_RECORDS_DDL),
        ("intel_entity_sources", INTEL_ENTITY_SOURCES_DDL),
        ("intel_relation_sources", INTEL_RELATION_SOURCES_DDL),
    ] {
        connection
            .execute(ddl, [])
            .map_err(|source| StorageError::Schema {
                table: table.to_string(),
                source,
            })?;
    }
    for (name, ddl) in INTEL_INDEXES {
        connection
            .execute(ddl, [])
            .map_err(|source| StorageError::Schema {
                table: name.to_string(),
                source,
            })?;
    }
    Ok(())
}

/// 外部 Worker Runtime DDL（幂等；Rust-native，不进 parity TABLES）。
///
/// # Errors
/// - [`StorageError::Schema`]：任一 DDL 执行失败。
pub fn init_worker(connection: &Connection) -> Result<(), StorageError> {
    for (table, ddl) in [
        ("worker_runs", WORKER_RUNS_DDL),
        ("worker_invocations", WORKER_INVOCATIONS_DDL),
        ("worker_runtime_profiles", WORKER_RUNTIME_PROFILES_DDL),
        ("worker_usage", WORKER_USAGE_DDL),
        ("agent_presets", AGENT_PRESETS_DDL),
        ("skill_usage", SKILL_USAGE_DDL),
    ] {
        connection
            .execute(ddl, [])
            .map_err(|source| StorageError::Schema {
                table: table.to_string(),
                source,
            })?;
    }
    for (name, ddl) in WORKER_INDEXES {
        connection
            .execute(ddl, [])
            .map_err(|source| StorageError::Schema {
                table: name.to_string(),
                source,
            })?;
    }
    migrate_worker_runs_agent_preset(connection)?;
    strip_mission_target_type(connection)?;
    Ok(())
}

/// 存量库幂等迁移：`missions.payload` 里的 `target_type` 键已从
/// [`models::Mission`] 删除，老行反序列化会以 `deny_unknown_fields` 直接
/// 失败，整个 mission 列表都读不出来。
///
/// 只在 payload 真的含该键时改写（`json_type` 判对象 + `json_remove`），
/// 新库与已迁移库都是零行命中的空操作。
///
/// `rusqlite` 的 bundled SQLite 默认带 JSON1，`json_remove` / `json_type` /
/// `json_extract` 均可用。
fn strip_mission_target_type(connection: &Connection) -> Result<(), StorageError> {
    connection
        .execute(
            "UPDATE missions
             SET payload = json_remove(payload, '$.target_type')
             WHERE json_valid(payload)
               AND json_type(payload) = 'object'
               AND json_extract(payload, '$.target_type') IS NOT NULL",
            [],
        )
        .map_err(|source| StorageError::Schema {
            table: "missions".to_string(),
            source,
        })?;
    Ok(())
}

/// 存量库幂等迁移：`worker_runs` 补 `agent_preset_id` 列（WP4 审计）。
/// 新库由 `WORKER_RUNS_DDL` 直接建列，此处对老库 `ALTER TABLE`。
fn migrate_worker_runs_agent_preset(connection: &Connection) -> Result<(), StorageError> {
    let has_column: bool = connection
        .prepare("PRAGMA table_info(worker_runs)")
        .map_err(|source| StorageError::Schema {
            table: "worker_runs".to_string(),
            source,
        })?
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|source| StorageError::Schema {
            table: "worker_runs".to_string(),
            source,
        })?
        .filter_map(Result::ok)
        .any(|name| name == "agent_preset_id");
    if !has_column {
        connection
            .execute(
                "ALTER TABLE worker_runs ADD COLUMN agent_preset_id TEXT",
                [],
            )
            .map_err(|source| StorageError::Schema {
                table: "worker_runs".to_string(),
                source,
            })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 老库 missions.payload 带 `target_type` 时必须被剥掉，否则
    /// `Mission`（`deny_unknown_fields`）整表读不出来。
    #[test]
    fn legacy_mission_payload_loses_target_type() {
        let connection = Connection::open_in_memory()
            .unwrap_or_else(|error| panic!("in-memory db must open: {error}"));
        init(&connection).unwrap_or_else(|error| panic!("schema init must succeed: {error}"));
        connection
            .execute(
                "INSERT INTO missions (id, project_id, payload, created_at)
                 VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![
                    "mission_legacy",
                    "proj_legacy",
                    r#"{"id":"mission_legacy","project_id":"proj_legacy","user_goal":"g",
                        "target":{"url":"https://x.test"},"target_type":"source"}"#,
                    "2026-08-24T12:00:00.123456Z"
                ],
            )
            .unwrap_or_else(|error| panic!("legacy row must insert: {error}"));

        strip_mission_target_type(&connection)
            .unwrap_or_else(|error| panic!("migration must succeed: {error}"));

        let payload: String = connection
            .query_row(
                "SELECT payload FROM missions WHERE id = 'mission_legacy'",
                [],
                |row| row.get(0),
            )
            .unwrap_or_else(|error| panic!("row must remain readable: {error}"));
        assert!(
            !payload.contains("target_type"),
            "target_type must be stripped, got: {payload}"
        );
        // 幂等：第二次运行不改写内容。
        strip_mission_target_type(&connection)
            .unwrap_or_else(|error| panic!("second migration must succeed: {error}"));
        let again: String = connection
            .query_row(
                "SELECT payload FROM missions WHERE id = 'mission_legacy'",
                [],
                |row| row.get(0),
            )
            .unwrap_or_else(|error| panic!("row must remain readable: {error}"));
        assert_eq!(payload, again, "migration must be idempotent");
    }

    #[test]
    fn run_scoped_membership_matches_python_set() {
        // 关键成员抽查 + 数量守护：集合漂移（新增/删除表）必须先红这里。
        assert!(is_run_scoped("agent_tasks"));
        assert!(is_run_scoped("branches"));
        assert!(is_run_scoped("audit_events"));
        assert!(is_run_scoped("execution_jobs"));
        assert!(!is_run_scoped("audit_runs"));
        assert!(!is_run_scoped("missions"));
        assert!(!is_run_scoped("evidence"));
        assert!(!is_run_scoped("findings"));
        assert!(!is_run_scoped("tool_invocations"));
        assert_eq!(
            TABLES.len(),
            46,
            "表集合 = Python _TABLES(46) + finding_retests - retrieval_chunks"
        );
        assert_eq!(
            RUN_SCOPED.len(),
            26,
            "run-scoped 集合必须与 Python _RUN_SCOPED 一致（去 retrieval_chunks）"
        );
        // run-scoped 表必然都在表集合里。
        for table in RUN_SCOPED {
            assert!(TABLES.contains(&table), "{table} 必须在 TABLES 中");
        }
    }

    #[test]
    fn init_creates_all_tables_and_indexes() {
        let connection = Connection::open_in_memory().expect("内存库必须可创建");
        init(&connection).expect("全量 DDL 必须成功");
        let count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
                [],
                |row| row.get(0),
            )
            .expect("sqlite_master 查询必须成功");
        assert_eq!(
            count, 57,
            "46 张兼容表 + 5 张 Intelligence 表 + 6 张 Worker/Skill 表必须全部创建"
        );
        let index_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name LIKE 'idx_%'",
                [],
                |row| row.get(0),
            )
            .expect("sqlite_master 查询必须成功");
        assert_eq!(
            index_count, 20,
            "兼容 + Intelligence + 外部 Worker 索引必须全部创建"
        );
        // 幂等：重复 init 不报错、不重复建。
        init(&connection).expect("重复 init 必须幂等");
    }

    #[test]
    fn generic_ddl_column_layout_matches_python() {
        let connection = Connection::open_in_memory().expect("内存库必须可创建");
        connection
            .execute(&generic_table_ddl("agent_tasks"), [])
            .expect("DDL 必须可执行");
        let run_scoped_columns: Vec<String> = connection
            .prepare("PRAGMA table_info(agent_tasks)")
            .expect("pragma 必须可执行")
            .query_map([], |row| row.get::<_, String>(1))
            .expect("列名必须可读")
            .collect::<Result<Vec<_>, _>>()
            .expect("列名必须完整");
        assert_eq!(
            run_scoped_columns,
            ["seq", "id", "project_id", "run_id", "payload", "created_at"],
            "run-scoped 表列序必须与 Python 一致"
        );

        let connection = Connection::open_in_memory().expect("内存库必须可创建");
        connection
            .execute(&generic_table_ddl("findings"), [])
            .expect("DDL 必须可执行");
        let plain_columns: Vec<String> = connection
            .prepare("PRAGMA table_info(findings)")
            .expect("pragma 必须可执行")
            .query_map([], |row| row.get::<_, String>(1))
            .expect("列名必须可读")
            .collect::<Result<Vec<_>, _>>()
            .expect("列名必须完整");
        assert_eq!(
            plain_columns,
            ["seq", "id", "project_id", "payload", "created_at"],
            "普通表列序必须与 Python 一致"
        );
    }
}
