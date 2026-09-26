//! 存储层错误 —— dump 与仓储操作共用的领域错误。

use std::path::PathBuf;

/// 存储操作失败的原因。
///
/// 变体粒度对齐 Python 侧行为：SQL 语句失败携带表名与底层
/// `rusqlite::Error`（Python 侧透传 `sqlite3.Error`），payload 解析失败
/// 携带 `serde_json::Error`（Python 侧是 pydantic `ValidationError`）。
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    /// 数据库文件无法打开（不存在、被锁、非 SQLite 文件）。
    #[error("cannot open database {path}: {source}")]
    Open {
        /// 数据库文件路径。
        path: PathBuf,
        /// 底层 rusqlite 错误。
        #[source]
        source: rusqlite::Error,
    },
    /// 数据库父目录无法创建。
    #[error("cannot create parent directory of {path}: {source}")]
    Directory {
        /// 数据库文件路径。
        path: PathBuf,
        /// 底层 IO 错误。
        #[source]
        source: std::io::Error,
    },
    /// 初始化 PRAGMA 失败（`journal_mode` / `busy_timeout` / `foreign_keys`）。
    #[error("applying pragma {pragma} failed: {source}")]
    Pragma {
        /// PRAGMA 名称。
        pragma: &'static str,
        /// 底层 rusqlite 错误。
        #[source]
        source: rusqlite::Error,
    },
    /// 读取 schema（`sqlite_master`）或执行 DDL 失败。
    #[error("schema operation on {table} failed: {source}")]
    Schema {
        /// 表名。
        table: String,
        /// 底层 rusqlite 错误。
        #[source]
        source: rusqlite::Error,
    },
    /// 查询语句失败。
    #[error("query on {table} failed: {source}")]
    Query {
        /// 表名。
        table: String,
        /// 底层 rusqlite 错误。
        #[source]
        source: rusqlite::Error,
    },
    /// 写语句（INSERT / UPSERT / DELETE / 事务边界）失败。
    #[error("write on {table} failed: {source}")]
    Write {
        /// 表名。
        table: String,
        /// 底层 rusqlite 错误。
        #[source]
        source: rusqlite::Error,
    },
    /// TEXT 列含非法 UTF-8，无法进入 JSON。
    #[error("column {table}.{column} contains invalid UTF-8 text: {source}")]
    InvalidUtf8 {
        /// 表名。
        table: String,
        /// 列名。
        column: String,
        /// 底层 UTF-8 解码错误。
        #[source]
        source: std::str::Utf8Error,
    },
    /// REAL 列为非有限值（Inf/NaN），JSON 无法表示。
    #[error("column {table}.{column} holds non-finite REAL {value}; not representable in JSON")]
    NonFiniteReal {
        /// 表名。
        table: String,
        /// 列名。
        column: String,
        /// 非有限值。
        value: f64,
    },
    /// JSON 序列化失败（dump 的行、仓储的 payload 两条路径共用）。
    #[error("serializing JSON for {table} failed: {source}")]
    Serialize {
        /// 表名。
        table: String,
        /// 底层 `serde_json` 错误。
        #[source]
        source: serde_json::Error,
    },
    /// payload 列无法反序列化为实体（Python `model_validate_json` 失败的对应）。
    #[error("payload in {table} is not valid for the entity: {source}")]
    Decode {
        /// 表名。
        table: String,
        /// 底层 `serde_json` 错误。
        #[source]
        source: serde_json::Error,
    },
    /// `increment_run_steps` 收到负 delta（Python `ValueError` 的对应）。
    #[error("run step delta must be non-negative: {delta}")]
    NegativeStepDelta {
        /// 被拒绝的负增量。
        delta: i64,
    },
    /// 仓储操作收到无法安全解释的参数。
    #[error("invalid {entity}: {message}")]
    InvalidArgument {
        /// 参数所属的领域实体。
        entity: &'static str,
        /// 有界错误说明。
        message: String,
    },
    /// append-only 实体重复插入（Python `_insert(append_only=True)` 抛出
    /// `ValueError(f"{table[:-1]} {id} already exists; entity is append-only")`
    /// 的类型化对应）。
    #[error("{entity} {id} already exists; entity is append-only")]
    AppendOnlyConflict {
        /// 实体名单数名（Python 侧 `table[:-1]`）。
        entity: String,
        /// 重复的实体 id。
        id: String,
    },
    /// `commit_solver_result` 事务内的约束冲突（Python `except
    /// sqlite3.IntegrityError: raise ValueError("solver result commit
    /// failed")` 的对应——约束细节被有意吞掉，报文是固定字符串）。
    #[error("solver result commit failed")]
    SolverCommitConflict,
    /// 期望存在的实体缺失（Python `KeyError(f"decision gate not found: {id}")`
    /// 等的类型化对应）。
    #[error("{entity} not found: {id}")]
    NotFound {
        /// 实体描述。
        entity: &'static str,
        /// 缺失的实体 id。
        id: String,
    },
    /// 持有存储锁的线程 panic，互斥锁中毒。
    #[error("storage mutex poisoned by a panicked thread")]
    Poisoned,
}
