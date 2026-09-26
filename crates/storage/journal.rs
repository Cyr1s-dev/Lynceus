//! Mission 级追加只读 swarm 操作日志 —— `server/core/workers/journal.py`
//! 的移植。
//!
//! 全部 worker 写入串行化进每个 Mission 一个的 JSONL 文件。日志是操作的
//! 详尽真值来源；ContextPack 与轨迹摘要是派生视图，绝不替代或截断它。
//!
//! 并发模型：Python 用每路径 `asyncio.Lock` + `asyncio.to_thread` 把文件
//! 操作移出事件循环；Rust 侧仓储层整体是同步的，这里用一把全局互斥锁
//! 串行化（正确性等价，粒度更粗但临界区只有小文件读写）。

use std::collections::HashMap;
use std::fs;
use std::fs::OpenOptions;
use std::io::BufRead;
use std::io::BufReader;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::Deserialize;
use serde::Serialize;
use serde_json::Map;
use serde_json::Value;

use models::Timestamp;
use models::new_id;
use models::utcnow;
use models::{BranchId, MissionId, ProjectId, RunId, TaskId};

use crate::redaction::redact_value;

/// Python `_DEFAULT_MISSION_WORKSPACE_ROOT`。
const DEFAULT_MISSION_WORKSPACE_ROOT: &str = "data/missions";

/// 操作日志读取失败的原因。
#[derive(Debug, thiserror::Error)]
pub enum JournalError {
    /// 文件读取失败。
    #[error("reading operation journal {path} failed: {source}")]
    Read {
        /// 日志文件路径。
        path: PathBuf,
        /// 底层 IO 错误。
        #[source]
        source: std::io::Error,
    },
    /// 文件写入失败（含父目录创建失败）。
    #[error("writing operation journal {path} failed: {source}")]
    Write {
        /// 日志文件路径。
        path: PathBuf,
        /// 底层 IO 错误。
        #[source]
        source: std::io::Error,
    },
    /// 行无法解析为记录（读取路径跳过，此处仅由 `parse_line` 显式暴露）。
    #[error("journal line is not a valid record: {source}")]
    Parse {
        /// 底层解析错误。
        #[source]
        source: serde_json::Error,
    },
    /// 日志目录请求非法（Python `ValueError` 的对应）。
    #[error("{message}")]
    InvalidLogDir {
        /// Python 侧错误文本。
        message: String,
    },
    /// 持有日志锁的线程 panic，互斥锁中毒。
    #[error("journal mutex poisoned by a panicked thread")]
    Poisoned,
}

fn default_record_id() -> String {
    new_id("swarmop")
}

fn default_sequence() -> i64 {
    0
}

fn default_role() -> String {
    "agent".to_string()
}

/// 一条完整、已脱敏、外部可观察的 worker 操作（`SwarmOperationRecord`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SwarmOperationRecord {
    /// 记录标识符。
    #[serde(default = "default_record_id")]
    pub id: String,
    /// 文件内单调序号（落盘时由 journal 分配）。
    #[serde(default = "default_sequence")]
    pub sequence: i64,
    /// 所属 Project。
    pub project_id: ProjectId,
    /// 所属 Mission。
    #[serde(default)]
    pub mission_id: Option<MissionId>,
    /// 所属 Run。
    pub run_id: RunId,
    /// 所属 Branch。
    #[serde(default)]
    pub branch_id: Option<BranchId>,
    /// 关联 Task。
    #[serde(default)]
    pub task_id: Option<TaskId>,
    /// 执行 worker。
    #[serde(default)]
    pub worker_id: Option<String>,
    /// Provider id。
    #[serde(default)]
    pub provider_id: Option<String>,
    /// 模型名。
    #[serde(default)]
    pub model: Option<String>,
    /// 角色。
    #[serde(default = "default_role")]
    pub role: String,
    /// 人类可读的执行者标签。
    pub actor_label: String,
    /// 操作类型。
    pub operation_type: String,
    /// 操作条目。
    pub entry: String,
    /// 结构化载荷（键序 = 插入序）。
    #[serde(default)]
    pub payload: Map<String, Value>,
    /// 来源记录 ID。
    #[serde(default)]
    pub source_ids: Vec<String>,
    /// 创建时间。
    #[serde(default = "models::utcnow")]
    pub created_at: Timestamp,
}

impl SwarmOperationRecord {
    /// 以 Python 默认值构造（`SwarmOperationRecord(project_id=...,
    /// run_id=..., actor_label=..., operation_type=..., entry=...)`）。
    #[must_use]
    pub fn new(
        project_id: ProjectId,
        run_id: RunId,
        actor_label: String,
        operation_type: String,
        entry: String,
    ) -> Self {
        Self {
            id: default_record_id(),
            sequence: default_sequence(),
            project_id,
            mission_id: None,
            run_id,
            branch_id: None,
            task_id: None,
            worker_id: None,
            provider_id: None,
            model: None,
            role: default_role(),
            actor_label,
            operation_type,
            entry,
            payload: Map::new(),
            source_ids: Vec::new(),
            created_at: utcnow(),
        }
    }
}

/// 日志窗口读取参数（Python `read_page` 的关键字参数组）。
#[derive(Debug, Clone, Default)]
pub struct PageQuery {
    /// 只取 `sequence > after_sequence` 的记录。
    pub after_sequence: i64,
    /// 返回条数上限（钳制到 `[1, 1000]`）。
    pub limit: i64,
    /// 按 Run 过滤。
    pub run_id: Option<String>,
    /// 按 Branch 过滤。
    pub branch_id: Option<String>,
    /// 按 Task 过滤。
    pub task_id: Option<String>,
    /// 按 worker 过滤。
    pub worker_id: Option<String>,
    /// 按操作类型过滤。
    pub operation_type: Option<String>,
    /// 为真时取尾部（最近）而非头部。
    pub tail: bool,
}

impl PageQuery {
    /// `after_sequence=0, limit=200` 的默认窗口。
    #[must_use]
    pub fn new() -> Self {
        Self {
            limit: 200,
            ..Self::default()
        }
    }
}

/// 从 Mission 日志读出的一页无损、游标可寻址的记录（`SwarmOperationPage`）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SwarmOperationPage {
    /// 本页记录。
    #[serde(default)]
    pub records: Vec<SwarmOperationRecord>,
    /// 请求游标。
    #[serde(default)]
    pub after_sequence: i64,
    /// 下一页游标（本页最后一条的 sequence）。
    #[serde(default)]
    pub next_after_sequence: i64,
    /// 过滤后的匹配总数。
    #[serde(default)]
    pub total: i64,
    /// 是否还有更多记录。
    #[serde(default)]
    pub has_more: bool,
    /// 全文件最新 sequence（不受过滤影响）。
    #[serde(default)]
    pub latest_sequence: i64,
}

/// 每路径的序列号缓存状态。
#[derive(Default)]
struct PathState {
    /// 已知行数（`None` = 尚未统计）。
    lines: Option<i64>,
}

/// 把全部 worker 写入串行化为每 Mission 一个持久 JSONL 文件
/// （`SwarmOperationJournal`）。
#[derive(Default)]
pub struct SwarmOperationJournal {
    /// path → 行数缓存；互斥锁兼作 Python 每路径 `asyncio.Lock` 的串行化。
    states: Mutex<HashMap<PathBuf, PathState>>,
}

impl SwarmOperationJournal {
    /// 日志文件名（Python `filename` 类属性）。
    pub const FILENAME: &'static str = "swarm-operations.jsonl";

    /// 追加一条记录（分配 sequence + 脱敏 payload）并返回文件路径。
    ///
    /// # Errors
    /// 见 [`JournalError::Read`] / [`JournalError::Write`]。
    pub fn append(
        &self,
        log_dir: &Path,
        record: SwarmOperationRecord,
    ) -> Result<PathBuf, JournalError> {
        let resolved = normalize(log_dir);
        let path = resolved.join(Self::FILENAME);
        let mut states = self.lock_states()?;
        let lines = if let Some(state) = states.get(&path) {
            state.lines
        } else {
            let counted = line_count(&path)?;
            states.insert(
                path.clone(),
                PathState {
                    lines: Some(counted),
                },
            );
            Some(counted)
        };
        let sequence = lines.unwrap_or_default() + 1;
        let mut persisted = record;
        persisted.sequence = sequence;
        persisted.payload = match redact_value(&Value::Object(persisted.payload)) {
            Value::Object(map) => map,
            _ => Map::new(),
        };
        append_line(&path, &persisted)?;
        if let Some(state) = states.get_mut(&path) {
            state.lines = Some(sequence);
        }
        Ok(path)
    }

    /// 读取验证过的记录，不重写、不摘要其 payload（Python `read_page`）。
    ///
    /// # Errors
    /// 见 [`JournalError::Read`]。
    pub fn read_page(
        &self,
        log_dir: &Path,
        query: PageQuery,
    ) -> Result<SwarmOperationPage, JournalError> {
        let resolved = normalize(log_dir);
        let path = resolved.join(Self::FILENAME);
        let bounded_limit = query.limit.clamp(1, 1000);
        let _states = self.lock_states()?;
        read_page(
            &path,
            &PageQuery {
                limit: bounded_limit,
                ..query
            },
        )
    }

    /// 为模型 `ContextPack` 选择精确的近期/相关记录
    /// （Python `read_context_window`）。
    ///
    /// 选择是确定性的、基于元数据的；单条记录绝不被摘要或截断，被省略的
    /// 记录仍可通过游标 API 获取。
    ///
    /// # Errors
    /// 见 [`JournalError::Read`]。
    pub fn read_context_window(
        &self,
        log_dir: &Path,
        run_id: &str,
        branch_id: Option<&str>,
        task_id: Option<&str>,
        max_records: usize,
        max_chars: usize,
    ) -> Result<(Vec<SwarmOperationRecord>, Map<String, Value>), JournalError> {
        let limit = i64::try_from(max_records).unwrap_or(i64::MAX);
        let run_page = self.read_page(
            log_dir,
            PageQuery {
                run_id: Some(run_id.to_string()),
                limit,
                tail: true,
                ..PageQuery::new()
            },
        )?;
        let mut candidates = run_page.records.clone();
        if let Some(branch_id) = branch_id {
            let branch_page = self.read_page(
                log_dir,
                PageQuery {
                    run_id: Some(run_id.to_string()),
                    branch_id: Some(branch_id.to_string()),
                    limit,
                    tail: true,
                    ..PageQuery::new()
                },
            )?;
            candidates.extend(branch_page.records);
        }
        if let Some(task_id) = task_id {
            let task_page = self.read_page(
                log_dir,
                PageQuery {
                    run_id: Some(run_id.to_string()),
                    task_id: Some(task_id.to_string()),
                    limit,
                    tail: true,
                    ..PageQuery::new()
                },
            )?;
            candidates.extend(task_page.records);
        }

        // Python `by_id = {record.id: record ...}`：同 id 后写覆盖先写。
        let mut by_id: HashMap<String, SwarmOperationRecord> = HashMap::new();
        for record in candidates {
            by_id.insert(record.id.clone(), record);
        }
        let mut ordered: Vec<SwarmOperationRecord> = by_id.into_values().collect();
        ordered.sort_by_key(|record| std::cmp::Reverse(record.sequence));

        let mut selected_desc: Vec<SwarmOperationRecord> = Vec::new();
        let mut used_chars: usize = 0;
        for record in ordered {
            let serialized_size = serde_json::to_string(&record)
                .map_err(|source| JournalError::Parse { source })?
                .chars()
                .count();
            if !selected_desc.is_empty()
                && (selected_desc.len() >= max_records || used_chars + serialized_size > max_chars)
            {
                continue;
            }
            selected_desc.push(record);
            used_chars += serialized_size;
        }
        selected_desc.reverse();
        let selected = selected_desc;

        let mut metadata = Map::new();
        metadata.insert(
            "policy".to_string(),
            Value::String("raw_structured_redacted_records_no_summary_substitution".to_string()),
        );
        metadata.insert("matching_total".to_string(), Value::from(run_page.total));
        metadata.insert(
            "selected_count".to_string(),
            Value::from(i64::try_from(selected.len()).unwrap_or(i64::MAX)),
        );
        metadata.insert(
            "has_more".to_string(),
            Value::from(run_page.total > i64::try_from(selected.len()).unwrap_or(i64::MAX)),
        );
        metadata.insert(
            "oldest_sequence".to_string(),
            selected
                .first()
                .map_or(Value::Null, |record| Value::from(record.sequence)),
        );
        metadata.insert(
            "latest_sequence".to_string(),
            selected
                .last()
                .map_or(Value::Null, |record| Value::from(record.sequence)),
        );
        Ok((selected, metadata))
    }

    fn lock_states(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, HashMap<PathBuf, PathState>>, JournalError> {
        self.states.lock().map_err(|_| JournalError::Poisoned)
    }
}

/// Python `parse_line`：验证一行日志（回放/测试用）。
///
/// # Errors
/// 见 [`JournalError::Parse`]。
pub fn parse_line(line: &str) -> Result<SwarmOperationRecord, JournalError> {
    serde_json::from_str(line).map_err(|source| JournalError::Parse { source })
}

/// Python `Path.expanduser().resolve()` 的对应：`~` 前缀按
/// USERPROFILE/HOME 展开，再 canonicalize。
///
/// Python `resolve()` 默认非严格——不存在的尾部保持原样，但**已存在的
/// 祖先前缀仍被解析**（符号链接/短路径名展开）。直接退回展开态会让
/// "存在的根"（canonicalize 成功，带 `\\?\` 前缀）与"不存在的 workspace"
/// （退回原样）形态不一致，边界比较随即误判越界；因此这里沿祖先上溯，
/// 解析最深存在祖先后拼回不存在尾部。
///
/// `ContextCompressor` 的日志目录边界校验（`_operation_log_window`）复用
/// 同一归一化，保证"解析侧"与"读取侧"看到同一形态的路径。
#[must_use]
pub fn normalize(path: &Path) -> PathBuf {
    let expanded = expand_home(path);
    if let Ok(canonical) = fs::canonicalize(&expanded) {
        return canonical;
    }
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    let mut ancestor = expanded.as_path();
    while let Some(parent) = ancestor.parent() {
        if let Some(name) = ancestor.file_name() {
            tail.push(name.to_os_string());
        }
        if let Ok(canonical) = fs::canonicalize(parent) {
            let mut resolved = canonical;
            for component in tail.iter().rev() {
                resolved.push(component);
            }
            return resolved;
        }
        ancestor = parent;
    }
    expanded
}

fn expand_home(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix("~/")
        && let Some(home) = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))
    {
        return Path::new(&home).join(rest);
    }
    path.to_path_buf()
}

fn line_count(path: &Path) -> Result<i64, JournalError> {
    if !path.is_file() {
        return Ok(0);
    }
    // Python `sum(1 for _ in handle)`：计所有行（含空行）——与 _read_page
    // 跳过空行的过滤不同，序列号按物理行数推进。
    let file = fs::File::open(path).map_err(|source| JournalError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let reader = BufReader::new(file);
    let mut count = 0_i64;
    for line in reader.lines() {
        line.map_err(|source| JournalError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        count += 1;
    }
    Ok(count)
}

fn read_page(path: &Path, query: &PageQuery) -> Result<SwarmOperationPage, JournalError> {
    if !path.is_file() {
        return Ok(SwarmOperationPage {
            after_sequence: query.after_sequence,
            ..SwarmOperationPage::default()
        });
    }
    let file = fs::File::open(path).map_err(|source| JournalError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let reader = BufReader::new(file);
    let mut matches: Vec<SwarmOperationRecord> = Vec::new();
    let mut total = 0_i64;
    let mut latest_sequence = 0_i64;
    for line in reader.lines() {
        let line = line.map_err(|source| JournalError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        if line.trim().is_empty() {
            continue;
        }
        let Ok(record) = parse_line(&line) else {
            continue;
        };
        latest_sequence = latest_sequence.max(record.sequence);
        if let Some(run_id) = &query.run_id
            && record.run_id.as_str() != run_id
        {
            continue;
        }
        if let Some(branch_id) = &query.branch_id
            && record.branch_id.as_ref().map(BranchId::as_str) != Some(branch_id.as_str())
        {
            continue;
        }
        if let Some(task_id) = &query.task_id
            && record.task_id.as_ref().map(TaskId::as_str) != Some(task_id.as_str())
        {
            continue;
        }
        if let Some(worker_id) = &query.worker_id
            && record.worker_id.as_deref() != Some(worker_id.as_str())
        {
            continue;
        }
        if let Some(operation_type) = &query.operation_type
            && record.operation_type != *operation_type
        {
            continue;
        }
        total += 1;
        if record.sequence <= query.after_sequence {
            continue;
        }
        matches.push(record);
    }

    let eligible_total = i64::try_from(matches.len()).unwrap_or(i64::MAX);
    let limit = usize::try_from(query.limit.max(0)).unwrap_or(usize::MAX);
    let records = if query.tail {
        let start = matches.len().saturating_sub(limit);
        matches.split_off(start)
    } else {
        matches.truncate(limit);
        matches
    };
    let next_after = records
        .last()
        .map_or(query.after_sequence, |record| record.sequence);
    Ok(SwarmOperationPage {
        records,
        after_sequence: query.after_sequence,
        next_after_sequence: next_after,
        total,
        has_more: eligible_total > query.limit,
        latest_sequence,
    })
}

fn append_line(path: &Path, record: &SwarmOperationRecord) -> Result<(), JournalError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| JournalError::Write {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let line = serde_json::to_string(record).map_err(|source| JournalError::Parse { source })?;
    let mut handle = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|source| JournalError::Write {
            path: path.to_path_buf(),
            source,
        })?;
    writeln!(handle, "{line}").map_err(|source| JournalError::Write {
        path: path.to_path_buf(),
        source,
    })?;
    handle.flush().map_err(|source| JournalError::Write {
        path: path.to_path_buf(),
        source,
    })?;
    // Python `os.fsync`：Windows 下 std 的 sync_data 映射 FlushFileBuffers。
    handle.sync_data().map_err(|source| JournalError::Write {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(())
}

/// 环境变量读取函数（注入点，生产侧包装 `std::env::var`）。
pub type EnvGetter<'a> = &'a dyn Fn(&str) -> Option<String>;

/// 解析 Mission 日志目录而不接受任意路径
/// （Python `resolve_mission_operation_log_dir`）。
///
/// Mission 元数据可经控制 API 变更，仅向 `workspace_path` 追加 `logs`
/// 不足以构成读边界；持久化的 workspace 还必须保持在配置的 Mission 根
/// 之下。
///
/// # Errors
/// 见 [`JournalError::InvalidLogDir`]（Python `ValueError` 文本逐字一致）。
pub fn resolve_mission_operation_log_dir(
    metadata: &Map<String, Value>,
    env: Option<EnvGetter<'_>>,
) -> Result<PathBuf, JournalError> {
    let workspace_raw = metadata.get("workspace_path").and_then(Value::as_str);
    let workspace_text = workspace_raw.map(str::trim).unwrap_or_default();
    if workspace_text.is_empty() {
        return Err(JournalError::InvalidLogDir {
            message: "mission workspace is unavailable; no operation log can be read".to_string(),
        });
    }

    let mission_root_raw = env.and_then(|get| get("LYNCEUS_MISSION_WORKSPACE_DIR"));
    let workspace_root_raw = env.and_then(|get| get("LYNCEUS_WORKSPACE_DIR"));
    let root = match (mission_root_raw, workspace_root_raw) {
        (Some(mission_root), _) => normalize(Path::new(&mission_root)),
        (None, Some(workspace_root)) => normalize(&Path::new(&workspace_root).join("missions")),
        (None, None) => normalize(Path::new(DEFAULT_MISSION_WORKSPACE_ROOT)),
    };
    let workspace = normalize(Path::new(workspace_text));
    if !workspace.starts_with(&root) {
        return Err(JournalError::InvalidLogDir {
            message: "mission workspace escapes the configured Mission root".to_string(),
        });
    }
    if workspace == root {
        return Err(JournalError::InvalidLogDir {
            message: "mission workspace cannot be the Mission root".to_string(),
        });
    }

    let log_dir = normalize(&workspace.join("logs"));
    if !log_dir.starts_with(&workspace) {
        return Err(JournalError::InvalidLogDir {
            message: "mission operation log path escapes its workspace".to_string(),
        });
    }
    Ok(log_dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn project() -> ProjectId {
        ProjectId::new("proj_1".to_string())
    }

    fn run() -> RunId {
        RunId::new("run_1".to_string())
    }

    fn record(entry: &str) -> SwarmOperationRecord {
        SwarmOperationRecord::new(
            project(),
            run(),
            "solver".to_string(),
            "tool.call".to_string(),
            entry.to_string(),
        )
    }

    #[test]
    fn append_assigns_sequence_and_persists_redacted_payload() {
        let dir = tempfile::tempdir().expect("临时目录必须可创建");
        let journal = SwarmOperationJournal::default();
        let mut first = record("first");
        first.payload.insert(
            "api_key".to_string(),
            Value::String("sk-secret".to_string()),
        );
        journal
            .append(dir.path(), first.clone())
            .expect("追加必须成功");
        let second = record("second");
        journal.append(dir.path(), second).expect("追加必须成功");

        let page = journal
            .read_page(dir.path(), PageQuery::new())
            .expect("读取必须成功");
        assert_eq!(page.records.len(), 2);
        assert_eq!(page.records[0].sequence, 1);
        assert_eq!(page.records[1].sequence, 2);
        assert_eq!(
            page.records[0].payload.get("api_key"),
            Some(&Value::String("********".to_string())),
            "敏感键必须整体脱敏"
        );
        assert_eq!(page.total, 2);
        assert_eq!(page.latest_sequence, 2);
        assert!(!page.has_more);
    }

    #[test]
    fn read_page_filters_and_paginates_by_cursor() {
        let dir = tempfile::tempdir().expect("临时目录必须可创建");
        let journal = SwarmOperationJournal::default();
        for index in 0..5 {
            let mut item = record(&format!("entry-{index}"));
            if index == 3 {
                item.run_id = RunId::new("run_other".to_string());
            }
            journal.append(dir.path(), item).expect("追加必须成功");
        }

        let page = journal
            .read_page(
                dir.path(),
                PageQuery {
                    run_id: Some("run_1".to_string()),
                    limit: 2,
                    ..PageQuery::new()
                },
            )
            .expect("读取必须成功");
        assert_eq!(page.records.len(), 2, "limit 生效");
        assert_eq!(page.total, 4, "run 过滤后的匹配总数");
        assert!(page.has_more);
        assert_eq!(page.next_after_sequence, 2);

        let next = journal
            .read_page(
                dir.path(),
                PageQuery {
                    run_id: Some("run_1".to_string()),
                    limit: 10,
                    after_sequence: page.next_after_sequence,
                    ..PageQuery::new()
                },
            )
            .expect("读取必须成功");
        assert_eq!(next.records.len(), 2, "游标增量拉取");
        assert_eq!(next.records[0].sequence, 3);

        let tail = journal
            .read_page(
                dir.path(),
                PageQuery {
                    run_id: Some("run_1".to_string()),
                    limit: 2,
                    tail: true,
                    ..PageQuery::new()
                },
            )
            .expect("读取必须成功");
        assert_eq!(
            tail.records.iter().map(|r| r.sequence).collect::<Vec<_>>(),
            [3, 5],
            "tail 取最近的记录（sequence 4 是 run_other，被过滤）"
        );
    }

    #[test]
    fn read_context_window_dedupes_and_respects_budget() {
        let dir = tempfile::tempdir().expect("临时目录必须可创建");
        let journal = SwarmOperationJournal::default();
        for index in 0..4 {
            journal
                .append(dir.path(), record(&format!("entry-{index}")))
                .expect("追加必须成功");
        }
        let (records, metadata) = journal
            .read_context_window(dir.path(), "run_1", None, None, 80, 96_000)
            .expect("读取必须成功");
        assert_eq!(records.len(), 4);
        assert_eq!(
            metadata.get("matching_total"),
            Some(&Value::from(4)),
            "元数据携带匹配总数"
        );

        let (few, _) = journal
            .read_context_window(dir.path(), "run_1", None, None, 2, 96_000)
            .expect("读取必须成功");
        assert_eq!(few.len(), 2, "max_records 上限生效");
        assert_eq!(
            few.iter().map(|r| r.sequence).collect::<Vec<_>>(),
            [3, 4],
            "取最近 N 条并保持时间正序"
        );
    }

    #[test]
    fn record_roundtrips_through_wire() {
        let item = record("entry");
        let json = serde_json::to_string(&item).expect("序列化不会失败");
        let back: SwarmOperationRecord =
            serde_json::from_str(&json).unwrap_or_else(|error| panic!("往返必须可解析: {error}"));
        assert_eq!(back, item);
        assert_eq!(back.role, "agent");
        assert_eq!(back.sequence, 0);
    }

    #[test]
    fn resolve_log_dir_enforces_root_boundary() {
        let root = tempfile::tempdir().expect("临时根目录必须可创建");
        let root_text = root.path().to_string_lossy().to_string();
        let env = move |key: &str| -> Option<String> {
            match key {
                "LYNCEUS_MISSION_WORKSPACE_DIR" => Some(root_text.clone()),
                _ => None,
            }
        };
        let mut metadata = Map::new();
        let inside = root.path().join("m_1");
        metadata.insert(
            "workspace_path".to_string(),
            Value::String(inside.to_string_lossy().to_string()),
        );
        let dir = resolve_mission_operation_log_dir(&metadata, Some(&env))
            .expect("边界内的 workspace 必须可解析");
        assert!(dir.ends_with("logs"));

        metadata.insert(
            "workspace_path".to_string(),
            Value::String("E:/elsewhere".to_string()),
        );
        let error = resolve_mission_operation_log_dir(&metadata, Some(&env))
            .expect_err("越界 workspace 必须被拒绝");
        assert_eq!(
            error.to_string(),
            "mission workspace escapes the configured Mission root"
        );

        metadata.insert(
            "workspace_path".to_string(),
            Value::String(root.path().to_string_lossy().to_string()),
        );
        let error = resolve_mission_operation_log_dir(&metadata, Some(&env))
            .expect_err("workspace 等于根必须被拒绝");
        assert_eq!(
            error.to_string(),
            "mission workspace cannot be the Mission root"
        );

        metadata.remove("workspace_path");
        let error = resolve_mission_operation_log_dir(&metadata, Some(&env))
            .expect_err("缺失 workspace 必须被拒绝");
        assert_eq!(
            error.to_string(),
            "mission workspace is unavailable; no operation log can be read"
        );
    }

    #[test]
    fn missing_file_yields_empty_page() {
        let dir = tempfile::tempdir().expect("临时目录必须可创建");
        let journal = SwarmOperationJournal::default();
        let page = journal
            .read_page(&dir.path().join("absent"), PageQuery::new())
            .expect("缺失文件返回空页而非错误");
        assert!(page.records.is_empty());
        assert_eq!(page.total, 0);
    }

    #[test]
    fn record_serializes_python_wire_shape() {
        // 键序与字段名守护：run_id/project_id 等snake_case wire 名 + payload 对象。
        let item = record("e");
        let json = serde_json::to_value(&item).expect("序列化不会失败");
        assert_eq!(json["run_id"], json!("run_1"));
        assert_eq!(json["project_id"], json!("proj_1"));
        assert_eq!(json["role"], json!("agent"));
        assert!(json.get("actor_label").is_some());
        assert_eq!(json["payload"], json!({}));
    }
}
