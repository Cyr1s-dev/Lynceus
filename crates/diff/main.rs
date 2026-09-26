//! 差分对拍 harness（阶段 1 双跑验证）。
//!
//! 用法：
//! - `diff dump <db.sqlite3> <out.json>`：把 SQLite 数据库 dump 成
//!   规范化 JSON（格式见 `storage::dump` 模块文档）；
//! - `diff compare <a.json> <b.json>`：语义级深度比对两份 JSON，
//!   报告首个差异的路径与两侧取值；
//! - `diff fixture <out.sqlite3>`：执行固定输入的确定性仓储操作
//!   序列（`scripts/parity_fixture.py` 的 Rust 镜像），产出对拍用数据库。
//!
//! 退出码：`0` 一致/成功；`1` 发现差异；`2` 用法或运行错误。

// 应用边界：CLI 的 stdout 输出即产品本身，库 crate 仍受 print_stdout 约束。
#![allow(clippy::print_stdout)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::path::PathBuf;
use std::process::ExitCode;

use serde_json::Value;

#[derive(Debug, thiserror::Error)]
enum RunError {
    #[error(
        "用法: diff dump <db.sqlite3> <out.json> | diff compare <a.json> <b.json> | diff fixture <out.sqlite3>"
    )]
    Usage,
    #[error("无法读取 {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("无法写入 {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("解析 {path} 失败: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("{source}")]
    Storage {
        #[source]
        source: storage::StorageError,
    },
    #[error("fixture 语义校验失败: {0}")]
    FixtureSemantics(String),
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(exit_code) => exit_code,
        Err(error) => {
            eprintln!("错误: {error}");
            ExitCode::from(2)
        }
    }
}

fn run(args: &[String]) -> Result<ExitCode, RunError> {
    match args {
        [command, first, second] if command == "dump" => {
            dump_command(first, second)?;
            Ok(ExitCode::SUCCESS)
        }
        [command, first, second] if command == "compare" => compare_command(first, second),
        [command, first] if command == "fixture" => {
            fixture_command(first)?;
            Ok(ExitCode::SUCCESS)
        }
        _ => Err(RunError::Usage),
    }
}

fn dump_command(database: &str, output: &str) -> Result<(), RunError> {
    let dump = storage::dump_database(std::path::Path::new(database))
        .map_err(|source| RunError::Storage { source })?;
    // 紧凑格式即规范格式。键序确定性来自 dump.rs 的构造侧保证（列按字母
    // 序插入，serde_json 启用 preserve_order 后序列化保持插入序），与
    // Python 侧 json.dumps(sort_keys=True, separators=(",", ":")) 逐字节一致。
    let serialized = format!("{dump}\n");
    std::fs::write(output, serialized).map_err(|source| RunError::Write {
        path: PathBuf::from(output),
        source,
    })?;
    println!("已写出规范化 dump: {output}");
    Ok(())
}

fn fixture_command(database: &str) -> Result<(), RunError> {
    let repo =
        storage::SqliteRepository::open(database).map_err(|source| RunError::Storage { source })?;
    let snapshot = storage::fixture::build(&repo).map_err(|source| RunError::Storage { source })?;
    // WAL checkpoint：确保主数据库文件自包含（对拍读的是单文件字节），
    // 与 Python 侧 fixture 脚本的收尾行为一致。
    drop(repo);
    storage::fixture::verify(&snapshot).map_err(RunError::FixtureSemantics)?;
    println!("已构建对拍 fixture 数据库: {database}");
    Ok(())
}

fn compare_command(left: &str, right: &str) -> Result<ExitCode, RunError> {
    let left_value = read_json(left)?;
    let right_value = read_json(right)?;
    match first_difference(&left_value, &right_value, "$") {
        None => {
            println!("一致: 两份 JSON 语义相同");
            Ok(ExitCode::SUCCESS)
        }
        Some(difference) => {
            println!("发现差异: {difference}");
            Ok(ExitCode::from(1))
        }
    }
}

fn read_json(path: &str) -> Result<Value, RunError> {
    let text = std::fs::read_to_string(path).map_err(|source| RunError::Read {
        path: PathBuf::from(path),
        source,
    })?;
    serde_json::from_str(&text).map_err(|source| RunError::Parse {
        path: PathBuf::from(path),
        source,
    })
}

/// 递归找出首个差异，返回“路径: 左值 != 右值”形式的报告。
///
/// 对象按键的并集（字母序）遍历，缺失键报告为 only in left/right；
/// 数组按下标逐元素比对，长度不等先报长度。`serde_json` 解析后的对象键序
/// 本身已规范化，两侧等价对象在遍历顺序上不会产生假差异。
fn first_difference(left: &Value, right: &Value, path: &str) -> Option<String> {
    match (left, right) {
        (Value::Object(left_map), Value::Object(right_map)) => {
            let mut keys: Vec<&str> = left_map
                .keys()
                .chain(right_map.keys())
                .map(String::as_str)
                .collect();
            keys.sort_unstable();
            keys.dedup();
            for key in keys {
                let child_path = format!("{path}.{key}");
                match (left_map.get(key), right_map.get(key)) {
                    (Some(left_value), Some(right_value)) => {
                        if let Some(difference) =
                            first_difference(left_value, right_value, &child_path)
                        {
                            return Some(difference);
                        }
                    }
                    (Some(left_value), None) => {
                        return Some(format!("{child_path}: 仅左侧存在: {left_value}"));
                    }
                    (None, Some(right_value)) => {
                        return Some(format!("{child_path}: 仅右侧存在: {right_value}"));
                    }
                    // keys 来自两侧并集，此分支按构造不可能出现。
                    (None, None) => {}
                }
            }
            None
        }
        (Value::Array(left_items), Value::Array(right_items)) => {
            if left_items.len() != right_items.len() {
                return Some(format!(
                    "{path}: 数组长度 {} != {}",
                    left_items.len(),
                    right_items.len()
                ));
            }
            for (index, (left_value, right_value)) in
                left_items.iter().zip(right_items.iter()).enumerate()
            {
                let child_path = format!("{path}[{index}]");
                if let Some(difference) = first_difference(left_value, right_value, &child_path) {
                    return Some(difference);
                }
            }
            None
        }
        _ => (left != right).then(|| format!("{path}: {left} != {right}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn first_diff(left: &Value, right: &Value) -> Option<String> {
        first_difference(left, right, "$")
    }

    #[test]
    fn identical_values_have_no_difference() {
        assert_eq!(
            first_diff(
                &json!({"a": [1, 2], "b": "x"}),
                &json!({"b": "x", "a": [1, 2]})
            ),
            None,
            "键序不同但语义相同的对象必须判定一致"
        );
    }

    #[test]
    fn scalar_difference_reports_path_and_values() {
        let difference = first_diff(
            &json!({"tables": {"missions": [{"status": "running"}]}}),
            &json!({"tables": {"missions": [{"status": "completed"}]}}),
        )
        .expect("状态不同的对象必须报告差异");
        assert!(
            difference.contains("$.tables.missions[0].status"),
            "报告应包含差异路径: {difference}"
        );
        assert!(difference.contains("running") && difference.contains("completed"));
    }

    #[test]
    fn missing_key_reports_side() {
        let difference = first_diff(&json!({"a": 1, "b": 2}), &json!({"a": 1}));
        assert!(matches!(&difference, Some(d) if d.contains("$.b") && d.contains("仅左侧存在")));
    }

    #[test]
    fn array_length_difference_is_reported() {
        let difference = first_diff(&json!([1, 2, 3]), &json!([1, 2]));
        assert!(matches!(&difference, Some(d) if d.contains("长度 3 != 2")));
    }

    #[test]
    fn usage_error_for_unknown_or_missing_arguments() {
        assert!(run(&[]).is_err());
        assert!(run(&["frobnicate".to_string()]).is_err());
        assert!(run(&["dump".to_string()]).is_err());
    }
}
