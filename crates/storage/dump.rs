//! 差分对拍用的规范化 SQLite → JSON dump。
//!
//! 差分 harness 把同一 SQLite 文件喂给 Python 与 Rust 引擎并比对 JSON
//! 输出，因此两侧必须遵守同一份规范化序列化。本模块定义 Rust 侧规范：
//!
//! - 顶层对象：`{"format": 1, "tables": {...}}`；
//! - 表：`sqlite_master` 中 Python/Rust 共有的用户表（排除内部
//!   `sqlite_*` 与 Rust-native `intel_*` / 外部 Worker 扩展表），按名称排序；
//! - 行：以列名为键的 JSON 对象，键序由 `serde_json` 的 `BTreeMap` 语义
//!   统一为字母序（Python 侧用 `json.dumps(sort_keys=True)` 对齐）；
//! - 行序：按行的序列化字符串字典序排序，得到与物理行序无关的确定性
//!   全序（两侧插入顺序不同也产出相同 dump）；
//! - 值：`NULL` → `null`，`INTEGER` → 数字，`REAL` → 数字（仅有限值，
//!   SQLite 的 `Inf` 是 dump 错误而非静默标记），`TEXT` → 字符串（非法
//!   UTF-8 是错误），`BLOB` → `{"blob_hex": "<小写十六进制>"}`，使任意
//!   字节在 JSON 往返中无歧义存活。

use std::path::Path;

use rusqlite::Connection;
use rusqlite::OpenFlags;
use rusqlite::types::ValueRef;
use serde_json::{Map, Number, Value};

use crate::error::StorageError;

/// 把整个 SQLite 数据库文件 dump 成规范化 JSON。
///
/// 以只读模式打开，绝不修改数据库。规范格式见[模块文档](self)。
///
/// # Errors
/// - [`StorageError::Open`]：文件无法打开；
/// - [`StorageError::Schema`] / [`StorageError::Query`]：SQL 执行失败；
/// - [`StorageError::InvalidUtf8`] / [`StorageError::NonFiniteReal`]：
///   存储值无法进入规范化 JSON。
pub fn dump_database(path: &Path) -> Result<Value, StorageError> {
    let connection =
        Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|source| {
            StorageError::Open {
                path: path.to_path_buf(),
                source,
            }
        })?;
    dump_connection(&connection)
}

fn dump_connection(connection: &Connection) -> Result<Value, StorageError> {
    let mut tables = Map::new();
    for table in user_tables(connection)? {
        let rows = dump_table(connection, &table)?;
        tables.insert(table, Value::Array(rows));
    }
    let mut root = Map::new();
    root.insert("format".to_string(), Value::Number(Number::from(1u32)));
    root.insert("tables".to_string(), Value::Object(tables));
    Ok(Value::Object(root))
}

fn user_tables(connection: &Connection) -> Result<Vec<String>, StorageError> {
    const TABLES_SQL: &str = "SELECT name FROM sqlite_master \
        WHERE type = 'table' AND name NOT LIKE 'sqlite_%' \
        AND name NOT LIKE 'intel_%' \
        AND name NOT IN ('worker_runs', 'worker_invocations', 'worker_runtime_profiles', 'agent_presets', 'skill_usage') \
        ORDER BY name";
    let schema_error = |source: rusqlite::Error| StorageError::Schema {
        table: "sqlite_master".to_string(),
        source,
    };
    let mut statement = connection.prepare(TABLES_SQL).map_err(schema_error)?;
    let names = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(schema_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(schema_error)?;
    Ok(names)
}

fn dump_table(connection: &Connection, table: &str) -> Result<Vec<Value>, StorageError> {
    // 表名来自 sqlite_master，无法作为绑定参数；双引号内双写转义是 SQL
    // 标准的标识符转义，使插值后的名字仍是一个标识符 token。
    let sql = format!("SELECT * FROM {}", quote_identifier(table));
    let query_error = |source: rusqlite::Error| StorageError::Query {
        table: table.to_string(),
        source,
    };
    let mut statement = connection.prepare(&sql).map_err(query_error)?;
    // column_names 借用 statement，先克隆成自有 String 再发起查询。
    let columns: Vec<String> = statement
        .column_names()
        .iter()
        .map(|name| (*name).to_string())
        .collect();
    // 键序字母序是冻结规范的一部分：行对象必须按列名字母序插入，
    // 使 `serde_json`（preserve_order）序列化出的键序与 Python 侧
    // `json.dumps(sort_keys=True)` 逐字节一致。物理列序因此不得泄漏进 dump。
    let mut sorted_columns = columns.clone();
    sorted_columns.sort();
    let mut rows = statement.query([]).map_err(query_error)?;
    let mut serialized_rows: Vec<(String, Value)> = Vec::new();
    while let Some(row) = rows.next().map_err(query_error)? {
        let mut object = Map::new();
        for column in &sorted_columns {
            let index = columns
                .iter()
                .position(|name| name == column)
                .unwrap_or(columns.len());
            let value = column_value(row.get_ref(index).map_err(query_error)?, table, column)?;
            object.insert(column.clone(), value);
        }
        let value = Value::Object(object);
        let serialized =
            serde_json::to_string(&value).map_err(|source| StorageError::Serialize {
                table: table.to_string(),
                source,
            })?;
        serialized_rows.push((serialized, value));
    }
    serialized_rows.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(serialized_rows
        .into_iter()
        .map(|(_, value)| value)
        .collect())
}

fn column_value(value: ValueRef<'_>, table: &str, column: &str) -> Result<Value, StorageError> {
    Ok(match value {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(integer) => Value::Number(Number::from(integer)),
        ValueRef::Real(real) => {
            let number = Number::from_f64(real).ok_or(StorageError::NonFiniteReal {
                table: table.to_string(),
                column: column.to_string(),
                value: real,
            })?;
            Value::Number(number)
        }
        ValueRef::Text(text) => Value::String(
            std::str::from_utf8(text)
                .map_err(|source| StorageError::InvalidUtf8 {
                    table: table.to_string(),
                    column: column.to_string(),
                    source,
                })?
                .to_string(),
        ),
        ValueRef::Blob(blob) => {
            let mut object = Map::new();
            object.insert("blob_hex".to_string(), Value::String(to_hex(blob)));
            Value::Object(object)
        }
    })
}

fn quote_identifier(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// 小写十六进制编码。与 evidence 内的同名私有函数重复是刻意
/// 的：两处关注点不同（BLOB 序列化 vs 工件指纹），为 8 行代码引入共享
/// crate 过早；出现第三个使用方时再上提。
fn to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    fn create_database(sql: &[&str]) -> (TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("系统临时目录应可创建");
        let path = dir.path().join("test.sqlite3");
        let connection = Connection::open(&path).expect("新建数据库文件应成功");
        for statement in sql {
            connection
                .execute(statement, [])
                .expect("测试建表/插入语句应成功");
        }
        (dir, path)
    }

    #[test]
    fn quote_identifier_doubles_inner_quotes() {
        assert_eq!(quote_identifier("missions"), "\"missions\"");
        assert_eq!(quote_identifier("mis\"chief"), "\"mis\"\"chief\"");
    }

    #[test]
    fn dump_covers_all_value_kinds() {
        let (_dir, path) = create_database(&[
            "CREATE TABLE findings (id INTEGER, title TEXT, score REAL, payload BLOB, note TEXT)",
            "INSERT INTO findings VALUES (1, 'sql-injection', 9.5, x'deadbeef', NULL)",
        ]);
        let dump = dump_database(&path).expect("合法数据库应 dump 成功");
        assert_eq!(
            dump,
            json!({
                "format": 1,
                "tables": {
                    "findings": [
                        {
                            "id": 1,
                            "title": "sql-injection",
                            "score": 9.5,
                            "payload": {"blob_hex": "deadbeef"},
                            "note": null
                        }
                    ]
                }
            })
        );
    }

    #[test]
    fn dump_is_independent_of_physical_row_order() {
        let (dir_a, path_a) = create_database(&[
            "CREATE TABLE t (id INTEGER, label TEXT)",
            "INSERT INTO t VALUES (1, 'a'), (2, 'b'), (3, 'c')",
        ]);
        let (dir_b, path_b) = create_database(&[
            "CREATE TABLE t (id INTEGER, label TEXT)",
            "INSERT INTO t VALUES (3, 'c'), (1, 'a'), (2, 'b')",
        ]);
        let dump_a = dump_database(&path_a).expect("dump 不应失败");
        let dump_b = dump_database(&path_b).expect("dump 不应失败");
        assert_eq!(dump_a, dump_b, "行序规范化必须使插入顺序不影响 dump 结果");
        drop(dir_a);
        drop(dir_b);
    }

    #[test]
    fn dump_orders_tables_and_rows_deterministically() {
        let (_dir, path) = create_database(&[
            "CREATE TABLE zeta (id INTEGER)",
            "CREATE TABLE alpha (id INTEGER)",
            "INSERT INTO zeta VALUES (2), (1)",
            "INSERT INTO alpha VALUES (10), (9)",
        ]);
        let dump = dump_database(&path).expect("dump 不应失败");
        let tables = dump["tables"].as_object().expect("tables 应为对象");
        let names: Vec<&String> = tables.keys().collect();
        assert_eq!(names, ["alpha", "zeta"], "表必须按名称排序");
        let alpha_rows = dump["tables"]["alpha"].as_array().expect("行数组");
        // 字典序而非数值序：`{"id":10}` 的字符串序在 `{"id":9}` 之前
        // （'1' < '9'）。行序只需要确定性全序，与数值大小无关。
        assert_eq!(alpha_rows[0]["id"], json!(10), "行必须按序列化字符串排序");
        assert_eq!(alpha_rows[1]["id"], json!(9), "行必须按序列化字符串排序");
    }

    #[test]
    fn dump_excludes_internal_sqlite_tables() {
        let (_dir, path) = create_database(&["CREATE TABLE visible (id INTEGER)"]);
        let dump = dump_database(&path).expect("dump 不应失败");
        let tables = dump["tables"].as_object().expect("tables 应为对象");
        assert!(
            tables.keys().all(|name| !name.starts_with("sqlite_")),
            "内部 sqlite_* 表不得进入 dump"
        );
    }

    #[test]
    fn non_finite_real_is_an_error() {
        // 9e999 溢出为 REAL Inf，JSON 无法表示，必须显式报错而非静默丢弃。
        let (_dir, path) = create_database(&[
            "CREATE TABLE t (score REAL)",
            "INSERT INTO t VALUES (9e999)",
        ]);
        let error = dump_database(&path).expect_err("Inf 必须报错");
        assert!(matches!(error, StorageError::NonFiniteReal { .. }));
    }

    #[test]
    fn invalid_utf8_text_is_an_error() {
        // CAST 到 TEXT 不校验 UTF-8，模拟历史数据中的脏字节。
        let (_dir, path) = create_database(&[
            "CREATE TABLE t (value TEXT)",
            "INSERT INTO t VALUES (CAST(x'FFFE' AS TEXT))",
        ]);
        let error = dump_database(&path).expect_err("非法 UTF-8 必须报错");
        assert!(matches!(error, StorageError::InvalidUtf8 { .. }));
    }

    #[test]
    fn missing_database_file_is_an_error() {
        let dir = tempfile::tempdir().expect("系统临时目录应可创建");
        let path = dir.path().join("absent.sqlite3");
        let error = dump_database(&path).expect_err("不存在的文件必须报错");
        assert!(matches!(error, StorageError::Open { .. }));
    }
}
