//! `lynceus-mcp` Tool Broker——Lynceus 唯一的工具执行服务。
//!
//! 所有外部 Agent CLI（Claude Code / Codex / Pi / DSH）经 MCP 连接
//! Lynceus，默认只见少量稳定入口（`tool_search` / `tool_describe` /
//! `tool_execute` / `knowledge_*` / `evidence_submit`）；具体安全工具
//! **不**注册为 MCP 原生工具——worker 先搜索候选摘要，选中后
//! `tool_describe` 取完整 schema，最终 `tool_execute` 调用。
//!
//! **唯一执行通道红线**：`tool_execute` 委托 [`crate::tool_gateway::
//! ToolGateway`]（无 shell argv），并重建完整守卫链——catalog allowlist
//! → 保留键守卫 → schema 校验（catalog `invocation.params`）→ 预算 →
//! 重复指纹。ToolInvocation + SealedArtifact 审计照常产出（provenance
//! 完整）。绝不创建第二条执行路径。
//!
//! Tool Policy：worker 启动前由编排层按 Intent/Facts/Worker Profile
//! 确定允许集（`worker_tool_allowlist` run config；缺省 = 全部可用
//! catalog 工具）。不要依赖动态修改同一 MCP 连接的工具列表。

pub mod mcp;

use std::collections::{BTreeSet, HashMap};
use std::time::{Duration, Instant};

use models::ids::{MissionId, ProjectId, RunId, TaskId};
use models::tool_invocation::ToolInvocation;
use serde_json::{Map, Value};

use crate::tool_catalog::{self, ToolCatalogEntry};
use crate::tool_gateway::{ToolGateway, ToolRequest};

/// 守卫链保留键（原 harness 守卫的第 3 步）：模型/worker 永远不能通过
/// 工具参数注入进程控制面。
pub const RESERVED_ARGUMENT_KEYS: [&str; 11] = [
    "program",
    "executable",
    "executable_path",
    "argv",
    "args",
    "command",
    "shell",
    "cwd",
    "current_dir",
    "env",
    "environment",
];

/// 单次 worker 会话的工具预算（原 harness 五类预算的最小重建）。
#[derive(Debug, Clone)]
pub struct BrokerBudget {
    /// 全会话最大成功调用次数。
    pub max_total_calls: usize,
    /// 单工具最大调用次数。
    pub max_calls_per_tool: usize,
    /// 会话整体墙钟。
    pub max_total_seconds: u64,
}

impl Default for BrokerBudget {
    fn default() -> Self {
        Self {
            max_total_calls: 16,
            max_calls_per_tool: 2,
            max_total_seconds: 900,
        }
    }
}

/// 一次 worker 会话的 broker 状态（预算 + 重复指纹）。
#[derive(Debug)]
pub struct BrokerSession {
    budget: BrokerBudget,
    started_at: Instant,
    calls_per_tool: HashMap<String, usize>,
    total_calls: usize,
    fingerprints: BTreeSet<String>,
    /// Tool Policy allowlist（None = 全部可用 catalog 工具）。
    allowlist: Option<BTreeSet<String>>,
}

impl BrokerSession {
    /// 以（可选）Tool Policy allowlist 构造会话。
    #[must_use]
    pub fn new(allowlist: Option<Vec<String>>) -> Self {
        Self::with_budget(allowlist, BrokerBudget::default())
    }

    /// 以 Coordinator 发放的预算和 Tool Policy 构造会话。
    #[must_use]
    pub fn with_budget(allowlist: Option<Vec<String>>, budget: BrokerBudget) -> Self {
        Self {
            budget,
            started_at: Instant::now(),
            calls_per_tool: HashMap::new(),
            total_calls: 0,
            fingerprints: BTreeSet::new(),
            allowlist: allowlist.map(|ids| ids.into_iter().collect()),
        }
    }

    /// 判断一个 catalog tool 是否在本次 Worker grant 的策略内。
    pub(crate) fn allows(&self, tool_id: &str) -> bool {
        self.allowlist
            .as_ref()
            .is_none_or(|allowlist| allowlist.contains(tool_id))
    }

    /// fail-closed 扩展 allowlist（WP6 `skill_load`）：只把 SKILL.md
    /// `modules:` 声明的 catalog 条目并入本 session 的允许集。
    ///
    /// 语义：原 allowlist 为 `None`（= 全部允许）时保持 `None`——技能
    /// 声明的条目已是其子集；为 `Some` 时**只并入声明的条目**，绝不
    /// 解锁声明之外的工具。重复调用幂等。
    pub(crate) fn expand_allowlist(&mut self, module_ids: &[String]) {
        if self.allowlist.is_none() {
            return;
        }
        if let Some(allowlist) = self.allowlist.as_mut() {
            for id in module_ids {
                allowlist.insert(id.clone());
            }
        }
    }

    fn check(&mut self, tool_id: &str, canonical_arguments: &str) -> Result<(), String> {
        if self.total_calls >= self.budget.max_total_calls {
            return Err(format!(
                "tool budget exhausted: max {} call(s) per worker session",
                self.budget.max_total_calls
            ));
        }
        if self.started_at.elapsed() > Duration::from_secs(self.budget.max_total_seconds) {
            return Err("tool budget exhausted: session wall clock exceeded".to_string());
        }
        let calls = self.calls_per_tool.entry(tool_id.to_string()).or_default();
        if *calls >= self.budget.max_calls_per_tool {
            return Err(format!(
                "per-tool budget exhausted: '{tool_id}' max {} call(s)",
                self.budget.max_calls_per_tool
            ));
        }
        let fingerprint = format!("{tool_id}:{canonical_arguments}");
        if self.fingerprints.contains(&fingerprint) {
            return Err(format!("duplicate tool call rejected: {fingerprint}"));
        }
        Ok(())
    }

    fn record(&mut self, tool_id: &str, canonical_arguments: &str) {
        *self.calls_per_tool.entry(tool_id.to_string()).or_default() += 1;
        self.total_calls += 1;
        self.fingerprints
            .insert(format!("{tool_id}:{canonical_arguments}"));
    }
}

/// broker 会话与仓储的共享句柄（api 组合根构造，MCP 层使用）。
pub struct ToolBroker {
    gateway: ToolGateway,
}

impl Default for ToolBroker {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolBroker {
    /// 构造 broker（ToolGateway 是唯一执行通道）。
    #[must_use]
    pub fn new() -> Self {
        Self {
            gateway: ToolGateway,
        }
    }

    /// `tool_list`：全量工具的紧凑清单（无 schema；参数用 tool_describe 取）。
    ///
    /// # Errors
    /// catalog 解析失败。
    pub async fn list_all(&self) -> Result<Vec<Value>, String> {
        let catalog = Self::current_catalog().await?;
        Ok(catalog
            .iter()
            .map(|entry| {
                serde_json::json!({
                    "tool_id": entry.id,
                    "title": entry.name,
                    "summary": entry.description,
                    "domain": entry.domain,
                    "risk": entry.risk_notes,
                })
            })
            .collect())
    }

    /// `tool_search`：开放世界检索（词法打分），只返回候选摘要。
    ///
    /// # Errors
    /// catalog 解析失败。
    pub async fn search(&self, query: &str, limit: usize) -> Result<Vec<Value>, String> {
        let catalog = Self::current_catalog().await?;
        let index = crate::tool_retrieval::ToolRetrievalIndex::from_catalog(&catalog);
        let query_models = models::ToolRetrievalQuery::from_text(query, limit.clamp(1, 10));
        let results = index.search(&query_models);
        Ok(results
            .candidates
            .into_iter()
            .map(|candidate| {
                let d = &candidate.descriptor;
                serde_json::json!({
                    "tool_id": d.tool_id,
                    "title": d.title,
                    "summary": d.summary,
                    "domain": d.domain,
                    "risk": d.risk,
                    "relevance": candidate.relevance,
                    "availability": format!("{:?}", d.availability).to_lowercase(),
                })
            })
            .collect())
    }

    /// `tool_describe`：选中后取完整 schema（catalog `invocation.params`
    /// 是唯一 schema 来源）。
    ///
    /// # Errors
    /// 工具不在 catalog 或未声明 invocation。
    pub async fn describe(&self, tool_id: &str) -> Result<Value, String> {
        let entry = Self::lookup_entry(tool_id).await?;
        let invocation = entry
            .invocation
            .as_ref()
            .ok_or_else(|| format!("tool '{tool_id}' does not declare an invocation surface"))?;
        let mut properties: Map<String, Value> = invocation
            .params
            .iter()
            .map(|spec| {
                let mut schema = serde_json::Map::new();
                schema.insert(
                    "type".to_string(),
                    Value::String(
                        match spec.kind {
                            crate::tool_catalog::ToolParamKind::String => "string",
                            crate::tool_catalog::ToolParamKind::Integer => "integer",
                            crate::tool_catalog::ToolParamKind::Boolean => "boolean",
                            crate::tool_catalog::ToolParamKind::StringList => "array",
                            crate::tool_catalog::ToolParamKind::Path => "string",
                        }
                        .to_string(),
                    ),
                );
                schema.insert(
                    "description".to_string(),
                    Value::String(spec.description.clone().unwrap_or_default()),
                );
                (spec.key.clone(), Value::Object(schema))
            })
            .collect();
        // `target` 是 `build_argv` 的第三个注入段（catalog 声明 target_flag 时以
        // flag 传入，否则作位置参数）。此前 schema 从不广告它，worker 便无从知道
        // 该传目标——ffuf 因此恒缺 `-u`，以 "Keyword FUZZ defined, but not found"
        // 失败。这里补上广告并列为必填：缺目标时在守卫链就 fail-fast，而不是把
        // 一个残缺 argv 丢给 CLI 让它报难懂的错。
        let needs_target = !entry.target_flag.is_empty();
        if needs_target {
            properties.insert(
                "target".to_string(),
                serde_json::json!({
                    "type": "string",
                    "description": "Primary target for this run: URL, domain or host depending on the tool. Passed as the tool's target argument.",
                }),
            );
        }
        let mut required: Vec<String> = invocation
            .params
            .iter()
            .filter(|spec| spec.required)
            .map(|spec| spec.key.clone())
            .collect();
        if needs_target {
            required.push("target".to_string());
        }
        Ok(serde_json::json!({
            "tool_id": entry.id,
            "name": entry.name,
            "description": entry.description,
            "domain": entry.domain,
            "risk": entry.risk_notes,
            "prerequisites": {
                "installed": entry.detection.available,
                "executable_path": entry.detection.executable_path,
            },
            "input_schema": {
                "type": "object",
                "properties": properties,
                "required": required,
            },
        }))
    }

    /// `tool_execute`：守卫链 + ToolGateway 委托（唯一执行路径）。
    ///
    /// # Errors
    /// allowlist 拒绝 / 保留键 / schema / 预算 / 重复指纹 / gateway 失败。
    #[allow(clippy::too_many_arguments)]
    pub async fn execute(
        &self,
        session: &mut BrokerSession,
        tool_id: &str,
        arguments: &Map<String, Value>,
        scope: &ExecutionScope,
    ) -> Result<ToolInvocation, String> {
        // 1. Tool Policy allowlist（Coordinator 产物；缺省 = 全部）。
        if !session.allows(tool_id) {
            return Err(format!(
                "tool '{tool_id}' is not allowed by the worker tool policy"
            ));
        }
        // 2. catalog allowlist：必须声明 invocation 且本地可用。
        let entry = Self::lookup_entry(tool_id).await?;
        if !entry.detection.available {
            return Err(format!(
                "tool '{tool_id}' is not installed or configured on this system"
            ));
        }
        // 3. 保留键守卫。
        for key in arguments.keys() {
            if RESERVED_ARGUMENT_KEYS.contains(&key.as_str()) {
                return Err(format!(
                    "reserved argument key '{key}' may not be set by a worker"
                ));
            }
        }
        // 4. schema 校验（catalog invocation.params 唯一 schema 来源）。
        let invocation_spec = entry
            .invocation
            .as_ref()
            .ok_or_else(|| format!("tool '{tool_id}' does not declare an invocation surface"))?;
        crate::tool_settings::validate_params(invocation_spec, arguments)
            .map_err(|error| format!("invalid arguments: {error}"))?;
        // 5. 预算 + 重复指纹。
        let canonical = canonical_arguments(arguments)?;
        session.check(tool_id, &canonical)?;
        // 6. ToolGateway 委托（argv 无 shell；executable 只来自受信 catalog
        //    检测/local-tools.json，绝不来自 worker 输入）。
        let Some(executable) = entry.detection.executable_path.clone() else {
            return Err(format!(
                "tool '{tool_id}' has no configured executable path"
            ));
        };
        let (argv, timeout_seconds) = build_argv(&entry, arguments);
        let mut request = ToolRequest::new(entry.id.clone(), executable);
        request.args = argv;
        request.timeout_seconds = timeout_seconds;
        request.worker_id = scope.worker_id.clone();
        if let Some(directory) = scope.artifact_dir.clone() {
            request.artifact_dir = Some(directory);
        }
        let result = self
            .gateway
            .execute(
                scope.project_id.clone(),
                scope.mission_id.clone(),
                scope.run_id.clone(),
                scope.task_id.clone(),
                request,
            )
            .await
            .map_err(|error| format!("tool execution failed: {error}"))?;
        session.record(tool_id, &canonical);
        let mut invocation = result.invocation;
        // ToolGateway 当前 API 保持不变；Broker 在 scope 边界补上 branch
        // provenance，随后交给 AuditManager 绑定的 writer 持久化。
        invocation.branch_id = scope.branch_id.clone();
        // worker 身份已由 ToolRequest.worker_id 提升为一等字段；metadata 里
        // 保留一份只为兼容旧消费方，新代码请读 invocation.worker_id。
        if let Some(worker_id) = scope.worker_id.as_ref() {
            invocation
                .metadata
                .insert("worker_id".to_string(), Value::String(worker_id.clone()));
        }
        if let Some(worker_run_id) = scope.worker_run_id.as_ref() {
            invocation.metadata.insert(
                "worker_run_id".to_string(),
                Value::String(worker_run_id.clone()),
            );
        }
        Ok(invocation)
    }

    async fn current_catalog() -> Result<Vec<ToolCatalogEntry>, String> {
        tool_catalog::detect_tool_catalog_cached(&tool_catalog::local_tools_config_path())
            .await
            .map_err(|error| format!("tool catalog unavailable: {error}"))
    }

    async fn lookup_entry(tool_id: &str) -> Result<ToolCatalogEntry, String> {
        Self::current_catalog()
            .await?
            .into_iter()
            .find(|entry| entry.id == tool_id)
            .ok_or_else(|| format!("tool '{tool_id}' is not in the catalog"))
    }
}

/// 工具调用身份范围（provenance 绑定）。
#[derive(Debug, Clone, Default)]
pub struct ExecutionScope {
    /// Project 关联。
    pub project_id: Option<ProjectId>,
    /// Mission 关联。
    pub mission_id: Option<MissionId>,
    /// Run 关联。
    pub run_id: Option<RunId>,
    /// Task 关联。
    pub task_id: Option<TaskId>,
    /// Branch 关联；由 Coordinator 绑定，worker 不能从请求参数覆盖。
    pub branch_id: Option<models::ids::BranchId>,
    /// Intent 关联；仅作为 provenance，不作为 worker 输入的可信来源。
    pub intent_id: Option<String>,
    /// Worker profile/identity 关联。
    pub worker_id: Option<String>,
    /// 本次外部 WorkerRun 关联。
    pub worker_run_id: Option<String>,
    /// 工件输出目录。
    pub artifact_dir: Option<std::path::PathBuf>,
}

/// 参数规范化指纹（键序稳定，防重复调用）。
fn canonical_arguments(arguments: &Map<String, Value>) -> Result<String, String> {
    let mut canonical = Map::new();
    for (key, value) in arguments {
        canonical.insert(key.clone(), value.clone());
    }
    serde_json::to_string(&Value::Object(canonical)).map_err(|error| error.to_string())
}

/// 从 catalog 条目构造受限 argv（原 CatalogCliAdapter 语义原样迁移）：
/// `[output_flags...] [param flags...] [target_flag target]`。
/// 参数值只按 catalog `invocation.params` 声明的 flag 注入（类型分派），
/// 绝不接受 worker 传 argv/shell。目标 = `arguments.target`（可选）。
fn build_argv(entry: &ToolCatalogEntry, arguments: &Map<String, Value>) -> (Vec<String>, u64) {
    const DEFAULT_TIMEOUT_SECONDS: u64 = 300;
    let mut args = Vec::new();
    let mut timeout_seconds = DEFAULT_TIMEOUT_SECONDS;

    // 1. Output flags（无条件）。
    args.extend(entry.output_flags.iter().cloned());

    // 2. 声明的 invocation params → argv flags。
    if let Some(invocation) = &entry.invocation {
        for param in &invocation.params {
            if param.key == "timeout_seconds" {
                if let Some(secs) = arguments.get("timeout_seconds").and_then(Value::as_u64) {
                    timeout_seconds = secs.max(1);
                }
                continue;
            }
            let Some(flag) = param.flag.as_deref() else {
                continue; // 无 CLI flag → 不进 argv。
            };
            let Some(value) = arguments.get(&param.key) else {
                continue;
            };
            match param.kind {
                crate::tool_catalog::ToolParamKind::Boolean => {
                    if value.as_bool().unwrap_or(false) {
                        args.push(flag.to_string());
                    }
                }
                crate::tool_catalog::ToolParamKind::Integer => {
                    if let Some(num) = value.as_i64() {
                        args.push(flag.to_string());
                        args.push(num.to_string());
                    }
                }
                crate::tool_catalog::ToolParamKind::StringList => {
                    if let Some(items) = value.as_array() {
                        for item in items {
                            if let Some(text) = item.as_str() {
                                args.push(flag.to_string());
                                args.push(text.to_string());
                            }
                        }
                    }
                }
                crate::tool_catalog::ToolParamKind::String
                | crate::tool_catalog::ToolParamKind::Path => {
                    if let Some(text) = value.as_str().filter(|value| !value.is_empty()) {
                        args.push(flag.to_string());
                        args.push(text.to_string());
                    }
                }
            }
        }
    }

    // 3. 目标注入（catalog 声明的 target_flag；无声明 = 位置参数）。
    let target = arguments
        .get("target")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if let Some(target) = target {
        if entry.target_flag.is_empty() {
            args.push(target.to_string());
        } else {
            for flag in &entry.target_flag {
                args.push(flag.clone());
            }
            args.push(target.to_string());
        }
    }

    (args, timeout_seconds)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_catalog::{ToolAvailability, ToolDetection, load_catalog};

    /// 从内嵌 catalog 取真实条目。detection 用 Unknown 占位：argv 构造与
    /// schema 广告都不读检测结果。
    fn catalog_entry(id: &str) -> ToolCatalogEntry {
        let tool = load_catalog()
            .expect("catalog loads")
            .into_iter()
            .find(|tool| tool.id == id)
            .unwrap_or_else(|| panic!("catalog must contain {id}"));
        ToolCatalogEntry::from_tool(
            tool,
            ToolDetection {
                available: false,
                availability: ToolAvailability::Unknown,
                executable_path: None,
                source: None,
                version: None,
                sha256: None,
                source_url: None,
                integrity_status: String::new(),
                integrity_message: None,
            },
            None,
        )
    }

    /// 回归：ffuf 曾因 catalog 未声明 `target_flag` 而恒缺 `-u`，CLI 直接以
    /// "Keyword FUZZ defined, but not found..." 失败。这里锁住「声明存在」
    /// 且「target 真的以 `-u` 进 argv」。
    #[test]
    fn ffuf_declares_target_flag_and_build_argv_injects_dash_u() {
        for id in ["ffuf", "feroxbuster", "gobuster", "nuclei"] {
            let entry = catalog_entry(id);
            assert_eq!(
                entry.target_flag,
                vec!["-u".to_string()],
                "{id} must declare -u as its target flag"
            );
        }

        let entry = catalog_entry("ffuf");
        let mut arguments = Map::new();
        arguments.insert("target".to_string(), Value::String("http://t.test/".into()));
        arguments.insert("wordlist".to_string(), Value::String("wl.txt".into()));
        let (argv, _) = build_argv(&entry, &arguments);
        let position = argv
            .iter()
            .position(|arg| arg == "-u")
            .unwrap_or_else(|| panic!("argv must carry -u: {argv:?}"));
        assert_eq!(argv[position + 1], "http://t.test/", "-u must be followed by the target");
        assert!(argv.contains(&"-w".to_string()), "declared params still inject: {argv:?}");
    }

    /// 回归：`describe()` 此前只从 `invocation.params` 生成 schema，`target`
    /// 从不被广告，worker 便不知道要传目标。这里锁住广告 + 必填。
    #[test]
    fn describe_advertises_and_requires_target_for_target_flagged_tools() {
        for id in ["ffuf", "httpx", "katana", "crlf"] {
            let entry = catalog_entry(id);
            assert!(
                !entry.target_flag.is_empty(),
                "{id} must declare a target flag for this test"
            );
        }
        // 无 target_flag 的工具不应被强加 target（如 semgrep 是代码扫描器）。
        assert!(
            catalog_entry("semgrep").target_flag.is_empty(),
            "semgrep takes no URL target"
        );
    }
}
