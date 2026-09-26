//! `lynceus-mcp` Streamable HTTP MCP server（挂在 api 进程 `/mcp`）。
//!
//! MCP 只负责协议、认证、session 和取消边界；具体工具仍由
//! [`crate::broker::ToolBroker`] 统一路由到现有 Catalog/Gateway。

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::State;
use axum::http::header::{AUTHORIZATION, HOST, ORIGIN};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use models::retrieval::{ArtifactKind, ArtifactRecord};
use models::tool_invocation::ToolInvocation;
use models::{BlackboardEntry, BlackboardEntryKind, Evidence, Finding};
use models::lifecycle::{EvidenceKind, FindingStatus, ToolStatus};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use storage::Repository;
use tokio::sync::Mutex as AsyncMutex;

use crate::broker::{BrokerBudget, BrokerSession, ExecutionScope, ToolBroker};
use crate::skills::SkillManager;
use evidence::FindingVerificationService;

const MCP_PROTOCOL_VERSION: &str = "2025-06-18";
const DEFAULT_GRANT_TTL: Duration = Duration::from_secs(15 * 60);

/// MCP 执行结果的持久化回调；生产组合根将它绑定到 AuditManager。
pub type AuditWriter =
    Arc<dyn Fn(ToolInvocation, Vec<ArtifactRecord>) -> Result<(), String> + Send + Sync>;

/// Coordinator 发放给单个外部 Worker 的一次性连接授权。
///
/// bearer 原文只在返回值中出现，不进入 session 表、数据库、日志或 MCP
/// 配置；服务端只保留 hash。scope 也只从服务端保存的 grant 复制到 session，
/// 不信任 Worker 传入的 JSON-RPC 参数。
#[derive(Debug, Clone)]
pub struct WorkerGrantSpec {
    /// 本次 Worker 的不可变 provenance scope。
    pub scope: ExecutionScope,
    /// 允许执行的 catalog tool；`None` 表示使用当前 catalog 的全部工具。
    pub allowlist: Option<Vec<String>>,
    /// 本次 Worker 的 broker 预算。
    pub budget: BrokerBudget,
    /// grant 有效期；过短或为零时使用安全默认值。
    pub ttl: Duration,
}

impl WorkerGrantSpec {
    /// 使用默认短期有效期构造 grant spec。
    #[must_use]
    pub fn new(scope: ExecutionScope, allowlist: Option<Vec<String>>) -> Self {
        Self {
            scope,
            allowlist,
            budget: BrokerBudget::default(),
            ttl: DEFAULT_GRANT_TTL,
        }
    }
}

/// Worker 连接 MCP 时拿到的短期凭据。
#[derive(Clone, PartialEq, Eq)]
pub struct WorkerGrantCredentials {
    /// 用于后续审计/撤销的 grant ID；不是 bearer secret。
    pub grant_id: String,
    /// 只应放入当前子进程的 Authorization header。
    pub bearer_token: String,
}

impl std::fmt::Debug for WorkerGrantCredentials {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WorkerGrantCredentials")
            .field("grant_id", &self.grant_id)
            .field("bearer_token", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone)]
struct WorkerGrant {
    token_digest: [u8; 32],
    scope: ExecutionScope,
    allowlist: Option<Vec<String>>,
    budget: BrokerBudget,
    expires_at: Instant,
    revoked: bool,
}

/// MCP 会话状态：BrokerSession（预算 + 指纹 + allowlist）+ provenance scope。
pub struct McpSession {
    /// 该 session 由哪个短期 grant 创建。
    grant_id: String,
    /// broker 会话（预算/指纹/allowlist）。
    pub broker: BrokerSession,
    /// provenance scope（只来自 WorkerGrant）。
    pub scope: ExecutionScope,
}

/// MCP server 共享状态。
pub struct McpServerState {
    /// 唯一 Tool Broker（ToolGateway 委托）。
    pub broker: ToolBroker,
    /// 仓储句柄（knowledge_* 和 candidate reference 校验）。
    pub repository: Arc<dyn Repository>,
    /// Skill 目录管理器（WP6：`skill_list` / `skill_load` 数据源）。
    /// RwLock 支持测试注入根目录（`set_root`）。
    pub skills: std::sync::RwLock<SkillManager>,
    /// 本地 MCP endpoint；不从 Worker 请求参数接受任意远程地址。
    endpoint: String,
    /// 由组合根绑定的唯一审计写入口。
    audit_writer: std::sync::RwLock<Option<AuditWriter>>,
    /// grant 表；只保存 token hash，不保存 bearer 原文。
    grants: Mutex<HashMap<String, WorkerGrant>>,
    /// session 表保持 session 本身不被并发请求移除；每个 session 内部串行。
    sessions: Mutex<HashMap<String, Arc<AsyncMutex<McpSession>>>>,
}

impl McpServerState {
    /// 构造共享状态。
    #[must_use]
    pub fn new(repository: Arc<dyn Repository>) -> Arc<Self> {
        let endpoint = std::env::var("LYNCEUS_MCP_URL")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| {
                let bind =
                    std::env::var("LYNCEUS_BIND").unwrap_or_else(|_| "127.0.0.1:8000".to_string());
                format!("http://{bind}/mcp")
            });
        Arc::new(Self {
            broker: ToolBroker::new(),
            repository,
            skills: std::sync::RwLock::new(SkillManager::from_env()),
            endpoint,
            audit_writer: std::sync::RwLock::new(None),
            grants: Mutex::new(HashMap::new()),
            sessions: Mutex::new(HashMap::new()),
        })
    }

    /// Coordinator 注入 WorkerExecutionRequest 的固定 MCP endpoint。
    #[must_use]
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// 将 MCP 审计写入绑定到 AuditManager；未绑定时执行会 fail closed。
    pub fn set_audit_writer(&self, writer: Option<AuditWriter>) {
        let mut guard = self
            .audit_writer
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *guard = writer;
    }

    fn audit_writer(&self) -> Option<AuditWriter> {
        self.audit_writer
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// 发放一个短期、scope-bound Worker grant。
    ///
    /// # Errors
    /// mission/run/task/worker scope 不完整，或 artifact root 未绑定。
    pub fn issue_worker_grant(
        &self,
        mut spec: WorkerGrantSpec,
    ) -> Result<WorkerGrantCredentials, String> {
        validate_worker_scope(&spec.scope)?;
        if spec.ttl.is_zero() {
            spec.ttl = DEFAULT_GRANT_TTL;
        }
        let grant_id = uuid::Uuid::new_v4().simple().to_string();
        let bearer_token = format!(
            "lyn_{}_{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let token_digest = digest_token(&bearer_token);
        let grant = WorkerGrant {
            token_digest,
            scope: spec.scope,
            allowlist: spec.allowlist,
            budget: spec.budget,
            expires_at: Instant::now() + spec.ttl,
            revoked: false,
        };
        self.grants
            .lock()
            .map_err(|_| "MCP grant table is poisoned".to_string())?
            .insert(grant_id.clone(), grant);
        Ok(WorkerGrantCredentials {
            grant_id,
            bearer_token,
        })
    }

    /// 撤销 grant；已建立的 session 也会在下一次请求时失效。
    pub fn revoke_worker_grant(&self, grant_id: &str) -> bool {
        let Ok(mut grants) = self.grants.lock() else {
            return false;
        };
        let Some(grant) = grants.get_mut(grant_id) else {
            return false;
        };
        grant.revoked = true;
        true
    }

    fn grant_for_token(&self, token: &str) -> Result<(String, WorkerGrant), String> {
        let digest = digest_token(token);
        let grants = self
            .grants
            .lock()
            .map_err(|_| "MCP grant table is poisoned".to_string())?;
        grants
            .iter()
            .find(|(_, grant)| {
                !grant.revoked && grant.expires_at > Instant::now() && grant.token_digest == digest
            })
            .map(|(id, grant)| (id.clone(), grant.clone()))
            .ok_or_else(|| "invalid, expired, or revoked Worker grant".to_string())
    }

    async fn session_for_token(
        &self,
        session_id: &str,
        token: &str,
    ) -> Result<Arc<AsyncMutex<McpSession>>, String> {
        let session = self
            .sessions
            .lock()
            .map_err(|_| "MCP session table is poisoned".to_string())?
            .get(session_id)
            .cloned()
            .ok_or_else(|| "unknown or expired MCP session".to_string())?;
        let session_grant_id = session.lock().await.grant_id.clone();
        let (grant_id, _) = self.grant_for_token(token)?;
        if grant_id != session_grant_id {
            return Err("MCP bearer is not authorized for this session".to_string());
        }
        Ok(session)
    }

    fn insert_session(&self, grant_id: String, mut grant: WorkerGrant) -> Result<String, String> {
        // WP5：按 Agent 预设的 `tools` 白名单收紧会话允许集（fail-closed）。
        // worker 连接时 dispatch 已把 agent_preset_id 写进 worker_runs，
        // 此处以它为准解析授权（mission-config 与 profile 绑定都覆盖）。
        let preset = grant
            .scope
            .worker_run_id
            .as_deref()
            .and_then(|worker_run_id| self.repository.get_worker_run(worker_run_id).ok().flatten())
            .and_then(|run| run.agent_preset_id)
            .and_then(|preset_id| self.repository.get_agent_preset(&preset_id).ok().flatten())
            .filter(|preset| preset.enabled && !preset.tools.is_empty());
        if let Some(preset) = preset {
            grant.allowlist = match grant.allowlist {
                // grant 已收紧（run config worker_tool_allowlist）→ 取交集。
                Some(allowlist) => Some(
                    allowlist
                        .into_iter()
                        .filter(|tool_id| preset.tools.contains(tool_id))
                        .collect(),
                ),
                // grant 未限制 → 预设白名单即授权面。
                None => Some(preset.tools.iter().cloned().collect()),
            };
        }
        let session_id = format!("mcp_{}", uuid::Uuid::new_v4().simple());
        let session = McpSession {
            grant_id,
            broker: BrokerSession::with_budget(grant.allowlist, grant.budget),
            scope: grant.scope,
        };
        self.sessions
            .lock()
            .map_err(|_| "MCP session table is poisoned".to_string())?
            .insert(session_id.clone(), Arc::new(AsyncMutex::new(session)));
        Ok(session_id)
    }
}

fn digest_token(token: &str) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(token.as_bytes());
    digest.finalize().into()
}

fn validate_worker_scope(scope: &ExecutionScope) -> Result<(), String> {
    let required = [
        (
            "mission_id",
            scope.mission_id.as_ref().map(|value| value.as_str()),
        ),
        ("run_id", scope.run_id.as_ref().map(|value| value.as_str())),
        (
            "task_id",
            scope.task_id.as_ref().map(|value| value.as_str()),
        ),
        ("worker_id", scope.worker_id.as_deref()),
        ("worker_run_id", scope.worker_run_id.as_deref()),
    ];
    for (name, value) in required {
        if value.map_or(true, str::is_empty) {
            return Err(format!("Worker grant requires non-empty {name}"));
        }
    }
    if scope.artifact_dir.is_none() {
        return Err("Worker grant requires an artifact directory".to_string());
    }
    Ok(())
}

/// MCP 工具清单（稳定入口；具体安全工具绝不在此列出）。
const TOOL_NAMES: [&str; 14] = [
    "tool_list",
    "tool_search",
    "tool_describe",
    "tool_execute",
    "knowledge_search",
    "knowledge_get",
    "evidence_propose",
    "blackboard_read",
    "blackboard_append",
    "blackboard_claim",
    "skill_list",
    "skill_load",
    "traffic_search",
    "traffic_get",
];

fn tool_definitions() -> Value {
    // 白板 kind 枚举从模型单一来源取：schema 广告与 parse 校验永不漂移。
    let blackboard_kinds: Vec<&str> =
        BlackboardEntryKind::ALL.iter().map(|kind| kind.as_str()).collect();
    json!({
        "tool_list": {
            "description": "List every tool in the Lynceus catalog as a compact summary (id/title/domain/availability, no schemas). Pick candidates with tool_describe before calling tool_execute.",
            "inputSchema": {"type": "object", "properties": {}}
        },
        "tool_search": {
            "description": "Search the Lynceus tool catalog by free-text query. Returns compact candidate summaries only; describe a candidate with tool_describe before calling tool_execute.",
            "inputSchema": {"type": "object", "properties": {
                "query": {"type": "string"},
                "limit": {"type": "integer", "minimum": 1, "maximum": 10}
            }, "required": ["query"]}
        },
        "skill_list": {
            "description": "List available Lynceus skills (SKILL.md procedures) with their descriptions. Load one with skill_load to get the full procedure manual; loading also unlocks only the catalog tools the skill declares in frontmatter `modules:`.",
            "inputSchema": {"type": "object", "properties": {}}
        },
        "skill_load": {
            "description": "Load a skill by name: returns the SKILL.md procedure manual and unlocks ONLY the catalog tools listed in its frontmatter `modules:` (fail-closed). Every call is recorded to the usage ledger, including misses.",
            "inputSchema": {"type": "object", "properties": {
                "name": {"type": "string"}
            }, "required": ["name"]}
        },
        "tool_describe": {
            "description": "Get the full input schema, prerequisites and risk notes for one catalog tool selected via tool_search.",
            "inputSchema": {"type": "object", "properties": {
                "tool_id": {"type": "string"}
            }, "required": ["tool_id"]}
        },
        "tool_execute": {
            "description": "Execute a catalog tool through the Lynceus guard chain (allowlist, reserved-key guard, schema validation, budget, duplicate-fingerprint) and the no-shell ToolGateway. Arguments must match the schema returned by tool_describe.",
            "inputSchema": {"type": "object", "properties": {
                "tool_id": {"type": "string"},
                "arguments": {"type": "object"}
            }, "required": ["tool_id", "arguments"]}
        },
        "knowledge_search": {
            "description": "Search the Lynceus tactical knowledge base (FTS/BM25 with fallback). Knowledge is a tactical wiki, never audit evidence.",
            "inputSchema": {"type": "object", "properties": {
                "query": {"type": "string"},
                "limit": {"type": "integer", "minimum": 1, "maximum": 20}
            }, "required": ["query"]}
        },
        "knowledge_get": {
            "description": "Fetch one knowledge card by id.",
            "inputSchema": {"type": "object", "properties": {
                "card_id": {"type": "string"}
            }, "required": ["card_id"]}
        },
        "evidence_propose": {
            "description": "Promote a confirmed observation into a Finding, bound to real execution output (provenance is verified against on-disk bytes, never trusted). Two evidence sources: (a) a recorded traffic exchange — pass `traffic_id` (from traffic_search/traffic_get) when the evidence came from an HTTP request you made (e.g. curl); its response body is sealed and hash-verified. (b) a tool_execute run — pass `invocation_id` + `artifact_id` + `locator` from that call's result. Pass `flag` when you recovered a CTF flag that appears verbatim in that output — it then runs the flag_capture gate (byte-exact match). Returns `confirmed` with a finding_id, or `rejected` with reasons. Only call after the evidence truly appeared in real output.",
            "inputSchema": {"type": "object", "properties": {
                "traffic_id": {"type": "string"},
                "invocation_id": {"type": "string"},
                "artifact_id": {"type": "string"},
                "locator": {"type": "string"},
                "summary": {"type": "string", "maxLength": 2000},
                "flag": {"type": "string"},
                "title": {"type": "string", "maxLength": 200}
            }, "required": ["summary"]}
        },
        "blackboard_read": {
            "description": "Read the authenticated Worker's Mission-scoped append-only whiteboard after an optional sequence cursor.",
            "inputSchema": {"type": "object", "properties": {
                "cursor": {"type": ["integer", "string"], "minimum": 0},
                "kind": {"type": "string"},
                "limit": {"type": "integer", "minimum": 1, "maximum": 100},
                "context_byte_budget": {"type": "integer", "minimum": 1024, "maximum": 65536}
            }}
        },
        "blackboard_append": {
            "description": "Append one bounded Mission whiteboard entry. `kind` MUST be one of: task, hypothesis, observation, artifact_ref, evidence_ref, question, blocker, decision, summary (use `observation` for a checked result, `summary` for a bounded wrap-up). References must be typed ArtifactRecord or Evidence ids; raw evidence is not accepted.",
            "inputSchema": {"type": "object", "properties": {
                "kind": {"type": "string", "enum": blackboard_kinds},
                "content": {"type": "string", "maxLength": 16384},
                "artifact_id": {"type": "string"},
                "evidence_id": {"type": "string"},
                "locator": {"type": "string"},
                "idempotency_key": {"type": "string", "maxLength": 256}
            }, "required": ["kind", "idempotency_key"]}
        },
        "blackboard_claim": {
            "description": "Atomically claim the session task with a short-lived Worker lease.",
            "inputSchema": {"type": "object", "properties": {
                "task_id": {"type": "string"},
                "lease_seconds": {"type": "integer", "minimum": 1, "maximum": 3600}
            }, "required": ["task_id"]}
        },
        "traffic_search": {
            "description": "Search recorded HTTP traffic for one host (required). Returns lite index rows only (id/method/url/status, no bodies); use traffic_get for the full request/response. body_contains does substring search in request and response bodies (at least 3 characters).",
            "inputSchema": {"type": "object", "properties": {
                "host": {"type": "string"},
                "body_contains": {"type": "string", "minLength": 3},
                "limit": {"type": "integer", "minimum": 1, "maximum": 10}
            }, "required": ["host"]}
        },
        "traffic_get": {
            "description": "Fetch the full raw request and response (headers + body) of one recorded traffic exchange by id from traffic_search.",
            "inputSchema": {"type": "object", "properties": {
                "id": {"type": "string"}
            }, "required": ["id"]}
        }
    })
}

/// 挂载到 api 路由的 `/mcp` 处理器（POST JSON-RPC）。
pub async fn mcp_handler(
    State(state): State<Arc<McpServerState>>,
    headers: HeaderMap,
    Json(request): Json<Value>,
) -> Response {
    let request_id = request["id"].clone();
    if let Err(error) = validate_loopback_headers(&headers) {
        return jsonrpc_error(request_id, -32001, &error);
    }
    let method = request["method"].as_str().unwrap_or_default().to_string();
    let session_id = headers
        .get("mcp-session-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);

    // MCP initialized 是必须认证的通知；其他无 id 通知不产生响应。
    if method == "initialized" {
        let Some(session_id) = session_id else {
            return jsonrpc_error(request_id, -32002, "missing Mcp-Session-Id header");
        };
        let Some(token) = bearer_token(&headers) else {
            return jsonrpc_error(request_id, -32001, "missing Worker grant bearer token");
        };
        if let Err(error) = state.session_for_token(&session_id, token).await {
            return jsonrpc_error(request_id, -32001, &error);
        }
        return StatusCode::ACCEPTED.into_response();
    }
    if request["id"].is_null() {
        return StatusCode::ACCEPTED.into_response();
    }

    let (result, session_header) = match method.as_str() {
        "initialize" => match initialize(&state, &headers, &request) {
            Ok((result, session_id)) => (Ok(result), Some(session_id)),
            Err(error) => (Err(error), None),
        },
        "tools/list" => match authenticated_session(&state, &headers).await {
            Ok(session) => (Ok(tool_list(&session)), None),
            Err(error) => (Err(error), None),
        },
        "tools/call" => {
            let Some(session_id) = session_id else {
                return jsonrpc_error(
                    request_id,
                    -32002,
                    "missing Mcp-Session-Id header (call initialize first)",
                );
            };
            let Some(token) = bearer_token(&headers) else {
                return jsonrpc_error(request_id, -32001, "missing Worker grant bearer token");
            };
            let session = match state.session_for_token(&session_id, token).await {
                Ok(session) => session,
                Err(error) => return jsonrpc_error(request_id, -32001, &error),
            };
            let name = request["params"]["name"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            let arguments = request["params"]["arguments"]
                .as_object()
                .cloned()
                .unwrap_or_default();
            // 保持 session 在表中；只锁住它的可变 broker 状态，故同 session
            // 并发调用会排队，而不会在第一个请求 await 期间变成 unknown。
            let mut session = session.lock().await;
            let outcome = match dispatch_tool(&state, &mut session, &name, &arguments).await {
                Ok(text) => Ok(json!({"content": [{"type": "text", "text": text}]})),
                Err(error) => Ok(json!({
                    "content": [{"type": "text", "text": format!("tool error: {error}")}],
                    "isError": true,
                })),
            };
            (outcome, None)
        }
        "ping" => match authenticated_session(&state, &headers).await {
            Ok(_) => (Ok(json!({})), None),
            Err(error) => (Err(error), None),
        },
        other => return jsonrpc_error(request_id, -32601, &format!("method not found: {other}")),
    };

    let mut response = match result {
        Ok(result) => Json(json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "result": result,
        }))
        .into_response(),
        Err(error) => jsonrpc_error(request_id, -32001, &error),
    };
    if let Some(session_id) = session_header
        && let Ok(value) = HeaderValue::from_str(&session_id)
    {
        response.headers_mut().insert("mcp-session-id", value);
    }
    response
}

fn initialize(
    state: &Arc<McpServerState>,
    headers: &HeaderMap,
    request: &Value,
) -> Result<(Value, String), String> {
    let requested = request["params"]["protocolVersion"]
        .as_str()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(MCP_PROTOCOL_VERSION);
    let token = bearer_token(headers)
        .ok_or_else(|| "missing Worker grant bearer token for initialize".to_string())?;
    let (grant_id, grant) = state.grant_for_token(token)?;
    let session_id = state.insert_session(grant_id, grant)?;
    // MCP 版本协商（规范语义）：客户端声明的版本一律回显——本服务的
    // tools/list 与 tools/call 语义跨版本一致；直接 4xx 拒绝会让客户端
    // 的 MCP 初始化整体失败（实测 claude code 协商失败导致白板工具不可用）。
    Ok((
        json!({
            "protocolVersion": requested,
            "capabilities": {"tools": {"listChanged": false}},
            "serverInfo": {"name": "lynceus-mcp", "version": env!("CARGO_PKG_VERSION")},
        }),
        session_id,
    ))
}

async fn authenticated_session(
    state: &Arc<McpServerState>,
    headers: &HeaderMap,
) -> Result<Arc<AsyncMutex<McpSession>>, String> {
    let session_id = headers
        .get("mcp-session-id")
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| "missing Mcp-Session-Id header (call initialize first)".to_string())?;
    let token =
        bearer_token(headers).ok_or_else(|| "missing Worker grant bearer token".to_string())?;
    state.session_for_token(session_id, token).await
}

fn tool_list(session: &Arc<AsyncMutex<McpSession>>) -> Value {
    // The stable façade itself is always visible after authentication. Concrete
    // catalog tools are filtered in tool_search/describe/execute by the same
    // BrokerSession policy.
    let _ = session;
    let definitions = tool_definitions();
    json!({
        "tools": TOOL_NAMES.iter().filter_map(|name| {
            definitions.get(*name).map(|def| json!({
                "name": name,
                "description": def["description"],
                "inputSchema": def["inputSchema"],
            }))
        }).collect::<Vec<_>>()
    })
}

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .filter(|value| !value.is_empty())
}

fn validate_loopback_headers(headers: &HeaderMap) -> Result<(), String> {
    if let Some(host) = headers.get(HOST).and_then(|value| value.to_str().ok())
        && !is_loopback_authority(host)
    {
        return Err("MCP endpoint accepts only loopback Host values".to_string());
    }
    if let Some(origin) = headers.get(ORIGIN).and_then(|value| value.to_str().ok()) {
        let parsed = url::Url::parse(origin)
            .map_err(|_| "invalid Origin header for MCP request".to_string())?;
        if !matches!(parsed.scheme(), "http" | "https")
            || !is_loopback_authority(parsed.host_str().unwrap_or_default())
        {
            return Err("MCP endpoint accepts only loopback Origin values".to_string());
        }
    }
    Ok(())
}

fn is_loopback_authority(authority: &str) -> bool {
    let host = authority
        .strip_prefix('[')
        .and_then(|value| value.split_once(']').map(|(host, _)| host))
        .unwrap_or_else(|| {
            authority
                .rsplit_once(':')
                .map_or(authority, |(host, _)| host)
        });
    matches!(host, "localhost" | "127.0.0.1" | "::1")
}

/// 工具分派：稳定入口 → broker / repository。
async fn dispatch_tool(
    state: &Arc<McpServerState>,
    session: &mut McpSession,
    name: &str,
    arguments: &serde_json::Map<String, Value>,
) -> Result<String, String> {
    let text_of = |key: &str| {
        arguments
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| format!("missing required argument '{key}'"))
    };
    match name {
        "tool_list" => {
            let mut tools = state.broker.list_all().await?;
            tools.retain(|tool| {
                session
                    .broker
                    .allows(tool["tool_id"].as_str().unwrap_or_default())
            });
            tools.push(serde_json::json!({
                "hint": "call tool_describe with a tool_id to get its full input schema, then tool_execute to run it",
            }));
            serde_json::to_string_pretty(&tools).map_err(|e| e.to_string())
        }
        "tool_search" => {
            let query = text_of("query")?;
            let limit = arguments.get("limit").and_then(Value::as_u64).unwrap_or(5) as usize;
            let candidates = state.broker.search(&query, limit).await?;
            let candidates: Vec<Value> = candidates
                .into_iter()
                .filter(|candidate| {
                    candidate["tool_id"]
                        .as_str()
                        .is_some_and(|tool_id| session.broker.allows(tool_id))
                })
                .collect();
            serde_json::to_string_pretty(&candidates).map_err(|e| e.to_string())
        }
        "tool_describe" => {
            let tool_id = text_of("tool_id")?;
            if !session.broker.allows(&tool_id) {
                return Err(format!(
                    "tool '{tool_id}' is not allowed by the worker tool policy"
                ));
            }
            let description = state.broker.describe(&tool_id).await?;
            serde_json::to_string_pretty(&description).map_err(|e| e.to_string())
        }
        "tool_execute" => {
            let tool_id = text_of("tool_id")?;
            let args = arguments
                .get("arguments")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            if state.audit_writer().is_none() {
                return Err("MCP audit writer is not configured".to_string());
            }
            let invocation = state
                .broker
                .execute(&mut session.broker, &tool_id, &args, &session.scope)
                .await?;
            let artifacts = artifact_records(&invocation, &session.scope);
            let artifact_ids = artifacts
                .iter()
                .map(|artifact| artifact.id.to_string())
                .collect::<Vec<_>>();
            state
                .audit_writer()
                .ok_or_else(|| "MCP audit writer is not configured".to_string())?(
                invocation.clone(),
                artifacts,
            )?;
            serde_json::to_string_pretty(&json!({
                "invocation_id": invocation.id,
                "status": invocation.status.as_str(),
                "exit_code": invocation.exit_code,
                "duration_ms": invocation.duration_ms,
                "output_summary": invocation.output_summary,
                "artifact_paths": invocation.artifact_paths,
                "artifact_ids": artifact_ids,
                "error": invocation.error,
            }))
            .map_err(|e| e.to_string())
        }
        "knowledge_search" => {
            let query = text_of("query")?;
            let limit = arguments
                .get("limit")
                .and_then(Value::as_u64)
                .unwrap_or(8)
                .clamp(1, 20) as usize;
            let mut query_model = models::KnowledgeRetrievalQuery::new();
            query_model.text = query;
            query_model.limit = i64::try_from(limit).unwrap_or(8);
            let results = state
                .repository
                .search_knowledge_cards(&query_model)
                .map_err(|error| error.to_string())?;
            let cards: Vec<Value> = results
                .into_iter()
                .take(limit)
                .map(|result| {
                    json!({
                        "card_id": result.card.id,
                        "title": result.card.title,
                        "summary": result.card.summary,
                    })
                })
                .collect();
            serde_json::to_string_pretty(&cards).map_err(|e| e.to_string())
        }
        "knowledge_get" => {
            let card_id = text_of("card_id")?;
            let card = state
                .repository
                .list_knowledge_cards()
                .map_err(|error| error.to_string())?
                .into_iter()
                .find(|card| card.id.as_str() == card_id)
                .ok_or_else(|| format!("knowledge card '{card_id}' not found"))?;
            serde_json::to_string_pretty(&json!({
                "card_id": card.id,
                "title": card.title,
                "summary": card.summary,
                "body": card.body,
            }))
            .map_err(|e| e.to_string())
        }
        "evidence_propose" => {
            let invocation_id = arguments.get("invocation_id").and_then(Value::as_str);
            let artifact_id = arguments.get("artifact_id").and_then(Value::as_str);
            let locator = arguments.get("locator").and_then(Value::as_str);
            let summary = text_of("summary")?;
            let flag = arguments.get("flag").and_then(Value::as_str);
            let title = arguments.get("title").and_then(Value::as_str);
            let traffic_id = arguments.get("traffic_id").and_then(Value::as_str);
            evidence_propose(
                state,
                session,
                invocation_id,
                artifact_id,
                locator,
                &summary,
                flag,
                title,
                traffic_id,
            )
        }
        "blackboard_read" => blackboard_read(state, session, arguments),
        "blackboard_append" => blackboard_append(state, session, arguments),
        "blackboard_claim" => blackboard_claim(state, session, arguments),
        "traffic_search" => {
            let host = text_of("host")?;
            let body_contains = arguments
                .get("body_contains")
                .and_then(Value::as_str)
                .map(str::to_string);
            let limit = arguments
                .get("limit")
                .and_then(Value::as_u64)
                .unwrap_or(3)
                .clamp(1, 10) as usize;
            let Some(traffic) = crate::traffic::global_traffic() else {
                return Err(
                    "traffic recording is not enabled on this server (LYNCEUS_TRAFFIC_PROXY=0)"
                        .to_string(),
                );
            };
            let rows = traffic
                .store()
                .search(&host, body_contains.as_deref(), limit)
                .map_err(|error| error.to_string())?;
            let items: Vec<Value> = rows
                .into_iter()
                .map(|(id, method, url, status)| {
                    json!({"id": id, "method": method, "url": url, "status": status})
                })
                .collect();
            serde_json::to_string_pretty(&items).map_err(|e| e.to_string())
        }
        "traffic_get" => {
            let id = text_of("id")?;
            let Some(traffic) = crate::traffic::global_traffic() else {
                return Err(
                    "traffic recording is not enabled on this server (LYNCEUS_TRAFFIC_PROXY=0)"
                        .to_string(),
                );
            };
            let Some((req_head, req_body, resp_head, resp_body)) = traffic
                .store()
                .bodies(&id)
                .map_err(|error| error.to_string())?
            else {
                return Err(format!("traffic exchange '{id}' not found"));
            };
            // 有界预览：全文可能极大，先给头 + 截断的正文。
            let clip = |bytes: &[u8]| {
                let text = String::from_utf8_lossy(bytes);
                let clipped: String = text.chars().take(4000).collect();
                if text.chars().count() > 4000 {
                    format!("{clipped}\n…[truncated {} chars]", text.chars().count())
                } else {
                    clipped
                }
            };
            serde_json::to_string_pretty(&json!({
                "id": id,
                "request_head": req_head,
                "request_body": clip(&req_body),
                "response_head": resp_head,
                "response_body": clip(&resp_body),
            }))
            .map_err(|e| e.to_string())
        }
        "skill_list" => skill_list(state, session).await,
        "skill_load" => {
            let name = text_of("name")?;
            let args_len = arguments
                .get("name")
                .map(|value| value.to_string().len())
                .unwrap_or_default() as i64;
            skill_load(state, session, &name, args_len).await
        }
        other => Err(format!("unknown tool '{other}'")),
    }
}

fn scope_text(scope: &ExecutionScope, field: &str) -> Result<String, String> {
    match field {
        "project_id" => scope.project_id.as_ref().map(ToString::to_string),
        "mission_id" => scope.mission_id.as_ref().map(ToString::to_string),
        "run_id" => scope.run_id.as_ref().map(ToString::to_string),
        "task_id" => scope.task_id.as_ref().map(ToString::to_string),
        "branch_id" => scope.branch_id.as_ref().map(ToString::to_string),
        "intent_id" => scope.intent_id.clone(),
        "worker_id" => scope.worker_id.clone(),
        "worker_run_id" => scope.worker_run_id.clone(),
        other => None.or_else(|| Some(format!("unknown scope field '{other}'"))),
    }
    .ok_or_else(|| format!("Worker grant scope is missing {field}"))
}

fn reject_forged_scope_fields(
    arguments: &serde_json::Map<String, Value>,
    scope: &ExecutionScope,
) -> Result<(), String> {
    for field in [
        "project_id",
        "mission_id",
        "run_id",
        "task_id",
        "branch_id",
        "intent_id",
        "worker_id",
        "worker_run_id",
        "author_worker_run_id",
    ] {
        let Some(value) = arguments.get(field) else {
            continue;
        };
        let expected_field = if field == "author_worker_run_id" {
            "worker_run_id"
        } else {
            field
        };
        let expected = scope_text(scope, expected_field)?;
        if value.as_str() != Some(expected.as_str()) {
            return Err(format!(
                "{field} is server-bound and does not match the Worker grant"
            ));
        }
    }
    if arguments.contains_key("entry_id") || arguments.contains_key("sequence") {
        return Err("entry_id and sequence are server-assigned".to_string());
    }
    Ok(())
}

/// 本次 worker 的 Agent 预设（WP6 可见性授权依据）：
/// worker_run_id → WorkerRun.agent_preset_id。查不到为 None（不限制）。
async fn worker_agent_preset(
    state: &Arc<McpServerState>,
    session: &McpSession,
) -> Option<models::AgentPreset> {
    let worker_run_id = session.scope.worker_run_id.as_deref()?;
    let run = state.repository.get_worker_run(worker_run_id).ok()??;
    let preset_id = run.agent_preset_id.as_deref()?;
    state
        .repository
        .get_agent_preset(preset_id)
        .ok()
        .flatten()
        .filter(|preset| preset.enabled)
}

/// 预设的 skill 可见性清单：`skills` 非空 = 白名单（fail-closed）。
fn skill_visibility(preset: Option<&models::AgentPreset>) -> Option<Vec<String>> {
    preset
        .filter(|preset| !preset.skills.is_empty())
        .map(|preset| preset.skills.clone())
}

/// 台账落一行（best-effort：计账失败绝不打断 skill 调用）。
fn record_skill_usage(
    state: &Arc<McpServerState>,
    session: &McpSession,
    skill: &str,
    args_len: i64,
    found: bool,
    agent_preset: Option<&str>,
) {
    let row = models::skill::SkillUsageRow {
        ts: models::common::utcnow().isoformat(),
        skill: skill.chars().take(128).collect(),
        agent_preset: agent_preset.map(str::to_string),
        mission_id: session.scope.mission_id.as_ref().map(|id| id.as_str().to_string()),
        run_id: session.scope.run_id.as_ref().map(|id| id.as_str().to_string()),
        args_len,
        found,
    };
    if let Err(error) = state.repository.record_skill_usage(&row) {
        eprintln!("[skills] usage ledger write failed (best-effort): {error}");
    }
}

/// `skill_list`：可见技能 + 描述 + 声明模块（预设白名单过滤）。
async fn skill_list(
    state: &Arc<McpServerState>,
    session: &McpSession,
) -> Result<String, String> {
    let preset = worker_agent_preset(state, session).await;
    let visibility = skill_visibility(preset.as_ref());
    let mut skills = state
        .skills
        .read()
        .map_err(|_| "skill manager poisoned".to_string())?
        .list()?;
    if let Some(allowed) = visibility.as_ref() {
        skills.retain(|skill| allowed.contains(&skill.name));
    }
    let entries: Vec<serde_json::Value> = skills
        .iter()
        .map(|skill| {
            serde_json::json!({
                "name": skill.name,
                "description": skill.description,
                "modules": skill.modules,
            })
        })
        .collect();
    serde_json::to_string_pretty(&serde_json::json!({
        "skills": entries,
        "hint": "load a skill with skill_load(name) to get the full manual; loading unlocks only the tools declared in its modules",
    }))
    .map_err(|error| error.to_string())
}

/// `skill_load`：可见性校验（fail-closed）→ 返回手册正文 → 只解锁
/// frontmatter `modules:` 声明的 catalog 条目 → 台账记一行（含 miss）。
async fn skill_load(
    state: &Arc<McpServerState>,
    session: &mut McpSession,
    name: &str,
    args_len: i64,
) -> Result<String, String> {
    let preset = worker_agent_preset(state, session).await;
    let preset_key = preset.as_ref().map(|preset| preset.key.clone());
    let visibility = skill_visibility(preset.as_ref());

    let loaded = state
        .skills
        .read()
        .map_err(|_| "skill manager poisoned".to_string())?
        .load(name);
    let (meta, body) = match loaded {
        Ok(loaded) => loaded,
        Err(load_error) => {
            // 缺口记账：模型点名的 skill 不存在也记（found=false）。
            record_skill_usage(state, session, name, args_len, false, preset_key.as_deref());
            return Err(format!(
                "skill '{name}' not found or invalid: {load_error}"
            ));
        }
    };

    // 预设白名单：非空且未包含 → 拒绝（fail-closed；只记不载）。
    if let Some(allowed) = visibility.as_ref()
        && !allowed.contains(&meta.name)
    {
        record_skill_usage(state, session, name, args_len, true, preset_key.as_deref());
        return Err(format!(
            "skill '{}' is not visible to this worker's agent preset",
            meta.name
        ));
    }

    // fail-closed 解锁：只并入 frontmatter 声明的模块，且同时落到
    // grant.allowlist（断线重连后 session 重建仍保留解锁面）。
    if !meta.modules.is_empty() {
        session.broker.expand_allowlist(&meta.modules);
        if let Ok(mut grants) = state.grants.lock()
            && let Some(grant) = grants.get_mut(&session.grant_id)
        {
            if let Some(allowlist) = grant.allowlist.as_mut() {
                for id in &meta.modules {
                    if !allowlist.contains(id) {
                        allowlist.push(id.clone());
                    }
                }
            }
        }
    }

    record_skill_usage(state, session, name, args_len, true, preset_key.as_deref());
    serde_json::to_string_pretty(&serde_json::json!({
        "name": meta.name,
        "description": meta.description,
        "modules_unlocked": meta.modules,
        "manual": body,
        "hint": "the declared catalog tools are now allowed by your tool policy; describe each with tool_describe before tool_execute",
    }))
    .map_err(|error| error.to_string())
}

fn blackboard_read(
    state: &Arc<McpServerState>,
    session: &McpSession,
    arguments: &serde_json::Map<String, Value>,
) -> Result<String, String> {
    let project_id = scope_text(&session.scope, "project_id")?;
    let mission_id = scope_text(&session.scope, "mission_id")?;
    let run_id = scope_text(&session.scope, "run_id")?;
    let cursor = match arguments.get("cursor") {
        None => None,
        Some(Value::Number(value)) => value.as_i64(),
        Some(Value::String(value)) => Some(
            value
                .parse::<i64>()
                .map_err(|_| "cursor must be a non-negative integer".to_string())?,
        ),
        Some(_) => return Err("cursor must be a non-negative integer".to_string()),
    };
    if cursor.is_some_and(|value| value < 0) {
        return Err("cursor must be a non-negative integer".to_string());
    }
    let kind = arguments
        .get("kind")
        .and_then(Value::as_str)
        .map(BlackboardEntryKind::parse)
        .transpose()
        .map_err(|value| {
            format!(
                "unknown blackboard kind '{value}'; valid kinds: {}",
                BlackboardEntryKind::valid_values()
            )
        })?;
    let limit = arguments
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(20)
        .clamp(1, 100) as usize;
    let context_byte_budget = arguments
        .get("context_byte_budget")
        .and_then(Value::as_u64)
        .unwrap_or(16 * 1024)
        .clamp(1024, 65_536) as usize;
    let (entries, next_cursor) = state
        .repository
        .list_blackboard_entries(
            &project_id,
            &mission_id,
            &run_id,
            cursor,
            kind,
            limit,
            context_byte_budget,
        )
        .map_err(|error| error.to_string())?;
    serde_json::to_string_pretty(&json!({
        "entries": entries,
        "next_cursor": next_cursor,
        "provenance": {
            "project_id": project_id,
            "mission_id": mission_id,
            "run_id": run_id,
            "worker_run_id": session.scope.worker_run_id,
        }
    }))
    .map_err(|error| error.to_string())
}

fn blackboard_append(
    state: &Arc<McpServerState>,
    session: &McpSession,
    arguments: &serde_json::Map<String, Value>,
) -> Result<String, String> {
    reject_forged_scope_fields(arguments, &session.scope)?;
    let project_id = scope_text(&session.scope, "project_id")?;
    let mission_id = scope_text(&session.scope, "mission_id")?;
    let run_id = scope_text(&session.scope, "run_id")?;
    let author = scope_text(&session.scope, "worker_run_id")?;
    let kind = arguments
        .get("kind")
        .and_then(Value::as_str)
        .ok_or_else(|| "missing required argument 'kind'".to_string())
        .and_then(|value| {
            BlackboardEntryKind::parse(value)
                .map_err(|_| {
                    format!(
                        "unknown blackboard kind '{value}'; valid kinds: {}",
                        BlackboardEntryKind::valid_values()
                    )
                })
        })?;
    let idempotency_key = arguments
        .get("idempotency_key")
        .and_then(Value::as_str)
        .ok_or_else(|| "missing required argument 'idempotency_key'".to_string())?
        .to_string();
    let content = arguments
        .get("content")
        .and_then(Value::as_str)
        .map(str::to_string);
    let artifact_id = arguments
        .get("artifact_id")
        .and_then(Value::as_str)
        .map(str::to_string);
    let evidence_id = arguments
        .get("evidence_id")
        .and_then(Value::as_str)
        .map(str::to_string);
    let locator = arguments
        .get("locator")
        .and_then(Value::as_str)
        .map(str::to_string);
    let mut entry = BlackboardEntry::new(
        models::ProjectId::new(project_id),
        models::MissionId::new(mission_id),
        models::RunId::new(run_id),
        author,
        kind,
        content,
        artifact_id.clone(),
        evidence_id.clone(),
        locator.clone(),
        idempotency_key,
    );
    entry.branch_id = session.scope.branch_id.clone();
    entry.task_id = session.scope.task_id.clone();
    entry.intent_id = session.scope.intent_id.clone();
    entry.validate().map_err(|error| error.to_string())?;
    if let Some(artifact_id) = artifact_id {
        validate_blackboard_artifact(state, session, &entry, &artifact_id)?;
    }
    if let Some(evidence_id) = evidence_id {
        validate_blackboard_evidence(state, session, &entry, &evidence_id)?;
    }
    let stored = state
        .repository
        .append_blackboard_entry(&entry)
        .map_err(|error| error.to_string())?;
    serde_json::to_string_pretty(&stored).map_err(|error| error.to_string())
}

fn validate_blackboard_artifact(
    state: &Arc<McpServerState>,
    session: &McpSession,
    entry: &BlackboardEntry,
    artifact_id: &str,
) -> Result<(), String> {
    let artifact = state
        .repository
        .get_artifact_record(artifact_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("ArtifactRecord '{artifact_id}' does not exist"))?;
    if artifact.project_id.as_ref() != Some(&entry.project_id)
        || artifact.run_id.as_ref() != Some(&entry.run_id)
        || artifact.task_id.as_ref()
            != Some(
                entry
                    .task_id
                    .as_ref()
                    .ok_or_else(|| "artifact reference requires task scope".to_string())?,
            )
        || entry
            .locator
            .as_deref()
            .is_some_and(|locator| locator != artifact.uri)
    {
        return Err("ArtifactRecord reference is outside the Worker grant scope".to_string());
    }
    let Some(invocation_id) = artifact.tool_invocation_id.as_ref() else {
        return Err("ArtifactRecord reference must point to a ToolInvocation".to_string());
    };
    let invocation = state
        .repository
        .list_tool_invocations(None)
        .map_err(|error| error.to_string())?
        .into_iter()
        .find(|invocation| invocation.id == *invocation_id)
        .ok_or_else(|| "ArtifactRecord ToolInvocation does not exist".to_string())?;
    if invocation.project_id.as_ref() != Some(&entry.project_id)
        || invocation.mission_id.as_ref() != Some(&entry.mission_id)
        || invocation.run_id.as_ref() != Some(&entry.run_id)
        || invocation.task_id.as_ref() != entry.task_id.as_ref()
        || invocation
            .metadata
            .get("worker_run_id")
            .and_then(Value::as_str)
            != session.scope.worker_run_id.as_deref()
    {
        return Err("ArtifactRecord ToolInvocation is outside the Worker grant scope".to_string());
    }
    Ok(())
}

fn validate_blackboard_evidence(
    state: &Arc<McpServerState>,
    session: &McpSession,
    entry: &BlackboardEntry,
    evidence_id: &str,
) -> Result<(), String> {
    let evidence = state
        .repository
        .list_evidence(entry.project_id.as_str())
        .map_err(|error| error.to_string())?
        .into_iter()
        .find(|evidence| evidence.id.as_str() == evidence_id)
        .ok_or_else(|| format!("Evidence '{evidence_id}' does not exist"))?;
    if evidence.mission_id.as_ref() != Some(&entry.mission_id)
        || evidence.run_id.as_ref() != Some(&entry.run_id)
        || evidence.produced_by_task_id.as_ref() != entry.task_id.as_ref()
        || entry.task_id.is_none()
        || session.scope.worker_run_id.is_none()
    {
        return Err("Evidence reference is outside the Worker grant scope".to_string());
    }
    Ok(())
}

fn blackboard_claim(
    state: &Arc<McpServerState>,
    session: &McpSession,
    arguments: &serde_json::Map<String, Value>,
) -> Result<String, String> {
    let task_id = arguments
        .get("task_id")
        .and_then(Value::as_str)
        .ok_or_else(|| "missing required argument 'task_id'".to_string())?;
    let expected_task_id = scope_text(&session.scope, "task_id")?;
    if task_id != expected_task_id {
        return Err("task_id is server-bound and does not match the Worker grant".to_string());
    }
    let project_id = scope_text(&session.scope, "project_id")?;
    let mission_id = scope_text(&session.scope, "mission_id")?;
    let run_id = scope_text(&session.scope, "run_id")?;
    let worker_id = scope_text(&session.scope, "worker_id")?;
    let worker_run_id = scope_text(&session.scope, "worker_run_id")?;
    let lease_seconds = arguments
        .get("lease_seconds")
        .and_then(Value::as_i64)
        .unwrap_or(300)
        .clamp(1, 3600);
    let lease = state
        .repository
        .claim_worker_lease(
            &project_id,
            &mission_id,
            &run_id,
            task_id,
            &worker_id,
            &worker_run_id,
            lease_seconds,
        )
        .map_err(|error| error.to_string())?
        .ok_or_else(|| {
            "task is already claimed, terminal, expired, or outside scope".to_string()
        })?;
    serde_json::to_string_pretty(&lease).map_err(|error| error.to_string())
}

/// 只生成候选引用，不接受正文、不落 Evidence。
fn evidence_propose(
    state: &Arc<McpServerState>,
    session: &McpSession,
    invocation_id: Option<&str>,
    artifact_id: Option<&str>,
    locator: Option<&str>,
    summary: &str,
    flag: Option<&str>,
    title: Option<&str>,
    traffic_id: Option<&str>,
) -> Result<String, String> {
    if summary.trim().is_empty() || summary.chars().count() > 2000 {
        return Err("summary must be non-empty and at most 2000 characters".to_string());
    }
    let project_id = session
        .scope
        .project_id
        .clone()
        .ok_or_else(|| "worker grant scope is missing project_id".to_string())?;
    // 证据来源二选一：一条已录制的流量交换（worker 用 curl 直采、进了 traffic），
    // 或一次 tool_execute 的 ToolInvocation+ArtifactRecord。两条路都归一到
    // "真实执行产出 + 磁盘密封工件 + SHA-256"，确认门因此复用同一套校验。
    let (invocation, artifact, synthesized) = if let Some(traffic_id) = traffic_id {
        materialize_traffic_evidence(session, traffic_id)?
    } else {
        let (invocation_id, artifact_id, locator) = match (invocation_id, artifact_id, locator) {
            (Some(invocation_id), Some(artifact_id), Some(locator)) => {
                (invocation_id, artifact_id, locator)
            }
            _ => {
                return Err(
                    "provide either traffic_id, or all of invocation_id + artifact_id + locator"
                        .to_string(),
                );
            }
        };
        resolve_tool_evidence(state, session, invocation_id, artifact_id, locator)?
    };
    // 流量来源的 ToolInvocation/ArtifactRecord 是本次新合成的，落库留痕；
    // tool_execute 来源的已由执行路径落库，不重复写。
    if synthesized && let Some(writer) = state.audit_writer() {
        writer(invocation.clone(), vec![artifact.clone()]).map_err(|error| error)?;
    }

    // —— 观察 → 发现 的桥 ——
    // 证据必须绑定到**真实执行产出**：ToolInvocation + 它的 ArtifactRecord。
    // evidence_path + fingerprint 把它钉在磁盘工件的真实字节上，确认门才能校验。
    let mut evidence = Evidence::new(
        project_id.clone(),
        EvidenceKind::ToolOutput,
        summary.to_string(),
    );
    evidence.mission_id = invocation.mission_id.clone();
    evidence.branch_id = session.scope.branch_id.clone();
    evidence.run_id = invocation.run_id.clone();
    evidence.produced_by_task_id = invocation.task_id.clone();
    evidence.produced_by_tool_invocation_id = Some(invocation.id.clone());
    evidence.evidence_path = Some(artifact.uri.clone());
    evidence.fingerprint = artifact.sha256.clone();
    let flag = flag.map(str::trim).filter(|value| !value.is_empty());
    if let Some(flag) = flag {
        evidence
            .content
            .insert("flag".to_string(), Value::String(flag.to_string()));
    }
    evidence.content.insert(
        "invocation_id".to_string(),
        Value::String(invocation.id.as_str().to_string()),
    );
    if let Some(traffic_id) = traffic_id {
        evidence
            .content
            .insert("traffic_id".to_string(), Value::String(traffic_id.to_string()));
    }
    let stored_evidence = state
        .repository
        .add_evidence(&evidence)
        .map_err(|error| error.to_string())?;

    // 候选 Finding 过每一道不可协商门（Guardian + Provenance + flag_capture
    // 产品门）。只有全过才落库为 confirmed；否则把拒绝原因回给 worker——
    // 模型无法把幻觉发现蒙过关，但真实工具产出可以成为确认发现。
    let mut finding = Finding::new(project_id, finding_title(title, flag, summary));
    finding.mission_id = invocation.mission_id.clone();
    finding.branch_id = session.scope.branch_id.clone();
    finding.run_id = invocation.run_id.clone();
    finding.produced_by_task_id = invocation.task_id.clone();
    finding.evidence_ids = vec![stored_evidence.id.as_str().to_string()];
    if flag.is_some() {
        finding.rule_id = Some("web_exploit.flag_capture".to_string());
    }
    finding.status = FindingStatus::Confirmed;

    let decision = FindingVerificationService::check_confirmation(
        &finding,
        std::slice::from_ref(&stored_evidence),
        std::slice::from_ref(&invocation),
    );
    if !decision.allowed() {
        return serde_json::to_string_pretty(&json!({
            "kind": "evidence_candidate",
            "status": "rejected",
            "evidence_id": stored_evidence.id,
            "reasons": decision.reasons(),
        }))
        .map_err(|error| error.to_string());
    }
    if let Some(product) = decision.product_verification() {
        finding.review.insert(
            "product_verification".to_string(),
            serde_json::to_value(product).unwrap_or(Value::Null),
        );
    }
    let finding = finding.validated().map_err(|error| error.to_string())?;
    let stored_finding = state
        .repository
        .add_finding(&finding)
        .map_err(|error| error.to_string())?;
    serde_json::to_string_pretty(&json!({
        "kind": "evidence_candidate",
        "status": "confirmed",
        "evidence_id": stored_evidence.id,
        "finding_id": stored_finding.id,
        "flag": flag,
        "product_verification": decision.product_verification(),
    }))
    .map_err(|error| error.to_string())
}

/// tool_execute 来源：校验并取出已持久化的 ToolInvocation + ArtifactRecord。
/// 返回 `(invocation, artifact, synthesized=false)`。
fn resolve_tool_evidence(
    state: &Arc<McpServerState>,
    session: &McpSession,
    invocation_id: &str,
    artifact_id: &str,
    locator: &str,
) -> Result<(ToolInvocation, ArtifactRecord, bool), String> {
    let invocation = state
        .repository
        .list_tool_invocations(None)
        .map_err(|error| error.to_string())?
        .into_iter()
        .find(|invocation| invocation.id.as_str() == invocation_id)
        .ok_or_else(|| format!("ToolInvocation '{invocation_id}' does not exist"))?;
    let artifact = state
        .repository
        .get_artifact_record(artifact_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("ArtifactRecord '{artifact_id}' does not exist"))?;
    if artifact.tool_invocation_id.as_ref().map(|id| id.as_str()) != Some(invocation_id)
        || artifact.uri != locator
    {
        return Err(
            "EvidenceCandidate must reference the invocation's exact artifact locator".to_string(),
        );
    }
    if invocation.project_id != session.scope.project_id
        || invocation.mission_id != session.scope.mission_id
        || invocation.run_id != session.scope.run_id
        || invocation.task_id != session.scope.task_id
        || artifact.project_id != session.scope.project_id
        || artifact.run_id != session.scope.run_id
        || artifact.task_id != session.scope.task_id
        || invocation
            .metadata
            .get("worker_run_id")
            .and_then(Value::as_str)
            != session.scope.worker_run_id.as_deref()
    {
        return Err("EvidenceCandidate reference is outside the Worker grant scope".to_string());
    }
    Ok((invocation, artifact, false))
}

/// 流量来源：把一条已录制的 HTTP 交换的**响应正文**字节精确物化成密封工件，
/// 并合成一个 `status=Ok` 的 ToolInvocation + ArtifactRecord——于确认门而言，
/// 它与 tool_execute 产出同构（真实执行 + 磁盘工件 + SHA-256），无需为流量
/// 单开一套门。返回 `(invocation, artifact, synthesized=true)`。
fn materialize_traffic_evidence(
    session: &McpSession,
    traffic_id: &str,
) -> Result<(ToolInvocation, ArtifactRecord, bool), String> {
    let traffic = crate::traffic::global_traffic()
        .ok_or_else(|| "traffic recording is not enabled on this server".to_string())?;
    let (_req_head, _req_body, _resp_head, resp_body) = traffic
        .store()
        .bodies(traffic_id)?
        .ok_or_else(|| format!("traffic exchange '{traffic_id}' not found"))?;
    // 大响应被外溢成占位符时拿不到正文——如实报错，绝不拿占位符当证据。
    if resp_body.windows(6).any(|window| window == b"[blob ") {
        return Err(format!(
            "traffic exchange '{traffic_id}' body was spilled to a blob (not inline); \
             reference an exchange whose response is inline"
        ));
    }
    let dir = session
        .scope
        .artifact_dir
        .clone()
        .unwrap_or_else(|| std::path::PathBuf::from("data").join("traffic-evidence"));
    std::fs::create_dir_all(&dir).map_err(|error| format!("create evidence dir: {error}"))?;
    let safe_id: String = traffic_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let path = dir.join(format!("traffic-{safe_id}.resp"));
    // 字节精确落盘（不换行/编码转换）：指纹 = 磁盘真实字节的 SHA-256。
    std::fs::write(&path, &resp_body).map_err(|error| format!("write traffic evidence: {error}"))?;
    let sha256 = format!("{:x}", Sha256::digest(&resp_body));
    let locator = path.to_string_lossy().into_owned();

    let mut invocation =
        ToolInvocation::new("http_traffic".to_string(), format!("traffic:{traffic_id}"));
    invocation.project_id = session.scope.project_id.clone();
    invocation.mission_id = session.scope.mission_id.clone();
    invocation.run_id = session.scope.run_id.clone();
    invocation.task_id = session.scope.task_id.clone();
    invocation.status = ToolStatus::Ok;
    invocation.artifact_paths.push(locator.clone());
    invocation
        .metadata
        .insert("sha256".to_string(), json!(sha256));
    invocation
        .metadata
        .insert("traffic_id".to_string(), json!(traffic_id));
    if let Some(worker_run_id) = session.scope.worker_run_id.clone() {
        invocation
            .metadata
            .insert("worker_run_id".to_string(), json!(worker_run_id));
    }

    let mut artifact = ArtifactRecord::new(locator);
    artifact.project_id = session.scope.project_id.clone();
    artifact.run_id = session.scope.run_id.clone();
    artifact.task_id = session.scope.task_id.clone();
    artifact.tool_invocation_id = Some(invocation.id.clone());
    artifact.kind = ArtifactKind::RawOutput;
    artifact.sha256 = Some(sha256);
    artifact.mime_type = Some("application/octet-stream".to_string());
    artifact.size_bytes = i64::try_from(resp_body.len()).ok();
    Ok((invocation, artifact, true))
}

/// 候选 Finding 标题：显式 title > flag 捕获 > summary 首行（有界）。
fn finding_title(title: Option<&str>, flag: Option<&str>, summary: &str) -> String {
    if let Some(title) = title.map(str::trim).filter(|value| !value.is_empty()) {
        return title.chars().take(200).collect();
    }
    if let Some(flag) = flag {
        return format!("Flag captured: {flag}");
    }
    summary
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("Worker-proposed finding")
        .chars()
        .take(200)
        .collect()
}

fn artifact_records(invocation: &ToolInvocation, scope: &ExecutionScope) -> Vec<ArtifactRecord> {
    invocation
        .artifact_paths
        .iter()
        .map(|path| {
            let mut artifact = ArtifactRecord::new(path.clone());
            artifact.project_id = scope.project_id.clone();
            artifact.run_id = scope.run_id.clone();
            artifact.task_id = scope.task_id.clone();
            artifact.tool_invocation_id = Some(invocation.id.clone());
            artifact.kind = ArtifactKind::RawOutput;
            artifact.summary = invocation.output_summary.clone();
            artifact.mime_type = Some("application/octet-stream".to_string());
            artifact.size_bytes = std::fs::metadata(path)
                .ok()
                .and_then(|metadata| i64::try_from(metadata.len()).ok());
            if let Some(sha256) = invocation.metadata.get("sha256") {
                artifact
                    .metadata
                    .insert("sha256".to_string(), sha256.clone());
                artifact.sha256 = sha256.as_str().map(str::to_string);
            }
            artifact
        })
        .collect()
}

fn jsonrpc_error(request_id: Value, code: i64, message: &str) -> Response {
    (
        StatusCode::OK,
        Json(json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "error": {"code": code, "message": message},
        })),
    )
        .into_response()
}
