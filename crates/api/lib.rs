//! Lynceus HTTP API —— `server/api/` 的 Rust 适配层。
//!
//! 该 crate 以 axum 暴露阶段 0 冻结的 REST 契约。所有状态写入仍经过
//! [`runtime::AuditManager`]，因此 HTTP 层只负责解析/校验请求、
//! 调用门面并把领域错误映射成稳定的 JSON 错误响应。路由会持续按
//! `contracts/openapi.json` 增量补齐；未迁移的路径不会被伪装成成功。

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
#![allow(clippy::doc_markdown)]
#![allow(clippy::module_name_repetitions)]

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::Path as FsPath;
use std::path::PathBuf;
use std::sync::Arc;

use agents::worker::WorkerRuntimeSelector as _;
use axum::extract::{Path, Query, State};
use axum::http::header::CONTENT_TYPE;
use axum::http::{HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, patch, post};
use axum::{Json, Router};
use engines::tool_catalog::{
    ToolCatalogEntry, ToolCatalogError, ToolHealthResult, ToolInstallCoordinator, ToolInstallJob,
    ToolRecommendation, configure_local_tool, load_catalog, local_tools_config_path,
    recommend_tools, test_tool_health,
};
use engines::tool_settings;
use models::{
    ApprovalMode, AuditDomain, DecisionAnswer, EvidenceKind, ExecutionBackendType, ExecutionJob,
    ExecutionRequest, ExecutionStatus, Mission, MissionId, MissionStartResult,
    ModuleConfig, ModuleProfile, ModuleTransport, ModuleType, Project, ProjectId, ProviderConfig,
    ProviderHealthResult, ProviderModelDiscoveryResult, ProviderType, RuntimeSetting,
    RuntimeSettingScope, Severity, StrMap, UserDirectiveType, normalize_module_domain, utcnow,
};
use runtime::errors::EngineError;
use runtime::mission_lifecycle::CreateMissionInput;
use runtime::{
    AuditManager, ExecutionControlError, ExecutionControlPlane, InMemoryTaskBackend,
    SubmitExecutionOptions,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub mod coverage_graph;
pub mod intake;
pub mod report;
pub mod retest_context;
pub mod mission_workspace;
pub mod streams;
pub mod uploads;
use sha2::{Digest, Sha256};

/// API 共享状态。
#[derive(Clone)]
pub struct ApiState {
    /// Rust 编排门面；所有持久化写入必须通过它完成。
    pub manager: Arc<AuditManager>,
    /// 持久、有界的工具执行监督器。
    pub execution_control: Arc<ExecutionControlPlane>,
    /// 可观察、allow-listed 的工具安装协调器。
    pub tool_installs: Arc<ToolInstallCoordinator>,
    /// 当前进程使用的本地工具配置文件。
    pub local_tools_path: PathBuf,
    /// 上传暂存根目录（`LYNCEUS_UPLOAD_DIR` 家族，构造时解析一次）。
    pub upload_root: PathBuf,
    /// Mission workspace 根目录（构造时解析一次）。
    pub mission_workspace_root: PathBuf,
    /// Intelligence Hub pipeline（source 集 + 仓储；测试可用
    /// [`ApiState::with_intelligence_sources`] 注入 mock source）。
    pub intelligence: Arc<intelligence::IntelligencePipeline>,
    /// 对外报告的服务版本。
    pub version: &'static str,
    /// lynceus-mcp Tool Broker server（/mcp；统一工具入口）。
    pub mcp: Arc<engines::broker::mcp::McpServerState>,
    /// LiteLLM Gateway sidecar（worker 统一模型网关；gateway.yaml 驱动）。
    pub gateway: Arc<engines::worker::gateway::GatewayManager>,
}

/// 构造并登记进程级 LiteLLM Gateway manager（组合根一次性调用；
/// registry 经 [`engines::worker::gateway::global_gateway`] 读取改写）。
///
/// 注入仓储：收养一个**外部拉起**的网关时（用户自己 `uv tool uvx litellm`），
/// 要靠它把 `gateway.yaml` 的 `agents` 绑定展开成 `models`——否则 Gateway
/// 页面上的模型列表是空的。
fn attach_gateway_manager(
    repository: std::sync::Arc<dyn storage::Repository>,
) -> std::sync::Arc<engines::worker::gateway::GatewayManager> {
    let manager = std::sync::Arc::new(engines::worker::gateway::GatewayManager::default());
    manager.set_repository(repository);
    engines::worker::gateway::attach_global_gateway(std::sync::Arc::clone(&manager));
    manager
}

/// 解析上传根目录（Python `_upload_root` 镜像，`LYNCEUS_UPLOAD_DIR` /
/// `LYNCEUS_WORKSPACE_DIR/_staging/uploads` / 默认 `data/_staging/uploads`）。
#[must_use]
pub fn upload_root_from_env() -> PathBuf {
    if let Some(raw) = std::env::var_os("LYNCEUS_UPLOAD_DIR").filter(|value| !value.is_empty()) {
        return mission_workspace::resolved_path(FsPath::new(&raw));
    }
    if let Some(workspace) =
        std::env::var_os("LYNCEUS_WORKSPACE_DIR").filter(|value| !value.is_empty())
    {
        return mission_workspace::resolved_path(
            FsPath::new(&workspace)
                .join("_staging")
                .join("uploads")
                .as_path(),
        );
    }
    mission_workspace::resolved_path(FsPath::new("data/_staging/uploads"))
}

/// API 共享状态初始化失败。
#[derive(Debug, thiserror::Error)]
pub enum ApiStateInitError {
    /// 执行控制面初始化失败。
    #[error(transparent)]
    Execution(#[from] ExecutionControlError),
    /// 内嵌 Tool Catalog 初始化失败。
    #[error(transparent)]
    ToolCatalog(#[from] ToolCatalogError),
}

impl ApiState {
    /// 使用当前 crate 版本构造 API 状态。
    ///
    /// # Errors
    /// 固定执行后端注册失败时返回错误；启动方不得在无执行监督器时继续。
    pub fn new(manager: Arc<AuditManager>) -> Result<Self, ApiStateInitError> {
        let execution_control = Arc::new(ExecutionControlPlane::safe_local(Arc::clone(
            manager.repository(),
        ))?);
        Ok(Self::with_execution_control(manager, execution_control)?)
    }

    /// 使用调用方已注册 allow-listed 后端的执行控制器构造状态。
    ///
    /// # Errors
    /// 内嵌 Tool Catalog 无法解析或验证时返回错误。
    pub fn with_execution_control(
        manager: Arc<AuditManager>,
        execution_control: Arc<ExecutionControlPlane>,
    ) -> Result<Self, ToolCatalogError> {
        let local_tools_path = local_tools_config_path();
        let tool_installs = Arc::new(ToolInstallCoordinator::new(local_tools_path.clone())?);
        Ok(Self::with_services(
            manager,
            execution_control,
            tool_installs,
            local_tools_path,
            upload_root_from_env(),
            mission_workspace::mission_workspace_root_from_env(),
        ))
    }

    /// 使用显式服务构造状态，供嵌入方与隔离测试注入路径。
    #[must_use]
    pub fn with_services(
        manager: Arc<AuditManager>,
        execution_control: Arc<ExecutionControlPlane>,
        tool_installs: Arc<ToolInstallCoordinator>,
        local_tools_path: PathBuf,
        upload_root: PathBuf,
        mission_workspace_root: PathBuf,
    ) -> Self {
        // WP4：Agent 预设首次幂等播种（first-insert-only，DB 成为
        // 可编辑权威源、用户编辑跨重启保留）+ 分工提示词覆盖表装载。
        seed_agent_presets(&manager);
        refresh_agent_prompt_overrides(&manager);
        let intelligence = Arc::new(intelligence::IntelligencePipeline::new(
            intelligence::default_sources(),
            Arc::clone(manager.repository()),
        ));
        let mcp = engines::broker::mcp::McpServerState::new(Arc::clone(manager.repository()));
        manager.set_worker_mcp(Some(Arc::clone(&mcp)));
        let audit_manager = Arc::downgrade(&manager);
        mcp.set_audit_writer(Some(Arc::new(move |invocation, artifacts| {
            let audit_manager = audit_manager
                .upgrade()
                .ok_or_else(|| "AuditManager is no longer available".to_string())?;
            audit_manager
                .persist_mcp_audit(invocation, artifacts)
                .map_err(|error| error.to_string())
        })));
        // 先取仓储再进结构体字面量：`manager` 会在 `Self { manager, .. }`
        // 里被 move。
        let gateway = attach_gateway_manager(Arc::clone(manager.repository()));
        Self {
            manager,
            execution_control,
            tool_installs,
            local_tools_path,
            upload_root,
            mission_workspace_root,
            intelligence,
            version: env!("CARGO_PKG_VERSION"),
            mcp,
            gateway,
        }
    }

    /// 覆盖 Intelligence source 集（测试注入 mock server 的 source；
    /// 生产保持 [`intelligence::default_sources`]）。
    #[must_use]
    pub fn with_intelligence_sources(
        mut self,
        sources: Vec<Arc<dyn intelligence::IntelligenceSource>>,
    ) -> Self {
        self.intelligence = Arc::new(intelligence::IntelligencePipeline::new(
            sources,
            Arc::clone(self.manager.repository()),
        ));
        self
    }
}

/// 挂接真实 provider 运行时（production composition root 与测试共用
/// 的唯一通道）：OpenAI 兼容网关 + 带熔断的 purpose 路由。
///
/// 历史 bug：该注入只存在于设想中，全仓无人调用，`/providers/
/// discover-models`、`/providers/{id}/test`、`/capabilities` 因此永远
/// 503。提取为公开函数后，main 与集成测试走同一条注入路径，回归
/// 可测。
///
/// # Errors
/// 网关初始化失败（HTTP 客户端构建异常，实际不可达）。
pub fn attach_provider_runtimes(
    manager: &AuditManager,
    repository: Arc<dyn storage::Repository>,
) -> Result<(), engines::model_providers::GatewayError> {
    let gateway =
        Arc::new(engines::model_providers::OpenAiCompatibleProviderRuntime::new(repository)?);
    let router = Arc::new(engines::model_providers::ProviderRouterRuntime::new(
        gateway,
    ));
    let runtime: Arc<dyn agents::llm::ProviderRuntime> = router.clone();
    manager.set_provider_runtime(Some(runtime));
    manager.set_provider_discovery_runtime(Some(router));
    Ok(())
}

/// 构造 production 使用的唯一 manager composition：solver registry、
/// task backend 与两个 provider runtime 槽位一次性完成装配。
///
/// # Errors
/// Provider HTTP client 初始化失败。
pub fn build_production_manager(
    repository: Arc<dyn storage::Repository>,
) -> Result<Arc<AuditManager>, engines::model_providers::GatewayError> {
    let manager = Arc::new(AuditManager::new(
        Arc::clone(&repository),
        engines::default_solver_registry(),
        Arc::new(InMemoryTaskBackend::default()),
    ));
    attach_provider_runtimes(&manager, Arc::clone(&repository))?;
    // 外部 Worker Runtime：组合根唯一装配点。分支执行经此派发到用户
    // 本机安装的 Agent CLI（未绑定 Connection → NotReady，显式失败）。
    let worker_registry = engines::worker::attach_global_registry(repository);
    manager.set_worker_runtime(Some(worker_registry));
    // 流量录制（P3）：嵌入式 MITM 代理，worker 子进程经 env 注入流入。
    // 启动失败 / `LYNCEUS_TRAFFIC_PROXY=0` 禁用都不阻塞——worker 照常
    // 执行，只是没有流量证据；注入侧见 `adapters::execute`。
    let traffic_enabled = std::env::var_os("LYNCEUS_TRAFFIC_PROXY")
        .map(|value| value != "0" && value.to_string_lossy().to_lowercase() != "false")
        .unwrap_or(true);
    if traffic_enabled {
        engines::traffic::attach_global_traffic(
            std::path::Path::new("data"),
            engines::traffic::DEFAULT_PROXY_ADDR,
        );
    }
    // Skill 播种：内置 skill 随仓库发在 `resources/skills/`，而运行时根
    // `data/skills` 被 `.gitignore` 忽略——不播种的话全新 clone 上 `/skills`
    // 恒空、MCP 的 skill_list/skill_load 全部找不到东西。只增不覆盖：用户经
    // UI/API 建过或改过的 skill 是权威，绝不被发货内容冲掉，因此重复启动幂等。
    // 失败绝不阻塞启动：技能不可用只是能力缺失，服务照常提供。
    let seeded = engines::skills::seed_skills(
        &engines::skills::default_resources_root(),
        &std::path::PathBuf::from(
            std::env::var("LYNCEUS_SKILLS_DIR")
                .unwrap_or_else(|_| "data/skills".to_string()),
        ),
    );
    if !seeded.is_empty() {
        eprintln!(
            "seeded {} built-in skill(s) into the runtime skill root: {}",
            seeded.len(),
            seeded.join(", ")
        );
    }
    Ok(manager)
}

/// 构造 axum 路由。
///
/// 当前已落地健康检查、Project/Mission 核心 CRUD 与 Mission 启动路径；
/// 路由采用无 `/api` 前缀形式，与冻结契约和现有 frontend 客户端一致。
#[allow(clippy::too_many_lines)]
// Route registration is intentionally kept in one place so the frozen
// contract can be audited against a single list of paths.
pub fn router(state: ApiState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route(
            "/missions/{mission_id}/findings/{finding_id}/retests",
            get(list_mission_finding_retests).post(start_mission_finding_retest),
        )
        .route(
            "/missions/{mission_id}/retests/active",
            get(list_active_mission_retests),
        )
        .route("/reports/{report_id}", get(report::get_report))
        .route("/capabilities", get(list_capabilities))
        .route("/engines", get(list_engines))
        .route(
            "/mcp",
            axum::routing::post(engines::broker::mcp::mcp_handler).with_state(state.mcp.clone()),
        )
        .route("/worker-runtimes", get(list_worker_runtimes))
        .route("/worker-runtimes/refresh", post(refresh_worker_runtimes))
        .route("/worker-runtimes/{runtime_id}", get(get_worker_runtime))
        .route("/worker-command-policy", get(get_worker_command_policy))
        .route(
            "/worker-runtime-profiles",
            get(list_worker_runtime_profiles).post(upsert_worker_runtime_profile),
        )
        .route(
            "/worker-runtime-profiles/{profile_id}",
            delete(delete_worker_runtime_profile),
        )
        .route(
            "/agent-presets",
            get(list_agent_presets).post(create_agent_preset),
        )
        .route(
            "/agent-presets/{key}",
            get(get_agent_preset)
                .patch(update_agent_preset)
                .delete(delete_agent_preset),
        )
        .route("/agent-presets/{key}/preview", post(preview_agent_preset))
        .route(
            "/skills",
            get(list_skills).post(create_skill),
        )
        .route("/skills/upload", post(upload_skill_zip))
        .route("/skills/missing", get(skill_missing_report))
        .route(
            "/skills/{name}",
            get(get_skill).delete(delete_skill),
        )
        .route("/skills/{name}/usage", get(get_skill_usage))
        .route("/skills/{name}/files", get(list_skill_files))
        .route(
            "/skills/{name}/file",
            get(read_skill_file).put(write_skill_file),
        )
        .route("/gateway/status", get(gateway_status))
        .route("/gateway/start", post(gateway_start))
        .route("/gateway/stop", post(gateway_stop))
        .route("/gateway/usage", get(gateway_usage))
        .route("/stats/dashboard", get(dashboard_stats))
        .route("/worker-runs", get(list_worker_runs))
        .route("/worker-runs/{run_id}", get(get_worker_run))
        .route("/graphrag/state", get(graphrag_state_retired))
        .route("/graphrag/query", post(graphrag_query_retired))
        .route("/knowledge/graphrag/status", get(graphrag_state_retired))
        .route("/knowledge/graphrag/init", post(graphrag_action_retired))
        .route(
            "/knowledge/graphrag/prompt-tune",
            post(graphrag_action_retired),
        )
        .route("/knowledge/graphrag/index", post(graphrag_action_retired))
        .route("/knowledge/graphrag/query", post(graphrag_query_retired))
        .route("/executions", get(list_executions).post(submit_execution))
        .route("/executions/{execution_id}", get(get_execution))
        .route("/executions/{execution_id}/wait", post(wait_execution))
        .route("/executions/{execution_id}/cancel", post(cancel_execution))
        .route("/intake/analyze", post(intake::analyze_intake))
        .route("/intake/async", post(intake::async_intake))
        .route(
            "/intake/create-project",
            post(intake::create_intake_project),
        )
        .route("/intake/start", post(intake::start_intake))
        .route("/uploads", post(uploads::upload_artifact))
        .route("/tool-catalog", get(list_tool_catalog))
        .route("/tool-catalog/status", get(tool_catalog_status))
        .route("/tool-catalog/refresh", post(refresh_tool_catalog))
        .route("/tool-catalog/installations", get(list_tool_installations))
        .route(
            "/tool-catalog/installations/{job_id}",
            get(get_tool_installation),
        )
        .route("/tool-catalog/recommendations", get(recommend_tool_catalog))
        .route("/tool-catalog/search", post(search_tool_catalog))
        .route(
            "/tool-catalog/{tool_id}/configure",
            post(configure_tool_catalog),
        )
        .route(
            "/tool-catalog/{tool_id}/install",
            post(install_tool_catalog),
        )
        .route("/tool-catalog/{tool_id}/test", post(test_tool_catalog))
        .route("/intelligence/sources", get(list_intel_sources))
        .route("/intelligence/query", post(run_intel_query))
        .route("/intelligence/entities", get(list_intel_entities))
        .route(
            "/intelligence/entities/{entity_id}/promote",
            post(promote_intel_entity),
        )
        .route("/intelligence/relations", get(list_intel_relations))
        .route("/intelligence/raw-records", get(list_intel_raw_records))
        .route("/runtime-settings", get(list_runtime_settings))
        .route(
            "/runtime-settings/{*key}",
            get(get_runtime_setting)
                .put(upsert_runtime_setting)
                .delete(delete_runtime_setting),
        )
        .route("/fingerprint-rule-packs", get(list_fingerprint_rule_packs))
        .route(
            "/fingerprint-rule-packs/{pack_id}",
            get(get_fingerprint_rule_pack),
        )
        .route("/modules", get(list_modules).post(create_module))
        .route("/modules/health", get(module_health))
        .route(
            "/modules/{module_id}",
            get(get_module).patch(update_module).delete(delete_module),
        )
        .route("/providers", get(list_providers).post(create_provider))
        .route("/providers/default", get(default_provider))
        .route("/providers/discover-models", post(discover_models))
        .route(
            "/providers/capabilities/models",
            get(list_model_capabilities).post(upsert_model_capability),
        )
        .route(
            "/providers/routes",
            get(list_provider_routes).post(create_provider_route),
        )
        .route(
            "/providers/routes/{route_id}",
            patch(update_provider_route).delete(delete_provider_route),
        )
        .route(
            "/providers/{provider_id}",
            get(get_provider)
                .patch(update_provider)
                .delete(delete_provider),
        )
        .route("/providers/{provider_id}/test", post(test_provider))
        .route("/projects", get(list_projects).post(create_project))
        .route(
            "/projects/{project_id}",
            get(get_project).delete(delete_project),
        )
        .route(
            "/projects/{project_id}/agent-narratives",
            get(list_agent_narratives).post(create_agent_narrative),
        )
        .route("/missions", get(list_missions).post(create_mission))
        .route("/missions/batch", post(batch_update_missions))
        .route(
            "/missions/{mission_id}",
            get(get_mission)
                .patch(update_mission)
                .delete(delete_mission),
        )
        .route(
            "/missions/{mission_id}/branches",
            get(list_mission_branches),
        )
        .route("/missions/{mission_id}/assets", get(list_mission_assets))
        .route(
            "/missions/{mission_id}/directives",
            get(list_mission_directives).post(apply_directive),
        )
        .route("/missions/{mission_id}/pause", post(pause_mission))
        .route("/missions/{mission_id}/resume", post(resume_mission))
        .route("/missions/{mission_id}/advise", post(mission_advise))
        .route(
            "/missions/{mission_id}/reassess",
            post(reassess_mission_completion),
        )
        .route("/missions/{mission_id}/signal", post(post_mission_signal))
        .route(
            "/missions/{mission_id}/interrupt",
            post(interrupt_mission_worker),
        )
        .route("/missions/{mission_id}/timeline", get(mission_timeline))
        .route("/missions/{mission_id}/graph", get(mission_graph))
        .route(
            "/missions/{mission_id}/coverage-graph",
            get(coverage_graph::mission_coverage_graph),
        )
        .route(
            "/missions/{mission_id}/exploration-graph",
            get(mission_exploration_graph),
        )
        .route(
            "/missions/{mission_id}/operation-log",
            get(mission_operation_log),
        )
        .route("/missions/{mission_id}/start", post(start_mission))
        .route("/branches/{branch_id}", get(get_branch))
        .route("/branches/{branch_id}/abandon", post(abandon_branch))
        .route("/branches/{branch_id}/reopen", post(reopen_branch))
        .route("/branches/{branch_id}/prioritize", post(prioritize_branch))
        .route("/decision-gates", get(list_decision_gates))
        .route("/decision-gates/{gate_id}", get(get_decision_gate))
        .route(
            "/decision-gates/{gate_id}/answer",
            post(answer_decision_gate),
        )
        .route(
            "/decision-gates/{gate_id}/cancel",
            post(cancel_decision_gate),
        )
        .route("/ws/missions/{mission_id}", get(streams::mission_ws))
        .route(
            "/projects/{project_id}/audit/events/stream",
            get(streams::stream_project_events),
        )
        .route("/projects/{project_id}/audit/runs", get(list_project_runs))
        .route("/projects/{project_id}/worker-usage", get(project_worker_usage))
        .route(
            "/projects/{project_id}/audit/runs/{run_id}/decision-gates",
            get(list_run_decision_gates),
        )
        .route(
            "/projects/{project_id}/audit/runs/{run_id}/resume",
            post(resume_audit_run),
        )
        .route("/projects/{project_id}/events", get(list_project_events))
        .route(
            "/projects/{project_id}/decision-gates",
            get(list_project_decision_gates),
        )
        .route(
            "/projects/{project_id}/termination-assessments",
            get(list_project_termination_assessments),
        )
        .route(
            "/projects/{project_id}/worker-leases",
            get(list_project_worker_leases),
        )
        .route(
            "/projects/{project_id}/reflector-reports",
            get(list_project_reflector_reports),
        )
        .route(
            "/projects/{project_id}/findings",
            get(list_project_findings).post(add_project_finding),
        )
        .route(
            "/projects/{project_id}/findings/tree",
            get(coverage_graph::project_finding_asset_tree),
        )
        .route(
            "/projects/{project_id}/findings/{finding_id}",
            patch(triage_project_finding),
        )
        .route("/projects/{project_id}/facts", post(add_project_fact))
        .route("/projects/{project_id}/intents", post(add_project_intent))
        .route("/projects/{project_id}/hints", post(add_project_hint))
        .route(
            "/projects/{project_id}/evidence",
            post(add_project_evidence),
        )
        .route(
            "/projects/{project_id}/observations",
            get(list_project_observations),
        )
        .route(
            "/projects/{project_id}/tool-invocations",
            get(list_project_tool_invocations),
        )
        .route("/projects/{project_id}/graph", get(project_graph))
        .route(
            "/projects/{project_id}/strategy-board/latest",
            get(latest_strategy_board),
        )
        .route(
            "/projects/{project_id}/strategy-board/snapshots",
            get(list_strategy_board_snapshots),
        )
        .route("/missions/{mission_id}/evidence", get(mission_evidence))
        .route("/missions/{mission_id}/canvas", get(mission_canvas))
        .route("/artifacts", get(list_artifacts).post(add_artifact))
        .route("/tool-invocations", get(list_tool_invocations_global))
        .route(
            "/knowledge/cards",
            get(list_knowledge_cards).post(add_knowledge_card),
        )
        .route("/knowledge/cards/search", post(search_knowledge_cards))
        .route("/knowledge/index-status", get(knowledge_index_status))
        .route("/knowledge/index-sync", post(knowledge_index_sync))
        .with_state(state)
        .layer(cors_layer())
}

const CORS_ORIGINS_ENV: &str = "LYNCEUS_CORS_ORIGINS";

/// 本机 frontend（Vite 开发服务器与 Tauri 桌面端）的默认跨域来源。
const DEFAULT_CORS_ORIGINS: &[&str] = &[
    "http://127.0.0.1:5173",
    "http://localhost:5173",
    "http://tauri.localhost",
    "tauri://localhost",
];

/// 构造 CORS 层。
///
/// frontend 与 API 部署在不同源（浏览器直连 `127.0.0.1:8000`），跨域请求
/// 必须先通过预检。默认只放行本机开发与 Tauri 桌面来源；需要暴露给
/// 其他前端来源时用 `LYNCEUS_CORS_ORIGINS`（逗号分隔的 Origin 列表）
/// 覆盖，设为 `*` 表示任意来源（仅限隔离环境）。
fn cors_layer() -> tower_http::cors::CorsLayer {
    use tower_http::cors::AllowOrigin;

    let configured = std::env::var(CORS_ORIGINS_ENV)
        .ok()
        .map(|raw| raw.trim().to_string())
        .filter(|raw| !raw.is_empty());
    let allow_origin = match configured.as_deref() {
        Some("*") => AllowOrigin::any(),
        Some(raw) => AllowOrigin::list(
            raw.split(',')
                .map(str::trim)
                .filter(|origin| !origin.is_empty())
                .filter_map(|origin| origin.parse::<HeaderValue>().ok())
                .collect::<Vec<_>>(),
        ),
        None => AllowOrigin::list(
            DEFAULT_CORS_ORIGINS
                .iter()
                .filter_map(|origin| origin.parse::<HeaderValue>().ok())
                .collect::<Vec<_>>(),
        ),
    };
    tower_http::cors::CorsLayer::new()
        .allow_origin(allow_origin)
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
        ])
        .allow_headers([CONTENT_TYPE])
        .max_age(std::time::Duration::from_secs(3600))
}

/// Handler 使用的错误包装器。
#[derive(Debug)]
struct ApiError(EngineError);

impl From<EngineError> for ApiError {
    fn from(value: EngineError) -> Self {
        Self(value)
    }
}

impl From<storage::StorageError> for ApiError {
    fn from(value: storage::StorageError) -> Self {
        Self(EngineError::Storage(value))
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match &self.0 {
            EngineError::ProjectNotFound(_)
            | EngineError::MissionNotFound(_)
            | EngineError::RunNotFound(_)
            | EngineError::BranchNotFound(_)
            | EngineError::DecisionGateNotFound(_)
            | EngineError::FindingNotFound(_)
            | EngineError::ProviderNotFound(_)
            | EngineError::FingerprintRulePackNotFound(_)
            | EngineError::ExecutionNotFound(_)
            | EngineError::ToolCatalogNotFound(_) => StatusCode::NOT_FOUND,
            EngineError::ExecutionBackendForbidden(_) => StatusCode::FORBIDDEN,
            EngineError::ProviderRuntimeUnavailable | EngineError::WorkerRuntimeUnavailable(_) => {
                StatusCode::SERVICE_UNAVAILABLE
            }
            EngineError::DecisionGateStateError(_) | EngineError::ExecutionConflict(_) => {
                StatusCode::CONFLICT
            }
            EngineError::Storage(_) | EngineError::ToolCatalogFailure(_) => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
            EngineError::SolverConfigError(_)
            | EngineError::ProviderConfigError(_)
            | EngineError::ModuleConfigError(_)
            | EngineError::BudgetConfigError(_)
            | EngineError::ReferenceValidationError(_)
            | EngineError::Value(_)
            | EngineError::RequestValidation(_)
            | EngineError::InvalidDecisionAnswerError(_) => StatusCode::UNPROCESSABLE_ENTITY,
        };
        let detail = match &self.0 {
            EngineError::RequestValidation(detail) => detail.clone(),
            EngineError::ProjectNotFound(raw) => Value::String(format!(
                "project not found: {}",
                raw.strip_prefix("unknown project: ").unwrap_or(raw)
            )),
            EngineError::MissionNotFound(raw) => Value::String(format!(
                "mission not found: {}",
                raw.strip_prefix("unknown mission: ").unwrap_or(raw)
            )),
            _ => Value::String(self.0.to_string()),
        };
        (status, Json(serde_json::json!({"detail": detail}))).into_response()
    }
}

#[derive(Debug, Serialize)]
struct HealthResponse {
    status: &'static str,
    version: &'static str,
    tools: Vec<String>,
    solvers: Vec<String>,
}

async fn health(State(state): State<ApiState>) -> Json<HealthResponse> {
    // Python `/health.tools` = `runtime.tools.names()`：注册表全量名单，
    // 不做本地可用性过滤（可用性走 manager 工具闸的独立判定）。
    let tools = engines::solvers::REGISTERED_TOOL_ADAPTERS
        .iter()
        .map(|name| (*name).to_string())
        .collect::<Vec<_>>();
    let mut solvers = state.manager.solvers().names();
    solvers.sort();
    Json(HealthResponse {
        status: "ok",
        version: state.version,
        tools,
        solvers,
    })
}

#[derive(Debug, Serialize)]
struct CapabilityView {
    id: String,
    name: String,
    domain: String,
    audit_domains: Vec<AuditDomain>,
    capability_type: String,
    source: String,
    status: String,
    available: bool,
    configured: bool,
    description: String,
    reason: Option<String>,
    last_error: Option<String>,
    related_module_id: Option<String>,
    related_provider_id: Option<String>,
    supported_branch_kinds: Vec<String>,
}

struct ImplementedCapabilitySpec {
    id: &'static str,
    name: &'static str,
    domain: AuditDomain,
    solver: &'static str,
    tools: &'static [&'static str],
    description: &'static str,
    branch_kinds: &'static [&'static str],
}

struct PlannedCapabilitySpec {
    id: &'static str,
    name: &'static str,
    domain: AuditDomain,
    status: &'static str,
    description: &'static str,
    branch_kinds: &'static [&'static str],
}

const IMPLEMENTED_CAPABILITIES: &[ImplementedCapabilitySpec] = &[
    ImplementedCapabilitySpec {
        id: "web_sast.semgrep",
        name: "Semgrep / Web SAST",
        domain: AuditDomain::WebSast,
        solver: "web_sast",
        tools: &["semgrep"],
        description: "Fast source security scanning through Semgrep-backed Web SAST.",
        branch_kinds: &[
            "source.source_sink",
            "source.dependency_config",
            "source.framework_route",
            "source.secret_exposure",
        ],
    },
    ImplementedCapabilitySpec {
        id: "web_dast.nuclei",
        name: "Nuclei / Web DAST",
        domain: AuditDomain::WebDast,
        solver: "web_dast",
        tools: &["nuclei"],
        description: "Template-based black-box web validation through Nuclei.",
        branch_kinds: &["url.input_validation", "url.known_exposure"],
    },
    ImplementedCapabilitySpec {
        id: "asset_recon",
        name: "Asset Recon",
        domain: AuditDomain::AssetRecon,
        solver: "asset_recon",
        tools: &["subfinder", "naabu", "httpx"],
        description: "External attack-surface discovery for root domains.",
        branch_kinds: &["mixed.classification", "mixed.initial_surface"],
    },
    ImplementedCapabilitySpec {
        id: "web_recon",
        name: "Web Recon",
        domain: AuditDomain::WebRecon,
        solver: "web_recon",
        tools: &[],
        description: "Endpoint, parameter, and JavaScript discovery with native page-hint fallback; katana/jsluice enhance when configured.",
        branch_kinds: &["url.surface_mapping", "url.auth_session"],
    },
    ImplementedCapabilitySpec {
        id: "binary_analysis",
        name: "Binary / IDA MCP",
        domain: AuditDomain::BinaryStatic,
        solver: "binary_analysis",
        tools: &[],
        description: "Read-only IDA MCP tool surface (metadata, functions, decompile, disasm, callgraph, data-flow traces) driven by the Agent Tool Harness from binary_analysis.ida_host/ida_port.",
        branch_kinds: &[
            "binary.parser_surface",
            "binary.dangerous_api",
            "binary.heap_stack",
            "binary.protocol_surface",
        ],
    },
    ImplementedCapabilitySpec {
        id: "traffic_intelligence",
        name: "Traffic Intelligence",
        domain: AuditDomain::TrafficIntelligence,
        solver: "traffic_intelligence",
        tools: &[],
        description: "HAR/Burp/Chrome traffic import and request fact extraction.",
        branch_kinds: &[
            "traffic.parameter_analysis",
            "traffic.auth_context",
            "traffic.interesting_endpoint",
            "traffic.replay_validation",
        ],
    },
    ImplementedCapabilitySpec {
        id: "web_validation",
        name: "Web Validators",
        domain: AuditDomain::WebValidation,
        solver: "web_validation",
        tools: &["dalfox", "crlfsuite"],
        description: "Specialist validators for XSS and CRLF.",
        branch_kinds: &["url.input_validation"],
    },
];

/// Static registry entry for the IDA MCP-backed binary analysis capability.
/// Runtime status is derived from the actual solver/MCP module configuration
/// in `list_capabilities`; this table only supplies identity metadata.
struct IdaMcpCapabilitySpec {
    id: &'static str,
    name: &'static str,
    domain: AuditDomain,
    branch_kinds: &'static [&'static str],
}

const IDA_MCP_CAPABILITY: IdaMcpCapabilitySpec = IdaMcpCapabilitySpec {
    id: "remote_mcp.ida",
    name: "Remote MCP / IDA MCP",
    domain: AuditDomain::BinaryStatic,
    branch_kinds: &["binary.parser_surface", "binary.dangerous_api"],
};

const PLANNED_CAPABILITIES: &[PlannedCapabilitySpec] = &[
    PlannedCapabilitySpec {
        id: "fuzzing",
        name: "Fuzzing",
        domain: AuditDomain::Fuzzing,
        status: "planned",
        description: "Coverage-guided fuzzing is planned and not implemented.",
        branch_kinds: &["binary.heap_stack", "binary.protocol_surface"],
    },
    PlannedCapabilitySpec {
        id: "supply_chain",
        name: "Supply Chain",
        domain: AuditDomain::SupplyChain,
        status: "planned",
        description: "SBOM, dependency, lockfile, and package ecosystem auditing are planned.",
        branch_kinds: &["source.dependency_config"],
    },
    PlannedCapabilitySpec {
        id: "cloud_native",
        name: "Cloud Native",
        domain: AuditDomain::CloudNative,
        status: "planned",
        description: "Kubernetes, container, IaC, and CI/CD configuration auditing are planned.",
        branch_kinds: &[],
    },
];

fn capability_module_matches(module: &ModuleConfig, spec: &ImplementedCapabilitySpec) -> bool {
    if !module.enabled || module.domain.as_str() != spec.domain.as_str() {
        return false;
    }
    if spec.tools.is_empty() {
        return true;
    }
    let mut names = module
        .tool_allowlist
        .iter()
        .map(|name| name.to_lowercase())
        .collect::<HashSet<_>>();
    names.insert(module.name.to_lowercase());
    if let Some(tool_name) = module.metadata.get("tool_name").and_then(Value::as_str) {
        names.insert(tool_name.to_lowercase());
    }
    names.extend(
        module
            .capability_map
            .iter()
            .map(|(_, name)| name.to_lowercase()),
    );
    spec.tools.iter().any(|tool| names.contains(*tool))
}

fn module_is_available(module: &ModuleConfig, available_tools: &HashSet<String>) -> bool {
    module.enabled
        && module
            .tool_allowlist
            .iter()
            .all(|tool| available_tools.contains(tool))
}

fn planned_capability(spec: &PlannedCapabilitySpec) -> CapabilityView {
    CapabilityView {
        id: spec.id.to_string(),
        name: spec.name.to_string(),
        domain: spec.domain.as_str().to_string(),
        audit_domains: vec![spec.domain],
        capability_type: "planned".to_string(),
        source: "config".to_string(),
        status: spec.status.to_string(),
        available: false,
        configured: false,
        description: spec.description.to_string(),
        reason: Some(
            "capability is planned or partial and not available for autonomous execution"
                .to_string(),
        ),
        last_error: None,
        related_module_id: None,
        related_provider_id: None,
        supported_branch_kinds: spec.branch_kinds.iter().map(ToString::to_string).collect(),
    }
}

fn replace_capability(items: &mut Vec<CapabilityView>, capability: CapabilityView) {
    if let Some(existing) = items.iter_mut().find(|item| item.id == capability.id) {
        *existing = capability;
    } else {
        items.push(capability);
    }
}

/// Python `/capabilities` 镜像：已注册 solver 与可用工具的诚实视图
/// （占位 fail-closed solver 不宣称 available）。
#[allow(clippy::too_many_lines)] // Python 同构逐能力推导，拆分会破坏对照性
async fn list_capabilities(
    State(state): State<ApiState>,
) -> Result<Json<Vec<CapabilityView>>, ApiError> {
    let available_tools = state.manager.available_tool_names().unwrap_or_default();
    let solver_names = state.manager.solvers().names();
    let modules = state.manager.repository().list_modules()?;
    let providers = state.manager.repository().list_providers()?;
    let mut capabilities = Vec::new();

    for spec in IMPLEMENTED_CAPABILITIES {
        let matching_modules = modules
            .iter()
            .filter(|module| capability_module_matches(module, spec))
            .collect::<Vec<_>>();
        let healthy = matching_modules
            .iter()
            .find(|module| module_is_available(module, &available_tools));
        let registered = solver_names.iter().any(|name| name == spec.solver);
        let (status, reason, related_module_id) = if let Some(module) = healthy {
            (
                "available",
                "healthy configured module is available",
                Some(module.id.as_str().to_string()),
            )
        } else if let Some(module) = matching_modules.first() {
            (
                "configured",
                "module is configured but health check is not ok",
                Some(module.id.as_str().to_string()),
            )
        } else if registered && spec.tools.is_empty() {
            (
                "available",
                "solver is registered and does not require an external local tool",
                None,
            )
        } else if registered {
            (
                "partial",
                "solver is registered, but no healthy executable/module config proves runtime availability",
                None,
            )
        } else {
            ("unavailable", "solver is not registered", None)
        };
        capabilities.push(CapabilityView {
            id: spec.id.to_string(),
            name: spec.name.to_string(),
            domain: spec.domain.as_str().to_string(),
            audit_domains: vec![spec.domain],
            capability_type: "solver".to_string(),
            source: "solver".to_string(),
            status: status.to_string(),
            available: status == "available",
            configured: registered || !matching_modules.is_empty(),
            description: spec.description.to_string(),
            reason: Some(reason.to_string()),
            last_error: (status == "configured")
                .then(|| "one or more allowlisted tools are unavailable".to_string()),
            related_module_id,
            related_provider_id: None,
            supported_branch_kinds: spec.branch_kinds.iter().map(ToString::to_string).collect(),
        });
    }

    for module in modules
        .iter()
        .filter(|module| module.module_type == ModuleType::McpRemote)
    {
        let available = module_is_available(module, &available_tools);
        let status = if !module.enabled {
            "unavailable"
        } else if available {
            "available"
        } else {
            "configured"
        };
        let audit_domain = AuditDomain::try_from(module.domain).ok();
        capabilities.push(CapabilityView {
            id: format!("remote_mcp.{}", module.id.as_str()),
            name: format!("Remote MCP / {}", module.name),
            domain: module.domain.as_str().to_string(),
            audit_domains: audit_domain.into_iter().collect(),
            capability_type: "remote_mcp".to_string(),
            source: "module".to_string(),
            status: status.to_string(),
            available: status == "available",
            configured: true,
            description: format!(
                "{} exposes {} mapped capability/capabilities over {:?}.",
                module.name,
                module.capability_map.len(),
                module.transport
            ),
            reason: Some(if available {
                "remote MCP module is available".to_string()
            } else {
                "remote MCP health was not checked".to_string()
            }),
            last_error: (!available).then(|| "remote MCP health was not checked".to_string()),
            related_module_id: Some(module.id.as_str().to_string()),
            related_provider_id: None,
            supported_branch_kinds: Vec::new(),
        });
    }

    let enabled_provider = providers.iter().find(|provider| provider.enabled);
    let mut provider_status = "unavailable";
    let mut provider_reason = "no enabled provider configured".to_string();
    let mut provider_last_error = None;
    let mut provider_id = providers
        .first()
        .map(|provider| provider.id.as_str().to_string());
    if let Some(provider) = enabled_provider {
        provider_status = "configured";
        provider_id = Some(provider.id.as_str().to_string());
        provider_reason = "provider runtime is not configured".to_string();
        if let Some(runtime) = state.manager.provider_runtime() {
            match runtime.health_check(provider.id.as_str()).await {
                Ok(result) if result.status.as_str() == "ok" => {
                    provider_status = "available";
                    provider_reason = if result.message.is_empty() {
                        "provider health check succeeded".to_string()
                    } else {
                        result.message
                    };
                }
                Ok(result) => {
                    provider_reason.clone_from(&result.message);
                    provider_last_error = Some(result.message);
                }
                Err(error) => {
                    provider_reason = error.to_string();
                    provider_last_error = Some(error.to_string());
                }
            }
        }
    }
    capabilities.push(CapabilityView {
        id: "provider_runtime".to_string(),
        name: "Provider Runtime".to_string(),
        domain: "provider".to_string(),
        audit_domains: Vec::new(),
        capability_type: "provider".to_string(),
        source: "provider".to_string(),
        status: provider_status.to_string(),
        available: provider_status == "available",
        configured: enabled_provider.is_some(),
        description: "Configured model/provider runtime for intake and Strategy Board sidecars."
            .to_string(),
        reason: Some(provider_reason),
        last_error: provider_last_error,
        related_module_id: None,
        related_provider_id: provider_id,
        supported_branch_kinds: Vec::new(),
    });

    let ida_candidates = modules
        .iter()
        .filter(|module| {
            module.module_type == ModuleType::McpRemote
                && matches!(module.domain.as_str(), "binary_static" | "binary_dynamic")
                && (module.name.to_lowercase().contains("ida")
                    || module
                        .endpoint_url
                        .as_deref()
                        .is_some_and(|url| url.to_lowercase().contains("ida"))
                    || module
                        .capability_map
                        .iter()
                        .any(|(_, value)| value.to_lowercase().contains("ida")))
        })
        .collect::<Vec<_>>();
    let ida_module = ida_candidates.first();
    let binary_registered = solver_names.iter().any(|name| name == "binary_analysis");
    let ida_healthy =
        ida_module.is_some_and(|module| module_is_available(module, &available_tools));
    let (ida_status, ida_available, ida_reason, ida_last_error) = if ida_healthy {
        (
            "available",
            true,
            "IDA MCP module is healthy and consumed by the binary_analysis harness".to_string(),
            None,
        )
    } else if binary_registered {
        (
            "configured",
            false,
            "binary_analysis solver is registered; configure a healthy IDA MCP module or set \
             binary_analysis.ida_host/ida_port in the mission config"
                .to_string(),
            Some("no healthy IDA MCP module is configured".to_string()),
        )
    } else {
        (
            "unavailable",
            false,
            "binary_analysis solver is not registered".to_string(),
            None,
        )
    };
    replace_capability(
        &mut capabilities,
        CapabilityView {
            id: IDA_MCP_CAPABILITY.id.to_string(),
            name: IDA_MCP_CAPABILITY.name.to_string(),
            domain: IDA_MCP_CAPABILITY.domain.as_str().to_string(),
            audit_domains: vec![IDA_MCP_CAPABILITY.domain],
            capability_type: "remote_mcp".to_string(),
            source: "module".to_string(),
            status: ida_status.to_string(),
            available: ida_available,
            configured: ida_module.is_some() || binary_registered,
            description: "IDA MCP pass-through tool surface driven by the binary_analysis Agent \
                          Tool Harness from binary_analysis.ida_host/ida_port."
                .to_string(),
            reason: Some(ida_reason),
            last_error: ida_last_error,
            related_module_id: ida_module.map(|module| module.id.as_str().to_string()),
            related_provider_id: None,
            supported_branch_kinds: IDA_MCP_CAPABILITY
                .branch_kinds
                .iter()
                .map(ToString::to_string)
                .collect(),
        },
    );
    capabilities.extend(PLANNED_CAPABILITIES.iter().map(planned_capability));
    Ok(Json(capabilities))
}

/// README-compatible engine pool entry.
///
/// The Python implementation derives this view from the curated tool catalog.
/// Rust currently owns the solver registry, so the endpoint exposes each
/// registered solver with an honest `missing` status until a concrete external
/// adapter is configured. This keeps the UI useful without claiming that the
/// fail-closed baseline can execute a real scan.
#[derive(Debug, Serialize)]
struct EnginePoolEntry {
    id: String,
    name: String,
    domain: String,
    category: String,
    status: String,
    health: String,
    capabilities: Vec<String>,
    input_contract: Option<String>,
    output_contract: Option<String>,
    agent_usage_hint: Option<String>,
    install_suggestion: Option<String>,
}

async fn list_engines(
    State(state): State<ApiState>,
) -> Result<Json<Vec<EnginePoolEntry>>, ApiError> {
    let configured_tools = state.manager.available_tool_names().unwrap_or_default();
    let mut entries = Vec::new();
    for name in state.manager.solvers().names() {
        let Some(solver) = state.manager.solvers().get(&name) else {
            continue;
        };
        let mut domains = solver.audit_domains().into_iter().collect::<Vec<_>>();
        domains.sort_unstable_by_key(|domain| domain.as_str());
        let domain = domains
            .first()
            .map_or_else(|| "unknown".to_string(), |item| item.as_str().to_string());
        let installed = configured_tools.contains(&name);
        let status = if installed { "installed" } else { "missing" };
        entries.push(EnginePoolEntry {
            id: name.clone(),
            name: name.clone(),
            domain: domain.clone(),
            category: domain,
            status: status.to_string(),
            health: "unknown".to_string(),
            capabilities: vec![name.clone()],
            input_contract: Some("SolverContext".to_string()),
            output_contract: Some("SolverResult".to_string()),
            agent_usage_hint: Some(solver.description().to_string()),
            install_suggestion: (!installed).then(|| {
                format!("Configure a concrete adapter for the `{name}` solver before execution")
            }),
        });
    }
    entries.sort_unstable_by(|left, right| left.id.cmp(&right.id));
    Ok(Json(entries))
}

// ---------------------------------------------------------------------------
// External Worker Runtimes（观测 + Profile 绑定 + 会话审计）
// ---------------------------------------------------------------------------

fn worker_registry_or_unavailable() -> Result<Arc<engines::worker::WorkerRegistry>, ApiError> {
    engines::worker::global_registry().ok_or_else(|| {
        ApiError(EngineError::WorkerRuntimeUnavailable(
            "composition root did not attach a worker registry".to_string(),
        ))
    })
}

/// GET /worker-runtimes：外部 Worker Runtime 探测快照（TTL 缓存）。
async fn list_worker_runtimes() -> Result<Json<Vec<models::WorkerProbe>>, ApiError> {
    let registry = worker_registry_or_unavailable()?;
    Ok(Json(registry.probes().await))
}

/// POST /worker-runtimes/refresh：强制重新探测（真实 `--version` 进程）。
async fn refresh_worker_runtimes() -> Result<Json<Vec<models::WorkerProbe>>, ApiError> {
    let registry = worker_registry_or_unavailable()?;
    Ok(Json(registry.refresh_probes().await))
}

/// GET /worker-command-policy 响应：全局默认命令拦截规则。
#[derive(Debug, Serialize)]
pub struct WorkerCommandPolicyView {
    /// 禁则命令前缀（命令起始匹配；大小写不敏感）。
    pub denied_prefixes: Vec<String>,
    /// 提示词层禁则（SQL / DELETE / 批量清空等无法前缀判定的形态）。
    pub prompt_only_rules: Vec<String>,
}

/// GET /worker-command-policy：全局默认命令拦截规则（禁则前缀 + 提示词层），
/// 供工具审计页「拦截规则」面板展示；取内置种子策略，不随任务变化。
async fn get_worker_command_policy() -> Result<Json<WorkerCommandPolicyView>, ApiError> {
    let seed = engines::worker::CommandPolicy::seed();
    Ok(Json(WorkerCommandPolicyView {
        denied_prefixes: seed.denied_prefixes,
        prompt_only_rules: engines::worker::CommandPolicy::prompt_only_rules()
            .iter()
            .map(|rule| (*rule).to_string())
            .collect(),
    }))
}

/// GET /worker-runtimes/{runtime_id}：单个 runtime 的探测结论。
async fn get_worker_runtime(
    axum::extract::Path(runtime_id): axum::extract::Path<String>,
) -> Result<Json<models::WorkerProbe>, ApiError> {
    let registry = worker_registry_or_unavailable()?;
    let probes = registry.probes().await;
    probes
        .into_iter()
        .find(|probe| probe.runtime.as_str() == runtime_id)
        .map(Json)
        .ok_or_else(|| {
            ApiError(EngineError::ToolCatalogNotFound(format!(
                "worker runtime '{runtime_id}' is not a registered adapter"
            )))
        })
}

/// POST /worker-runtime-profiles 的请求体。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UpsertWorkerRuntimeProfileRequest {
    /// 绑定的 runtime 类型（wire 值）。
    runtime_type: String,
    /// 绑定的 Connection（ProviderConfig id）。
    connection_id: String,
    /// 模型覆盖（可选）。
    #[serde(default)]
    model_override: Option<String>,
    /// Agent 预设绑定（WP4；Some("") 清除，None 保持不变）。
    #[serde(default)]
    agent_preset: Option<String>,
    /// 执行位置（默认 local）。
    #[serde(default)]
    execution_environment: Option<models::WorkerExecutionEnvironment>,
    /// 最大并发会话数（默认 [`DEFAULT_WORKER_MAX_CONCURRENCY`]）。
    #[serde(default)]
    max_concurrency: Option<u32>,
    /// 单次调用超时秒数（默认 900）。
    #[serde(default)]
    timeout_seconds: Option<u64>,
    /// 是否启用（默认 true；启用时同 runtime 的其他 Profile 自动停用）。
    #[serde(default)]
    enabled: Option<bool>,
}

/// Worker runtime profile 的默认最大并发会话数。
///
/// 曾是 1（留空即串行）——用户实测"感觉 Worker 一个一个跑"的根因：
/// registry 按 (runtime, profile) 建信号量，1 会把分支波次的并行全卡死。
/// 外部 CLI worker（codex/claude/pi/dsh）在开发机上同时跑几个是常态，
/// 4 是保守起点；真正的上限仍由用户在该 profile 上显式设置。
const DEFAULT_WORKER_MAX_CONCURRENCY: u32 = 4;

/// GET /worker-runtime-profiles：列出 runtime → Connection 绑定。
async fn list_worker_runtime_profiles(
    State(state): State<ApiState>,
) -> Result<Json<Vec<models::WorkerRuntimeProfile>>, ApiError> {
    let profiles = state
        .manager
        .repository()
        .list_worker_runtime_profiles(None)?;
    Ok(Json(profiles))
}

/// POST /worker-runtime-profiles：创建/更新绑定（同 runtime 仅一个启用）。
async fn upsert_worker_runtime_profile(
    State(state): State<ApiState>,
    axum::Json(request): axum::Json<UpsertWorkerRuntimeProfileRequest>,
) -> Result<Json<models::WorkerRuntimeProfile>, ApiError> {
    let repository = state.manager.repository();
    let runtime_type = request
        .runtime_type
        .parse::<WorkerRuntimeTypeParse>()
        .map_err(|_| {
            ApiError(EngineError::Value(format!(
                "unknown worker runtime type '{}'",
                request.runtime_type
            )))
        })?
        .0;
    let profile = models::WorkerRuntimeProfile::new(
        runtime_type,
        request.connection_id,
        request
            .execution_environment
            .unwrap_or(models::WorkerExecutionEnvironment::Local),
        request
            .max_concurrency
            .unwrap_or(DEFAULT_WORKER_MAX_CONCURRENCY)
            .max(1),
        request.timeout_seconds.unwrap_or(900).max(1),
    )
    .map_err(|error| ApiError(EngineError::Value(error.to_string())))?;
    let mut profile = profile;
    profile.enabled = request.enabled.unwrap_or(true);
    if let Some(model_override) = request.model_override.as_deref() {
        if model_override.trim().is_empty() {
            return Err(ApiError(EngineError::Value(
                "model_override must be non-empty".to_string(),
            )));
        }
        profile.model_override = Some(model_override.to_string());
    }
    // WP4：Agent 预设绑定 → runtime_options.agent_preset（dispatch 解析
    // 优先级第二层；Some("") 清除绑定）。
    if let Some(agent_preset) = request.agent_preset.as_deref() {
        let key = agent_preset.trim();
        if !key.is_empty() {
            if repository.get_agent_preset(key)?.is_none() {
                return Err(ApiError(EngineError::Value(format!(
                    "agent preset '{key}' not found"
                ))));
            }
            profile
                .runtime_options
                .insert("agent_preset".to_string(), serde_json::Value::String(key.to_string()));
        }
    }
    // "每个类型至多一个启用 Profile"：新建启用绑定时停用同类型旧绑定。
    if profile.enabled {
        for existing in repository.list_worker_runtime_profiles(Some(runtime_type))? {
            if !existing.enabled {
                continue;
            }
            let mut disabled = existing;
            disabled.enabled = false;
            disabled.updated_at = models::utcnow();
            repository.upsert_worker_runtime_profile(&disabled)?;
        }
    }
    repository.upsert_worker_runtime_profile(&profile)?;
    Ok(Json(profile))
}

/// DELETE /worker-runtime-profiles/{profile_id}。
async fn delete_worker_runtime_profile(
    State(state): State<ApiState>,
    axum::extract::Path(profile_id): axum::extract::Path<String>,
) -> Result<StatusCode, ApiError> {
    let deleted = state
        .manager
        .repository()
        .delete_worker_runtime_profile(&profile_id)?;
    if deleted {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError(EngineError::ToolCatalogNotFound(format!(
            "worker runtime profile '{profile_id}' not found"
        ))))
    }
}

/// GET /worker-runs：外部 worker 会话审计（`project_id` / `limit` 过滤）。
#[derive(Debug, Deserialize)]
struct ListWorkerRunsQuery {
    project_id: Option<String>,
    run_id: Option<String>,
    task_id: Option<String>,
    limit: Option<usize>,
}

/// GET /gateway/status —— LiteLLM Gateway sidecar 当前状态。
async fn gateway_status(
    State(state): State<ApiState>,
) -> Json<engines::worker::gateway::GatewayStatus> {
    Json(state.gateway.status())
}

/// POST /gateway/start —— 生成 config 并拉起 sidecar（幂等）。
async fn gateway_start(
    State(state): State<ApiState>,
) -> Result<Json<engines::worker::gateway::GatewayStatus>, ApiError> {
    let repository = Arc::clone(state.manager.repository());
    Ok(state
        .gateway
        .start_with_repository(repository)
        .await
        .map_err(EngineError::Value)
        .map(Json)?)
}

/// POST /gateway/stop —— 树杀 sidecar。
async fn gateway_stop(
    State(state): State<ApiState>,
) -> Json<engines::worker::gateway::GatewayStatus> {
    Json(state.gateway.stop())
}

/// 启动播种：内置预设 key 缺失才插入（first-insert-only）。用户对内置
/// 预设的编辑（含停用）不会被重启覆盖。
fn seed_agent_presets(manager: &AuditManager) {
    let repository = manager.repository();
    for preset in models::agent_preset::builtin_seed_presets(models::common::utcnow()) {
        let existing = repository.get_agent_preset(&preset.key).ok().flatten();
        if existing.is_none() {
            // 播种失败仅降级为内置文件默认（运行时解析有回落），不阻断启动。
            let _ = repository.upsert_agent_preset(&preset);
        }
    }
    // 内置模板升级同步：DB 行仍是上一个内置版本（逐字节相同 = 用户从未
    // 编辑）时，用新内置模板覆盖该行；差一个字节就是用户的编辑，不动。
    // 没有这一步，first-insert-only 会让模板升级永远到不了已播种的库。
    for (key, legacy) in models::agent_preset::builtin_legacy_templates() {
        let Some(existing) = repository.get_agent_preset(key).ok().flatten() else {
            continue;
        };
        // 容忍尾部换行差异：历史种子可能来自无 trailing newline 的文件
        // 版本， TrimEnd 后比对内容本身。
        if existing.instruction_template.trim_end() != legacy.trim_end() {
            continue;
        }
        let Some(fresh) = models::agent_preset::builtin_seed_presets(models::common::utcnow())
            .into_iter()
            .find(|preset| preset.key == *key)
        else {
            continue;
        };
        let mut updated = existing.clone();
        updated.instruction_template = fresh.instruction_template.clone();
        updated.variables = fresh.variables.clone();
        updated.updated_at = models::common::utcnow();
        let _ = repository.upsert_agent_preset(&updated);
    }
}

/// 分工提示词覆盖表刷新：策略板维护 / 元认知发散两个固定调用点按 DB
/// 启用预设覆盖，停用/缺失回落内置文件默认。
fn refresh_agent_prompt_overrides(manager: &AuditManager) {
    let repository = manager.repository();
    for key in [
        models::agent_preset::PRESET_STRATEGY_BOARD_MAINTAINER,
        models::agent_preset::PRESET_METACOGNITION_DIVERGENCE,
    ] {
        match repository
            .get_agent_preset(key)
            .ok()
            .flatten()
            .filter(|preset| preset.enabled)
        {
            Some(preset) => agents::prompts::set_override(key, preset.instruction_template),
            None => agents::prompts::clear_override(key),
        }
    }
}

/// 预设变更后刷新覆盖表（仅分工两角色受全局覆盖影响）。
fn refresh_agent_prompt_override_for(state: &ApiState, key: &str) {
    if matches!(
        key,
        models::agent_preset::PRESET_STRATEGY_BOARD_MAINTAINER
            | models::agent_preset::PRESET_METACOGNITION_DIVERGENCE
    ) {
        refresh_agent_prompt_overrides(&state.manager);
    }
}

/// GET /agent-presets —— 全量列出（内置 + 自定义）。
async fn list_agent_presets(
    State(state): State<ApiState>,
) -> Result<Json<Vec<models::AgentPreset>>, ApiError> {
    Ok(Json(state.manager.repository().list_agent_presets(None)?))
}

/// GET /agent-presets/{key}。
async fn get_agent_preset(
    State(state): State<ApiState>,
    axum::extract::Path(key): axum::extract::Path<String>,
) -> Result<Json<models::AgentPreset>, ApiError> {
    state
        .manager
        .repository()
        .get_agent_preset(&key)?
        .map(Json)
        .ok_or_else(|| {
            ApiError(EngineError::Value(format!("agent preset '{key}' not found")))
        })
}

/// POST /agent-presets 请求体（创建自定义预设）。
#[derive(Debug, Deserialize)]
struct CreateAgentPresetRequest {
    /// 稳定 key（自定义预设；不得与内置 key 冲突）。
    key: String,
    name: String,
    #[serde(default)]
    description: Option<String>,
    instruction_template: String,
    #[serde(default)]
    wrapup_template: Option<String>,
    #[serde(default)]
    model_alias: Option<String>,
    #[serde(default)]
    max_turns: Option<u32>,
    #[serde(default)]
    skills: Vec<String>,
    #[serde(default)]
    tools: Vec<String>,
}

/// POST /agent-presets —— 创建自定义预设（v1 版本入库）。
async fn create_agent_preset(
    State(state): State<ApiState>,
    axum::Json(request): axum::Json<CreateAgentPresetRequest>,
) -> Result<Json<models::AgentPreset>, ApiError> {
    let key = request.key.trim().to_string();
    if key.is_empty() {
        return Err(ApiError(EngineError::Value(
            "agent preset key must be non-empty".to_string(),
        )));
    }
    if models::agent_preset::BUILTIN_KEYS.contains(&key.as_str()) {
        return Err(ApiError(EngineError::Value(format!(
            "agent preset key '{key}' is reserved by a builtin preset"
        ))));
    }
    if request.instruction_template.trim().is_empty() {
        return Err(ApiError(EngineError::Value(
            "instruction_template must be non-empty".to_string(),
        )));
    }
    let repository = state.manager.repository();
    if repository.get_agent_preset(&key)?.is_some() {
        return Err(ApiError(EngineError::Value(format!(
            "agent preset '{key}' already exists"
        ))));
    }
    let now = models::common::utcnow();
    let mut preset = models::AgentPreset::new_v1(
        key,
        request.name,
        request.description,
        request.instruction_template,
        now,
    );
    preset.builtin = false;
    preset.wrapup_template = request.wrapup_template;
    preset.model_alias = request.model_alias;
    preset.max_turns = request.max_turns;
    preset.skills = request.skills;
    preset.tools = request.tools;
    preset.variables = models::agent_preset::extract_variables(&preset.instruction_template);
    repository.upsert_agent_preset(&preset)?;
    Ok(Json(preset))
}

/// PATCH /agent-presets/{key} 请求体（部分更新；模板变更追加版本）。
#[derive(Debug, Deserialize)]
struct UpdateAgentPresetRequest {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<Option<String>>,
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    model_alias: Option<Option<String>>,
    #[serde(default)]
    max_turns: Option<Option<u32>>,
    #[serde(default)]
    instruction_template: Option<String>,
    #[serde(default)]
    wrapup_template: Option<Option<String>>,
    #[serde(default)]
    skills: Option<Vec<String>>,
    #[serde(default)]
    tools: Option<Vec<String>>,
}

/// PATCH /agent-presets/{key} —— 编辑预设（内置可编辑不可删除；分工三角色
/// 同步刷新全局覆盖表）。
async fn update_agent_preset(
    State(state): State<ApiState>,
    axum::extract::Path(key): axum::extract::Path<String>,
    axum::Json(request): axum::Json<UpdateAgentPresetRequest>,
) -> Result<Json<models::AgentPreset>, ApiError> {
    let repository = state.manager.repository();
    let mut preset = repository
        .get_agent_preset(&key)?
        .ok_or_else(|| ApiError(EngineError::Value(format!("agent preset '{key}' not found"))))?;
    if let Some(name) = request.name {
        if name.trim().is_empty() {
            return Err(ApiError(EngineError::Value(
                "name must be non-empty".to_string(),
            )));
        }
        preset.name = name;
    }
    if let Some(description) = request.description {
        preset.description = description;
    }
    if let Some(enabled) = request.enabled {
        preset.enabled = enabled;
    }
    if let Some(model_alias) = request.model_alias {
        preset.model_alias = model_alias;
    }
    if let Some(max_turns) = request.max_turns {
        preset.max_turns = max_turns;
    }
    if let Some(wrapup) = request.wrapup_template {
        preset.wrapup_template = wrapup;
    }
    if let Some(skills) = request.skills {
        preset.skills = skills;
    }
    if let Some(tools) = request.tools {
        preset.tools = tools;
    }
    if let Some(template) = request.instruction_template {
        if template.trim().is_empty() {
            return Err(ApiError(EngineError::Value(
                "instruction_template must be non-empty".to_string(),
            )));
        }
        if template != preset.instruction_template {
            preset.instruction_template = template;
            preset.variables =
                models::agent_preset::extract_variables(&preset.instruction_template);
        }
    }
    let now = models::common::utcnow();
    preset.updated_at = now;
    // 模板即权威：改动直接落在 instruction_template 上，不留历史副本。
    repository.upsert_agent_preset(&preset)?;
    refresh_agent_prompt_override_for(&state, &key);
    Ok(Json(preset))
}

/// DELETE /agent-presets/{key} —— 删除自定义预设（内置拒绝）。
async fn delete_agent_preset(
    State(state): State<ApiState>,
    axum::extract::Path(key): axum::extract::Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if models::agent_preset::BUILTIN_KEYS.contains(&key.as_str()) {
        return Err(ApiError(EngineError::Value(
            "builtin agent presets cannot be deleted (edit or disable instead)".to_string(),
        )));
    }
    let deleted = state.manager.repository().delete_agent_preset(&key)?;
    if !deleted {
        return Err(ApiError(EngineError::Value(format!(
            "agent preset '{key}' not found"
        ))));
    }
    refresh_agent_prompt_override_for(&state, &key);
    Ok(Json(serde_json::json!({ "deleted": key })))
}

/// POST /agent-presets/{key}/preview 请求体（示例变量值）。
#[derive(Debug, Deserialize)]
struct PreviewAgentPresetRequest {
    #[serde(default)]
    variables: serde_json::Map<String, serde_json::Value>,
}

/// POST /agent-presets/{key}/preview —— 用示例值渲染模板（变量缺失按空
/// 串渲染，与运行时渲染器行为一致）。
async fn preview_agent_preset(
    State(state): State<ApiState>,
    axum::extract::Path(key): axum::extract::Path<String>,
    axum::Json(request): axum::Json<PreviewAgentPresetRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let preset = state
        .manager
        .repository()
        .get_agent_preset(&key)?
        .ok_or_else(|| ApiError(EngineError::Value(format!("agent preset '{key}' not found"))))?;
    let rendered = models::agent_preset::render_template(
        &preset.instruction_template,
        &request.variables,
    );
    Ok(Json(serde_json::json!({
        "key": preset.key,
        "variables": preset.variables,
        "rendered": rendered,
    })))
}

/// 组合根注入的 Skill 目录管理器句柄。
fn skill_manager() -> engines::skills::SkillManager {
    engines::skills::SkillManager::from_env()
}

/// GET /skills —— 全量列出（含每项的模块声明）。
async fn list_skills(
    State(_state): State<ApiState>,
) -> Result<Json<Vec<engines::skills::SkillMeta>>, ApiError> {
    skill_manager()
        .list()
        .map(Json)
        .map_err(|error| ApiError(EngineError::Value(error)))
}

/// GET /skills/{name} —— 元数据 + 手册正文。
async fn get_skill(
    State(_state): State<ApiState>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let (meta, body) =
        skill_manager()
            .load(&name)
            .map_err(|error| ApiError(EngineError::Value(error)))?;
    Ok(Json(serde_json::json!({ "meta": meta, "manual": body })))
}

/// POST /skills 请求体。
#[derive(Debug, Deserialize)]
struct CreateSkillRequest {
    name: String,
    description: String,
    #[serde(default)]
    modules: Vec<String>,
    #[serde(default)]
    license: Option<String>,
    #[serde(default)]
    compatibility: Option<String>,
    #[serde(default)]
    body: String,
}

/// POST /skills —— 新建 skill（SKILL.md 由字段拼装）。
async fn create_skill(
    State(_state): State<ApiState>,
    axum::Json(request): axum::Json<CreateSkillRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    skill_manager()
        .create(
            request.name.trim(),
            request.description.trim(),
            &request.modules,
            request.license.as_deref(),
            request.compatibility.as_deref(),
            &request.body,
        )
        .map_err(|error| ApiError(EngineError::Value(error)))?;
    Ok(Json(serde_json::json!({ "created": request.name.trim() })))
}

/// POST /skills/upload —— zip 导入（skill 根 = 最浅 SKILL.md 所在目录）。
async fn upload_skill_zip(
    State(_state): State<ApiState>,
    mut upload: axum::extract::Multipart,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mut bytes: Option<Vec<u8>> = None;
    while let Some(field) = upload
        .next_field()
        .await
        .map_err(|error| ApiError(EngineError::Value(format!("multipart error: {error}"))))?
    {
        if field.name() == Some("file") {
            let data = field
                .bytes()
                .await
                .map_err(|error| ApiError(EngineError::Value(format!("read upload: {error}"))))?;
            bytes = Some(data.to_vec());
        }
    }
    let bytes = bytes
        .ok_or_else(|| ApiError(EngineError::Value("missing 'file' field".to_string())))?;
    if bytes.len() > 32 * 1024 * 1024 {
        return Err(ApiError(EngineError::Value(
            "skill zip exceeds 32 MiB".to_string(),
        )));
    }
    let name = skill_manager()
        .import_zip(&bytes)
        .map_err(|error| ApiError(EngineError::Value(error)))?;
    Ok(Json(serde_json::json!({ "imported": name })))
}

/// DELETE /skills/{name}。
async fn delete_skill(
    State(_state): State<ApiState>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    skill_manager()
        .delete(&name)
        .map_err(|error| ApiError(EngineError::Value(error)))?;
    Ok(Json(serde_json::json!({ "deleted": name })))
}

/// GET /skills/missing —— 缺口清单（found=0 按被点名次数排序）。
///
/// 已补进技能库的名字不再是缺口：用当前 `SkillManager` 名录剔掉历史 miss，
/// 否则补了手册，那条旧记录仍一直挂在缺口清单上（模型此刻已能 skill_load）。
async fn skill_missing_report(
    State(state): State<ApiState>,
) -> Result<Json<Vec<models::skill::SkillMissingEntry>>, ApiError> {
    let report = state.manager.repository().skill_missing_report()?;
    let present: std::collections::HashSet<String> = skill_manager()
        .list()
        .map(|skills| {
            skills
                .into_iter()
                .map(|meta| meta.name)
                .collect::<std::collections::HashSet<_>>()
        })
        .unwrap_or_default();
    let live = report
        .into_iter()
        .filter(|entry| !present.contains(&entry.skill))
        .collect::<Vec<_>>();
    Ok(Json(live))
}

/// GET /skills/{name}/usage —— 单个 skill 的调用台账。
async fn get_skill_usage(
    State(state): State<ApiState>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> Result<Json<Vec<models::skill::SkillUsageRow>>, ApiError> {
    Ok(Json(
        state
            .manager
            .repository()
            .list_skill_usage(Some(&name), 200)?,
    ))
}

/// GET /skills/{name}/files —— 相对文件树。
async fn list_skill_files(
    State(_state): State<ApiState>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let files = skill_manager()
        .file_tree(&name)
        .map_err(|error| ApiError(EngineError::Value(error)))?;
    Ok(Json(serde_json::json!({ "files": files })))
}

/// GET /skills/{name}/file?path= —— 读单个文件。
#[derive(Debug, Deserialize)]
struct SkillFileQuery {
    path: String,
}

async fn read_skill_file(
    State(_state): State<ApiState>,
    axum::extract::Path(name): axum::extract::Path<String>,
    axum::extract::Query(query): axum::extract::Query<SkillFileQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let content = skill_manager()
        .read_file(&name, &query.path)
        .map_err(|error| ApiError(EngineError::Value(error)))?;
    Ok(Json(serde_json::json!({ "path": query.path, "content": content })))
}

/// PUT /skills/{name}/file —— 写单个文件（编辑器保存）。
async fn write_skill_file(
    State(_state): State<ApiState>,
    axum::extract::Path(name): axum::extract::Path<String>,
    axum::extract::Query(query): axum::extract::Query<SkillFileQuery>,
    axum::Json(body): axum::Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let content = body["content"].as_str().ok_or_else(|| {
        ApiError(EngineError::Value("missing 'content' string".to_string()))
    })?;
    skill_manager()
        .write_file(&name, &query.path, content)
        .map_err(|error| ApiError(EngineError::Value(error)))?;
    Ok(Json(serde_json::json!({ "written": query.path })))
}

/// GET /gateway/usage 查询参数。
#[derive(Debug, Deserialize)]
struct GatewayUsageQuery {
    /// 统计窗口天数（1..=3650，缺省 30）。
    days: Option<i64>,
    /// daily 聚合的分组维度（`runtime` / `model`；缺省 = 总计单系列）。
    group_by: Option<String>,
}

/// GET /gateway/usage —— 网关 token 用量多维报表（LiteLLM 统计页数据源；
/// 从 worker_usage 聚合：总量 + 按日 + runtime×模型切片 + 可选分组每日）。
async fn gateway_usage(
    State(state): State<ApiState>,
    Query(query): Query<GatewayUsageQuery>,
) -> Result<Json<models::WorkerUsageBreakdown>, ApiError> {
    let days = query.days.unwrap_or(30).clamp(1, 3650);
    let group_by = query
        .group_by
        .as_deref()
        .map(|raw| match raw {
            "runtime" => Ok(models::WorkerUsageDimension::Runtime),
            "model" => Ok(models::WorkerUsageDimension::Model),
            other => Err(EngineError::Value(format!(
                "invalid group_by '{other}' (expected 'runtime' or 'model')"
            ))),
        })
        .transpose()?;
    Ok(Json(
        state
            .manager
            .repository()
            .sum_worker_usage_breakdown(None, Some(days), group_by)?,
    ))
}

/// GET /stats/dashboard —— 仪表盘首行五卡一次性聚合
/// （活跃任务 / 确认发现 / 资产节点 / 工具调用 / Token 用量）。
async fn dashboard_stats(
    State(state): State<ApiState>,
) -> Result<Json<models::DashboardStats>, ApiError> {
    Ok(Json(state.manager.repository().dashboard_stats()?))
}

async fn list_worker_runs(
    State(state): State<ApiState>,
    axum::extract::Query(query): axum::extract::Query<ListWorkerRunsQuery>,
) -> Result<Json<Vec<models::WorkerRun>>, ApiError> {
    let runs = state.manager.repository().list_worker_runs(
        query.project_id.as_deref(),
        query.run_id.as_deref(),
        query.task_id.as_deref(),
        query.limit.unwrap_or(50).min(200),
    )?;
    Ok(Json(runs))
}

/// GET /worker-runs/{run_id}：会话详情 + 调用审计。
#[derive(Debug, Serialize)]
struct WorkerRunDetail {
    run: models::WorkerRun,
    invocations: Vec<models::WorkerInvocation>,
}

async fn get_worker_run(
    State(state): State<ApiState>,
    axum::extract::Path(run_id): axum::extract::Path<String>,
) -> Result<Json<WorkerRunDetail>, ApiError> {
    let repository = state.manager.repository();
    let run = repository.get_worker_run(&run_id)?.ok_or_else(|| {
        ApiError(EngineError::ToolCatalogNotFound(format!(
            "worker run '{run_id}' not found"
        )))
    })?;
    let invocations = repository.list_worker_invocations(Some(&run_id), None, 100)?;
    Ok(Json(WorkerRunDetail { run, invocations }))
}

/// wire 字符串 → [`models::WorkerRuntimeType`] 的解析包装。
struct WorkerRuntimeTypeParse(models::WorkerRuntimeType);

impl std::str::FromStr for WorkerRuntimeTypeParse {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        models::WorkerRuntimeType::all()
            .into_iter()
            .find(|runtime| runtime.as_str() == value)
            .map(Self)
            .ok_or(())
    }
}

const FINGERPRINT_PACK_ENV: &str = "LYNCEUS_FINGERPRINT_PACKS_DIR";
const FINGERPRINT_PACK_MAX_BYTES: u64 = 64 * 1024 * 1024;
const FINGERPRINT_HEX: &[u8; 16] = b"0123456789abcdef";

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum FingerprintRuleFormat {
    EholeJson,
    FingerprinthubJson,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FingerprintRulePackSource {
    repository_url: String,
    revision: String,
    source_path: String,
    license_spdx: String,
    license_source_path: String,
    retrieved_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FingerprintRulePackManifest {
    #[serde(default = "default_schema_version")]
    schema_version: i64,
    id: String,
    name: String,
    format: FingerprintRuleFormat,
    data_file: String,
    license_file: String,
    sha256: String,
    license_sha256: String,
    rule_count: usize,
    #[serde(default)]
    redistribution_reviewed: bool,
    source: FingerprintRulePackSource,
}

const fn default_schema_version() -> i64 {
    1
}

fn fingerprint_pack_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Ok(path) = std::env::var(FINGERPRINT_PACK_ENV)
        && !path.trim().is_empty()
    {
        roots.push(PathBuf::from(path));
    }
    roots.push(PathBuf::from("data/fingerprint-packs"));
    roots.push(PathBuf::from("resources/fingerprints"));
    roots
}

fn safe_fingerprint_child(root: &FsPath, relative: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(relative);
    if relative.trim().is_empty() || path.is_absolute() {
        return Err(format!(
            "fingerprint pack path must be relative: {relative}"
        ));
    }
    if path
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(format!(
            "fingerprint pack path escapes its root: {relative}"
        ));
    }
    Ok(root.join(path))
}

fn hex_digest(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        output.push(char::from(FINGERPRINT_HEX[usize::from(byte >> 4)]));
        output.push(char::from(FINGERPRINT_HEX[usize::from(byte & 0x0f)]));
    }
    output
}

fn is_lower_hex_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

#[allow(clippy::too_many_lines)]
fn validate_fingerprint_manifest(
    manifest_path: &FsPath,
) -> Result<FingerprintRulePackManifest, String> {
    let manifest_text = fs::read_to_string(manifest_path).map_err(|error| {
        format!(
            "read fingerprint manifest {}: {error}",
            manifest_path.display()
        )
    })?;
    let manifest: FingerprintRulePackManifest =
        serde_json::from_str(&manifest_text).map_err(|error| {
            format!(
                "invalid fingerprint manifest {}: {error}",
                manifest_path.display()
            )
        })?;
    let root = manifest_path
        .parent()
        .ok_or_else(|| "fingerprint manifest has no parent directory".to_string())?;
    if root.file_name().and_then(|name| name.to_str()) != Some(manifest.id.as_str()) {
        return Err(format!(
            "fingerprint rule-pack directory must match manifest id: {} != {}",
            root.display(),
            manifest.id
        ));
    }
    if manifest.schema_version != 1
        || manifest.id.is_empty()
        || manifest.id.len() > 128
        || !manifest.id.chars().enumerate().all(|(index, character)| {
            (index == 0 && (character.is_ascii_lowercase() || character.is_ascii_digit()))
                || (index > 0
                    && (character.is_ascii_lowercase()
                        || character.is_ascii_digit()
                        || matches!(character, '.' | '_' | '-')))
        })
    {
        return Err(format!("invalid fingerprint rule-pack id: {}", manifest.id));
    }
    if manifest.name.trim().is_empty() || manifest.name.chars().count() > 300 {
        return Err("fingerprint rule-pack name must contain 1..300 characters".to_string());
    }
    if manifest.source.repository_url.trim().is_empty()
        || manifest.source.repository_url.chars().count() > 2048
        || manifest.source.revision.chars().count() < 7
        || manifest.source.revision.chars().count() > 128
        || manifest.source.license_spdx.trim().is_empty()
        || manifest.source.license_spdx.chars().count() > 128
    {
        return Err("fingerprint rule-pack source metadata is invalid".to_string());
    }
    for path in [
        &manifest.source.source_path,
        &manifest.source.license_source_path,
    ] {
        let source_path = PathBuf::from(path);
        if source_path.is_absolute()
            || source_path.as_os_str().is_empty()
            || source_path
                .components()
                .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            return Err("fingerprint rule-pack source paths must be relative".to_string());
        }
    }
    if !is_lower_hex_sha256(&manifest.sha256) || !is_lower_hex_sha256(&manifest.license_sha256) {
        return Err("fingerprint rule-pack checksums must be lowercase SHA-256".to_string());
    }
    if manifest.rule_count == 0 || manifest.rule_count > 100_000 {
        return Err(format!(
            "invalid fingerprint rule count: {}",
            manifest.rule_count
        ));
    }
    let data_path = safe_fingerprint_child(root, &manifest.data_file)?;
    let license_path = safe_fingerprint_child(root, &manifest.license_file)?;
    if data_path == license_path
        || manifest.data_file == "manifest.json"
        || manifest.license_file == "manifest.json"
    {
        return Err(
            "rule data and license must be distinct and manifest.json is reserved".to_string(),
        );
    }
    let data_metadata = fs::symlink_metadata(&data_path)
        .map_err(|error| format!("read fingerprint rule data metadata: {error}"))?;
    if !data_metadata.is_file()
        || data_metadata.file_type().is_symlink()
        || data_metadata.len() == 0
        || data_metadata.len() > FINGERPRINT_PACK_MAX_BYTES
    {
        return Err(format!(
            "fingerprint rule data size is invalid: {}",
            data_metadata.len()
        ));
    }
    let data =
        fs::read(&data_path).map_err(|error| format!("read fingerprint rule data: {error}"))?;
    let license_metadata = fs::symlink_metadata(&license_path)
        .map_err(|error| format!("read fingerprint license metadata: {error}"))?;
    if !license_metadata.is_file() || license_metadata.file_type().is_symlink() {
        return Err("fingerprint rule-pack license is not a regular file".to_string());
    }
    let license =
        fs::read(&license_path).map_err(|error| format!("read fingerprint license: {error}"))?;
    if String::from_utf8_lossy(&license).trim().is_empty() {
        return Err("fingerprint rule-pack license file is empty".to_string());
    }
    if hex_digest(&data) != manifest.sha256 {
        return Err(format!(
            "fingerprint rule data sha256 mismatch: {}",
            data_path.display()
        ));
    }
    if hex_digest(&license) != manifest.license_sha256 {
        return Err(format!(
            "fingerprint license sha256 mismatch: {}",
            license_path.display()
        ));
    }
    let payload: Value = serde_json::from_slice(&data)
        .map_err(|error| format!("invalid fingerprint rule JSON: {error}"))?;
    let actual_count = match manifest.format {
        FingerprintRuleFormat::EholeJson => {
            let Some(rules) = payload.get("fingerprint").and_then(Value::as_array) else {
                return Err("EHole rule pack requires a top-level fingerprint array".to_string());
            };
            for (index, rule) in rules.iter().enumerate() {
                let Some(object) = rule.as_object() else {
                    return Err(format!("EHole rule {index} is not an object"));
                };
                let Some(cms) = object.get("cms").and_then(Value::as_str) else {
                    return Err(format!("EHole rule {index} requires cms"));
                };
                let method = object.get("method").and_then(Value::as_str);
                let location = object.get("location").and_then(Value::as_str);
                let Some(keywords) = object.get("keyword").and_then(Value::as_array) else {
                    return Err(format!("EHole rule {index} has invalid keywords"));
                };
                if cms.trim().is_empty()
                    || !matches!(method, Some("keyword" | "regular" | "faviconhash"))
                    || !matches!(location, Some("body" | "header" | "title"))
                    || keywords.is_empty()
                    || keywords.len() > 64
                    || keywords
                        .iter()
                        .any(|keyword| keyword.as_str().is_none_or(str::is_empty))
                {
                    return Err(format!("EHole rule {index} has invalid fields"));
                }
            }
            rules.len()
        }
        FingerprintRuleFormat::FingerprinthubJson => {
            let Some(rules) = payload.as_array() else {
                return Err("FingerprintHub rule pack requires a top-level array".to_string());
            };
            let mut ids = HashSet::new();
            for (index, rule) in rules.iter().enumerate() {
                let Some(object) = rule.as_object() else {
                    return Err(format!("FingerprintHub rule {index} is not an object"));
                };
                let Some(id) = object.get("id").and_then(Value::as_str) else {
                    return Err("FingerprintHub rule requires a string id".to_string());
                };
                let info_name = object
                    .get("info")
                    .and_then(Value::as_object)
                    .and_then(|info| info.get("name"))
                    .and_then(Value::as_str);
                if id.trim().is_empty()
                    || info_name.is_none()
                    || object.get("http").and_then(Value::as_array).is_none()
                {
                    return Err(format!("FingerprintHub rule {index} has invalid fields"));
                }
                ids.insert(id.to_string());
            }
            ids.len()
        }
    };
    if actual_count != manifest.rule_count {
        return Err(format!(
            "fingerprint rule count mismatch: manifest={}, actual={actual_count}",
            manifest.rule_count
        ));
    }
    Ok(manifest)
}

fn list_valid_fingerprint_manifests() -> Result<Vec<FingerprintRulePackManifest>, EngineError> {
    let mut manifests = Vec::new();
    let mut roots = fingerprint_pack_roots();
    roots.sort();
    roots.dedup();
    for root in roots {
        if !root.is_dir() {
            continue;
        }
        let mut directories = fs::read_dir(&root)
            .map_err(|error| {
                EngineError::Value(format!(
                    "read fingerprint pack root {}: {error}",
                    root.display()
                ))
            })?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .collect::<Vec<_>>();
        directories.sort();
        for directory in directories {
            let manifest_path = directory.join("manifest.json");
            if manifest_path.is_file() {
                manifests.push(
                    validate_fingerprint_manifest(&manifest_path).map_err(EngineError::Value)?,
                );
            }
        }
    }
    manifests.sort_unstable_by(|left, right| left.id.cmp(&right.id));
    let mut seen = HashSet::new();
    for manifest in &manifests {
        if !seen.insert(manifest.id.clone()) {
            return Err(EngineError::Value(format!(
                "duplicate fingerprint rule-pack id: {}",
                manifest.id
            )));
        }
    }
    Ok(manifests)
}

async fn list_fingerprint_rule_packs() -> Result<Json<Vec<FingerprintRulePackManifest>>, ApiError> {
    Ok(Json(list_valid_fingerprint_manifests()?))
}

async fn get_fingerprint_rule_pack(
    Path(pack_id): Path<String>,
) -> Result<Json<FingerprintRulePackManifest>, ApiError> {
    let manifests = list_valid_fingerprint_manifests()?;
    manifests
        .into_iter()
        .find(|manifest| manifest.id == pack_id)
        .map(Json)
        .ok_or_else(|| ApiError(EngineError::FingerprintRulePackNotFound(pack_id)))
}

#[derive(Debug, Deserialize)]
struct RuntimeSettingQuery {
    project_id: Option<String>,
    run_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UpsertRuntimeSettingRequest {
    #[serde(default)]
    key: Option<String>,
    #[serde(default)]
    value: Map<String, Value>,
    #[serde(default)]
    scope: RuntimeSettingScope,
    project_id: Option<String>,
    run_id: Option<String>,
    #[serde(default)]
    description: String,
    #[serde(default = "default_runtime_updated_by")]
    updated_by: String,
}

fn default_runtime_updated_by() -> String {
    "system".to_string()
}

// ---------------------------------------------------------------------------
// Intelligence Hub
// ---------------------------------------------------------------------------

/// `GET /intelligence/sources` 只返回真实注册、可执行的 source。
#[derive(Debug, Serialize)]
struct IntelSourcesResponse {
    /// 已注册并可查询的 source。
    sources: Vec<intelligence::IntelSourceInfo>,
}

async fn list_intel_sources(State(state): State<ApiState>) -> Json<IntelSourcesResponse> {
    Json(IntelSourcesResponse {
        sources: state.intelligence.source_infos(),
    })
}

async fn run_intel_query(
    State(state): State<ApiState>,
    Json(query): Json<models::IntelQuery>,
) -> Result<Json<intelligence::IntelExpansionReport>, ApiError> {
    let seed = query.seed.trim();
    if seed.is_empty() {
        return Err(ApiError(EngineError::Value(
            "intelligence query seed must not be empty".to_string(),
        )));
    }
    if seed.len() > 2048 {
        return Err(ApiError(EngineError::Value(
            "intelligence query seed exceeds 2048 bytes".to_string(),
        )));
    }
    if query.limit == 0 || query.limit > 1000 {
        return Err(ApiError(EngineError::Value(
            "intelligence query limit must be between 1 and 1000".to_string(),
        )));
    }
    if query.source_ids.len() > 16 || query.filters.len() > 16 {
        return Err(ApiError(EngineError::Value(
            "intelligence query has too many sources or filters".to_string(),
        )));
    }
    let valid_seed = match query.query_type {
        models::IntelQueryType::Domain => intelligence::normalize::normalize_domain(seed).is_some(),
        models::IntelQueryType::Ip => intelligence::normalize::normalize_ip(seed).is_some(),
        models::IntelQueryType::Url => intelligence::normalize::normalize_url(seed).is_some(),
        _ => true,
    };
    if !valid_seed {
        return Err(ApiError(EngineError::Value(format!(
            "invalid {} intelligence seed",
            query.query_type.as_str()
        ))));
    }
    if serde_json::to_vec(&query.filters).map_or(true, |encoded| encoded.len() > 16 * 1024) {
        return Err(ApiError(EngineError::Value(
            "intelligence query filters exceed 16 KiB".to_string(),
        )));
    }
    Ok(Json(state.intelligence.expand(&query).await))
}

/// `GET /intelligence/entities` 的查询参数。
#[derive(Debug, Deserialize)]
struct IntelEntitiesQuery {
    kind: Option<models::IntelEntityKind>,
    q: Option<String>,
    limit: Option<usize>,
}

async fn list_intel_entities(
    State(state): State<ApiState>,
    Query(query): Query<IntelEntitiesQuery>,
) -> Result<Json<Vec<models::IntelEntityRecord>>, ApiError> {
    Ok(Json(state.manager.repository().list_intel_entities(
        query.kind,
        query.q.as_deref(),
        query.limit.unwrap_or(200).min(1000),
    )?))
}

/// `GET /intelligence/relations` 的查询参数。
#[derive(Debug, Deserialize)]
struct IntelRelationsQuery {
    entity_id: Option<String>,
}

async fn list_intel_relations(
    State(state): State<ApiState>,
    Query(query): Query<IntelRelationsQuery>,
) -> Result<Json<Vec<models::IntelRelationRecord>>, ApiError> {
    Ok(Json(
        state
            .manager
            .repository()
            .list_intel_relations(query.entity_id.as_deref())?,
    ))
}

/// `GET /intelligence/raw-records` 的查询参数。
#[derive(Debug, Deserialize)]
struct IntelRawRecordsQuery {
    source: Option<String>,
    limit: Option<usize>,
}

async fn list_intel_raw_records(
    State(state): State<ApiState>,
    Query(query): Query<IntelRawRecordsQuery>,
) -> Result<Json<Vec<models::IntelRawRecord>>, ApiError> {
    Ok(Json(state.manager.repository().list_intel_raw_records(
        query.source.as_deref(),
        query.limit.unwrap_or(50).min(200),
    )?))
}

/// `POST /intelligence/entities/{id}/promote` 的请求体。
#[derive(Debug, Deserialize)]
struct PromoteIntelEntityRequest {
    /// 目标 Mission（资产落在其 project 下）。
    mission_id: String,
}

async fn promote_intel_entity(
    State(state): State<ApiState>,
    Path(entity_id): Path<String>,
    Json(body): Json<PromoteIntelEntityRequest>,
) -> Result<Json<intelligence::PromoteOutcome>, ApiError> {
    state
        .intelligence
        .promote_entity(&entity_id, &body.mission_id)
        .map(Json)
        .map_err(|error| ApiError(EngineError::Value(error.to_string())))
}

async fn list_runtime_settings(
    State(state): State<ApiState>,
    Query(query): Query<RuntimeSettingQuery>,
) -> Result<Json<Vec<RuntimeSetting>>, ApiError> {
    Ok(Json(state.manager.repository().list_runtime_settings(
        query.project_id.as_deref(),
        query.run_id.as_deref(),
    )?))
}

async fn get_runtime_setting(
    State(state): State<ApiState>,
    Path(key): Path<String>,
    Query(query): Query<RuntimeSettingQuery>,
) -> Result<Json<Option<RuntimeSetting>>, ApiError> {
    Ok(Json(state.manager.repository().get_runtime_setting(
        &key,
        query.project_id.as_deref(),
        query.run_id.as_deref(),
    )?))
}

async fn upsert_runtime_setting(
    State(state): State<ApiState>,
    Path(key): Path<String>,
    Json(body): Json<UpsertRuntimeSettingRequest>,
) -> Result<Json<RuntimeSetting>, ApiError> {
    if let Some(body_key) = &body.key
        && body_key.trim().to_lowercase() != key.trim().to_lowercase()
    {
        return Err(ApiError(EngineError::Value(
            "body key does not match path key".to_string(),
        )));
    }
    let mut setting = RuntimeSetting::new(&key, body.value)
        .map_err(EngineError::Value)
        .map_err(ApiError)?;
    setting.scope = body.scope;
    setting.project_id = body.project_id.map(ProjectId::new);
    setting.run_id = body.run_id.map(models::RunId::new);
    setting.description = body.description;
    setting.updated_by = body.updated_by;
    Ok(Json(
        state
            .manager
            .repository()
            .upsert_runtime_setting(&setting)?,
    ))
}

async fn delete_runtime_setting(
    State(state): State<ApiState>,
    Path(key): Path<String>,
    Query(query): Query<RuntimeSettingQuery>,
) -> Result<StatusCode, ApiError> {
    state.manager.repository().delete_runtime_setting(
        &key,
        query.project_id.as_deref(),
        query.run_id.as_deref(),
    )?;
    Ok(StatusCode::NO_CONTENT)
}

const GRAPHRAG_RETIRED_REASON: &str = "GraphRAG compatibility API is retired; use /knowledge/cards/search for tactical knowledge";

fn graphrag_state_value() -> Value {
    serde_json::json!({
        "state": "unavailable",
        "root_path": "",
        "input_path": "",
        "output_path": "",
        "error": GRAPHRAG_RETIRED_REASON
    })
}

async fn graphrag_state_retired() -> (StatusCode, Json<Value>) {
    (StatusCode::GONE, Json(graphrag_state_value()))
}

async fn graphrag_action_retired() -> (StatusCode, Json<Value>) {
    (
        StatusCode::GONE,
        Json(serde_json::json!({
            "ok": false,
            "state": graphrag_state_value(),
            "message": GRAPHRAG_RETIRED_REASON
        })),
    )
}

async fn graphrag_query_retired(Json(body): Json<Value>) -> (StatusCode, Json<Value>) {
    let raw_user_query = body
        .get("raw_user_query")
        .and_then(Value::as_str)
        .unwrap_or_default();
    (
        StatusCode::GONE,
        Json(serde_json::json!({
            "status": "unavailable",
            "method": "local",
            "raw_user_query": raw_user_query,
            "answer": null,
            "no_hit_gap": GRAPHRAG_RETIRED_REASON,
            "metadata": {
                "reason": GRAPHRAG_RETIRED_REASON,
                "knowledge_search": "/knowledge/cards/search",
                "evidence_search": "/retrieval/search"
            }
        })),
    )
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ConfigureToolRequest {
    executable_path: Option<String>,
    enabled: bool,
    /// 调用参数整体替换集（按 invocation 声明逐项校验）；缺省 = 保持已存值。
    params: Option<Map<String, Value>>,
    /// env 逐 key 合并集；value 为 `null` 或空串清除该 key，缺省 key 保留。
    /// 明文只在落盘后用于子进程注入，任何响应都不回显。
    env: Option<BTreeMap<String, Option<String>>>,
}

impl Default for ConfigureToolRequest {
    fn default() -> Self {
        Self {
            executable_path: None,
            enabled: true,
            params: None,
            env: None,
        }
    }
}

#[derive(Debug, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct StartToolInstallRequest {
    force: bool,
}

#[derive(Debug, Deserialize, Default)]
struct ToolInstallationsQuery {
    tool_id: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct ToolRecommendationsQuery {
    project_id: Option<String>,
    mission_id: Option<String>,
}

fn map_tool_catalog_error(error: &ToolCatalogError) -> ApiError {
    match error {
        ToolCatalogError::NotFound(tool_id) => ApiError(EngineError::ToolCatalogNotFound(format!(
            "\"tool '{tool_id}' not found in catalog\""
        ))),
        // 非法调用配置属于请求侧校验失败（ValueError 族 → 422）。
        ToolCatalogError::InvalidConfig(detail) => ApiError(EngineError::Value(detail.clone())),
        _ => ApiError(EngineError::ToolCatalogFailure(error.to_string())),
    }
}

/// 快照文件与 local-tools.json 同目录（测试注入路径时天然隔离；
/// 生产默认 `data/config/tool-detection.json`）。
fn snapshot_path_for(local_tools_path: &std::path::Path) -> PathBuf {
    engines::tool_catalog::detection_snapshot_path_for(local_tools_path)
}

/// 探测状态摘要（`GET /tool-catalog/status` / `POST /refresh` 的
/// 响应）：前端据此显示「最后检测时间」并决定是否提示 refresh。
#[derive(Debug, Serialize)]
struct ToolDetectionStatusResponse {
    /// 当前 refresh lifecycle。
    state: engines::tool_catalog::ToolDetectionRuntimeState,
    /// 上次全量探测时间（从未探测过为 `None`）。
    detected_at: Option<String>,
    /// 快照中的工具条数。
    detection_count: usize,
    /// 其中可用的条数。
    available_count: usize,
    /// 是否从未有过有效快照。
    stale: bool,
    /// 最近一次 refresh 的有界错误（无错误为 None）。
    last_error: Option<String>,
}

fn detection_status_response(
    snapshot_path: &std::path::Path,
    snapshot: Option<&engines::tool_catalog::ToolDetectionSnapshot>,
) -> ToolDetectionStatusResponse {
    let (state, last_error) =
        engines::tool_catalog::detection_runtime_state(snapshot_path, snapshot);
    match snapshot {
        Some(snapshot) => ToolDetectionStatusResponse {
            state,
            detected_at: Some(snapshot.detected_at.clone()),
            detection_count: snapshot.detections.len(),
            available_count: snapshot
                .detections
                .iter()
                .filter(|record| record.detection.available)
                .count(),
            stale: state != engines::tool_catalog::ToolDetectionRuntimeState::Ready,
            last_error,
        },
        None => ToolDetectionStatusResponse {
            state,
            detected_at: None,
            detection_count: 0,
            available_count: 0,
            stale: true,
            last_error,
        },
    }
}

/// `GET /tool-catalog`：立即返回 元数据 + 持久化配置 + 上次探测
/// 快照的合并视图。**禁止**在这里扫描 PATH / spawn 任何进程——
/// 真实探测只走 `POST /tool-catalog/refresh`。
async fn list_tool_catalog(
    State(state): State<ApiState>,
) -> Result<Json<Vec<ToolCatalogEntry>>, ApiError> {
    let snapshot =
        engines::tool_catalog::load_detection_snapshot(&snapshot_path_for(&state.local_tools_path));
    Ok(Json(
        engines::tool_catalog::catalog_entries_from_snapshot(
            &state.local_tools_path,
            snapshot.as_ref(),
        )
        .map_err(|error| map_tool_catalog_error(&error))?,
    ))
}

/// `GET /tool-catalog/status`：只读快照元信息（同样绝不探测）。
async fn tool_catalog_status(State(state): State<ApiState>) -> Json<ToolDetectionStatusResponse> {
    let snapshot_path = snapshot_path_for(&state.local_tools_path);
    let snapshot = engines::tool_catalog::load_detection_snapshot(&snapshot_path);
    Json(detection_status_response(&snapshot_path, snapshot.as_ref()))
}

/// `POST /tool-catalog/refresh`：真实全量探测的唯一入口（PATH 扫描
/// + 身份校验），结果写磁盘快照并更新内存缓存。
async fn refresh_tool_catalog(
    State(state): State<ApiState>,
) -> Result<Json<ToolDetectionStatusResponse>, ApiError> {
    let snapshot = engines::tool_catalog::refresh_detection_snapshot(
        &state.local_tools_path,
        &snapshot_path_for(&state.local_tools_path),
    )
    .await
    .map_err(|error| map_tool_catalog_error(&error))?;
    Ok(Json(detection_status_response(
        &snapshot_path_for(&state.local_tools_path),
        Some(&snapshot),
    )))
}

async fn list_tool_installations(
    State(state): State<ApiState>,
    Query(query): Query<ToolInstallationsQuery>,
) -> Json<Vec<ToolInstallJob>> {
    Json(state.tool_installs.list(query.tool_id.as_deref()).await)
}

async fn get_tool_installation(
    State(state): State<ApiState>,
    Path(job_id): Path<String>,
) -> Result<Json<ToolInstallJob>, ApiError> {
    state
        .tool_installs
        .get(&job_id)
        .await
        .map(Json)
        .ok_or_else(|| {
            ApiError(EngineError::ToolCatalogNotFound(format!(
                "tool install job not found: {job_id}"
            )))
        })
}

async fn install_tool_catalog(
    State(state): State<ApiState>,
    Path(tool_id): Path<String>,
    Json(body): Json<StartToolInstallRequest>,
) -> Result<(StatusCode, Json<ToolInstallJob>), ApiError> {
    let job = state
        .tool_installs
        .start(&tool_id, body.force)
        .await
        .map_err(|error| map_tool_catalog_error(&error))?;
    Ok((StatusCode::ACCEPTED, Json(job)))
}

async fn configure_tool_catalog(
    State(state): State<ApiState>,
    Path(tool_id): Path<String>,
    Json(body): Json<ConfigureToolRequest>,
) -> Result<Json<ToolCatalogEntry>, ApiError> {
    // settings 请求先做声明面预检：invocation 声明 + 落点存在性。
    // 404 语义与 configure_local_tool 内部的 NotFound 保持一致。
    let wants_settings = body.params.is_some() || body.env.is_some();
    if wants_settings {
        let tools = load_catalog().map_err(|error| map_tool_catalog_error(&error))?;
        let tool = tools
            .iter()
            .find(|tool| tool.id.eq_ignore_ascii_case(&tool_id))
            .ok_or_else(|| {
                ApiError(EngineError::ToolCatalogNotFound(format!(
                    "\"tool '{tool_id}' not found in catalog\""
                )))
            })?;
        tool_settings::ensure_settings_target(
            tool,
            body.executable_path.as_deref(),
            &state.local_tools_path,
        )
        .map_err(|error| map_tool_catalog_error(&error))?;
    }
    configure_local_tool(
        &tool_id,
        body.executable_path.as_deref(),
        body.enabled,
        &state.local_tools_path,
    )
    .map_err(|error| map_tool_catalog_error(&error))?;
    if wants_settings {
        let tools = load_catalog().map_err(|error| map_tool_catalog_error(&error))?;
        let tool = tools
            .iter()
            .find(|tool| tool.id.eq_ignore_ascii_case(&tool_id))
            .ok_or_else(|| {
                ApiError(EngineError::ToolCatalogFailure(format!(
                    "configured tool disappeared from catalog: {tool_id}"
                )))
            })?;
        tool_settings::apply_tool_settings(
            tool,
            body.params.as_ref(),
            body.env.as_ref(),
            &state.local_tools_path,
        )
        .map_err(|error| map_tool_catalog_error(&error))?;
    }
    // 事件驱动失效：configure 落盘成功后立刻单工具重测并更新快照
    //（绝不全量探测）。快照更新失败不回滚 configure——configured
    // detection 在 list 合并视图中本来就是 live 的。
    let entry = engines::tool_catalog::redetect_tool_into_snapshot(
        &tool_id,
        &state.local_tools_path,
        &snapshot_path_for(&state.local_tools_path),
    )
    .await
    .map_err(|error| map_tool_catalog_error(&error))?
    .ok_or_else(|| {
        ApiError(EngineError::ToolCatalogFailure(format!(
            "configured tool disappeared from catalog: {tool_id}"
        )))
    })?;
    sync_local_tool_module(&state, &entry, body.enabled)?;
    Ok(Json(entry))
}

fn sync_local_tool_module(
    state: &ApiState,
    entry: &ToolCatalogEntry,
    enabled: bool,
) -> Result<(), ApiError> {
    let Some(executable_path) = entry.detection.executable_path.as_ref() else {
        return Ok(());
    };
    let canonical_name = entry.id.to_lowercase();
    let existing = state
        .manager
        .repository()
        .list_modules()?
        .into_iter()
        .find(|module| {
            module.module_type == ModuleType::LocalTool
                && module
                    .metadata
                    .get("tool_name")
                    .and_then(Value::as_str)
                    .is_some_and(|name| name.trim().eq_ignore_ascii_case(&canonical_name))
        });
    let domain = normalize_module_domain(&entry.domain)
        .map_err(|error| ApiError(EngineError::ToolCatalogFailure(error.to_string())))?;
    let mut module = ModuleConfig::new(entry.name.clone());
    module.module_type = ModuleType::LocalTool;
    module.domain = domain;
    module.transport = ModuleTransport::None;
    module.enabled = enabled;
    module.profile = ModuleProfile::FullAccess;
    module.tool_allowlist = vec![canonical_name.clone()];
    module.capability_map.insert(
        format!("{}.{}.scan", domain.as_str(), canonical_name),
        canonical_name.clone(),
    );
    module
        .metadata
        .insert("tool_name".to_string(), Value::String(canonical_name));
    module.metadata.insert(
        "executable_path".to_string(),
        Value::String(executable_path.clone()),
    );
    module.metadata.insert(
        "version_args".to_string(),
        serde_json::json!(entry.version_args),
    );
    if let Some(existing) = existing {
        module.id = existing.id;
        module.created_at = existing.created_at;
        module.updated_at = utcnow();
        state.manager.update_module(module)?;
    } else {
        state.manager.create_module(module)?;
    }
    Ok(())
}

async fn test_tool_catalog(
    State(state): State<ApiState>,
    Path(tool_id): Path<String>,
) -> Result<Json<ToolHealthResult>, ApiError> {
    // 单工具探测：test 一个工具不该触发全目录 PATH 扫描。
    let entry = engines::tool_catalog::detect_single_tool(&tool_id, &state.local_tools_path)
        .await
        .map_err(|error| map_tool_catalog_error(&error))?
        .ok_or_else(|| {
            ApiError(EngineError::ToolCatalogNotFound(format!(
                "tool '{tool_id}' not found in catalog"
            )))
        })?;
    Ok(Json(test_tool_health(&entry).await))
}

async fn recommend_tool_catalog(
    State(state): State<ApiState>,
    Query(query): Query<ToolRecommendationsQuery>,
) -> Result<Json<Vec<ToolRecommendation>>, ApiError> {
    let mut mission = None;
    let mut branch_metadata = Map::new();
    let mut resolved_project_id = query.project_id;
    if let Some(mission_id) = query.mission_id {
        let resolved = state
            .manager
            .repository()
            .get_mission(&mission_id)?
            .ok_or_else(|| {
                ApiError(EngineError::MissionNotFound(format!(
                    "mission not found: {mission_id}"
                )))
            })?;
        if resolved_project_id
            .as_deref()
            .is_some_and(|project_id| project_id != resolved.project_id.as_str())
        {
            return Err(ApiError(EngineError::Value(
                "project_id does not match mission_id".to_string(),
            )));
        }
        resolved_project_id = Some(resolved.project_id.as_str().to_string());
        if let Some(branch) = state
            .manager
            .repository()
            .list_branches(
                Some(resolved.project_id.as_str()),
                Some(resolved.id.as_str()),
                None,
            )?
            .into_iter()
            .next()
        {
            branch_metadata = branch.metadata;
        }
        mission = Some(resolved);
    }
    let (facts, findings, evidence) = if let Some(project_id) = resolved_project_id.as_deref() {
        state
            .manager
            .repository()
            .get_project(project_id)?
            .ok_or_else(|| {
                ApiError(EngineError::ProjectNotFound(format!(
                    "project not found: {project_id}"
                )))
            })?;
        (
            state.manager.repository().list_facts(project_id)?,
            state.manager.repository().list_findings(project_id)?,
            state.manager.repository().list_evidence(project_id)?,
        )
    } else {
        (Vec::new(), Vec::new(), Vec::new())
    };
    let snapshot =
        engines::tool_catalog::load_detection_snapshot(&snapshot_path_for(&state.local_tools_path));
    let catalog = engines::tool_catalog::catalog_entries_from_snapshot(
        &state.local_tools_path,
        snapshot.as_ref(),
    )
    .map_err(|error| map_tool_catalog_error(&error))?;
    Ok(Json(recommend_tools(
        mission.as_ref(),
        &branch_metadata,
        &facts,
        &findings,
        &evidence,
        &catalog,
    )))
}

const fn default_max_attempts() -> u8 {
    1
}

/// `POST /tool-catalog/search` 请求体（开发/诊断用途的 ToolRetriever
/// 直查入口；与 runtime bootstrap 共用同一 `CatalogToolRetriever`，
/// 不是第二套检索系统）。
#[derive(Debug, Deserialize)]
struct ToolCatalogSearchRequest {
    /// 自由文本查询。
    #[serde(default)]
    text: String,
    /// 能力查询。
    #[serde(default)]
    capability_queries: Vec<String>,
    /// 工件 kinds（elf/pcap/url/source…）。
    #[serde(default)]
    artifact_kinds: Vec<String>,
    /// 显式工具名（强加成，但仍经 Catalog/policy）。
    #[serde(default)]
    explicit_tools: Vec<String>,
    /// 风险上限（none/low/medium/high）。
    #[serde(default)]
    risk_limit: Option<String>,
    /// 返回条数上限（默认 10，硬上限 20）。
    #[serde(default)]
    limit: Option<usize>,
}

/// 工具检索诊断端点：对完整 Catalog（+ 原生/远程描述符）执行与
/// Solver bootstrap 相同的检索，返回 compact 候选（绝不执行工具）。
async fn search_tool_catalog(
    State(state): State<ApiState>,
    Json(request): Json<ToolCatalogSearchRequest>,
) -> Result<Json<models::ToolRetrievalResult>, ApiError> {
    use agents::tool_retrieval::ToolRetriever;

    let snapshot =
        engines::tool_catalog::load_detection_snapshot(&snapshot_path_for(&state.local_tools_path));
    let catalog = engines::tool_catalog::catalog_entries_from_snapshot(
        &state.local_tools_path,
        snapshot.as_ref(),
    )
    .map_err(|error| map_tool_catalog_error(&error))?;
    let mut extra = engines::tool_retrieval::all_native_descriptors();
    extra.push(engines::tool_retrieval::ida_mcp_descriptor());
    let index = engines::tool_retrieval::ToolRetrievalIndex::from_catalog(&catalog)
        .with_extra_descriptors(extra);
    let retriever = engines::tool_retrieval::CatalogToolRetriever::from_index(index);
    let query = models::ToolRetrievalQuery {
        text: request.text,
        capability_queries: request.capability_queries,
        artifact_kinds: request.artifact_kinds,
        explicit_tools: request.explicit_tools,
        risk_limit: request.risk_limit,
        limit: request.limit.unwrap_or(10).clamp(1, 20),
    };
    Ok(Json(retriever.retrieve(&query)))
}

fn default_execution_wait_seconds() -> f64 {
    1.0
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SubmitExecutionRequest {
    request: ExecutionRequest,
    #[serde(default)]
    safe_to_retry: bool,
    #[serde(default = "default_max_attempts")]
    max_attempts: u8,
    #[serde(default)]
    idempotency_key: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct ListExecutionsQuery {
    project_id: Option<String>,
    run_id: Option<String>,
    status: Option<ExecutionStatus>,
}

#[derive(Debug, Deserialize)]
struct WaitExecutionQuery {
    #[serde(default = "default_execution_wait_seconds")]
    timeout_seconds: f64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CancelExecutionRequest {
    #[serde(default)]
    note: String,
}

#[derive(Debug, Serialize)]
struct ExecutionWaitResponse {
    job: ExecutionJob,
    timed_out: bool,
}

fn execution_api_error(error: ExecutionControlError) -> ApiError {
    match error {
        ExecutionControlError::Storage(error) => ApiError(EngineError::Storage(error)),
        ExecutionControlError::Invalid(message) => ApiError(EngineError::Value(message)),
        ExecutionControlError::DuplicateBackend(message) => ApiError(EngineError::Value(format!(
            "execution backend already registered: {message}"
        ))),
        ExecutionControlError::NotFound(execution_id) => {
            ApiError(EngineError::ExecutionNotFound(execution_id))
        }
        ExecutionControlError::Conflict(message) => {
            ApiError(EngineError::ExecutionConflict(message))
        }
    }
}

async fn submit_execution(
    State(state): State<ApiState>,
    Json(body): Json<SubmitExecutionRequest>,
) -> Result<(StatusCode, Json<ExecutionJob>), ApiError> {
    if body.request.backend_type != ExecutionBackendType::Local {
        return Err(ApiError(EngineError::ExecutionBackendForbidden(
            "public execution submission currently permits only the local safe backend".to_string(),
        )));
    }
    let job = state
        .execution_control
        .submit(
            body.request,
            SubmitExecutionOptions {
                safe_to_retry: body.safe_to_retry,
                max_attempts: body.max_attempts,
                idempotency_key: body.idempotency_key,
            },
        )
        .await
        .map_err(execution_api_error)?;
    Ok((StatusCode::ACCEPTED, Json(job)))
}

async fn list_executions(
    State(state): State<ApiState>,
    Query(query): Query<ListExecutionsQuery>,
) -> Result<Json<Vec<ExecutionJob>>, ApiError> {
    let jobs = state
        .execution_control
        .list_jobs(
            query.project_id.as_deref(),
            query.run_id.as_deref(),
            query.status,
        )
        .map_err(execution_api_error)?;
    Ok(Json(jobs))
}

async fn get_execution(
    State(state): State<ApiState>,
    Path(execution_id): Path<String>,
) -> Result<Json<ExecutionJob>, ApiError> {
    state
        .execution_control
        .get(&execution_id)
        .map_err(execution_api_error)?
        .map(Json)
        .ok_or_else(|| ApiError(EngineError::ExecutionNotFound(execution_id)))
}

async fn wait_execution(
    State(state): State<ApiState>,
    Path(execution_id): Path<String>,
    Query(query): Query<WaitExecutionQuery>,
) -> Result<Json<ExecutionWaitResponse>, ApiError> {
    if !query.timeout_seconds.is_finite() || !(0.0..=30.0).contains(&query.timeout_seconds) {
        return Err(ApiError(EngineError::Value(
            "timeout_seconds must be a finite number between 0 and 30".to_string(),
        )));
    }
    let outcome = state
        .execution_control
        .wait(
            &execution_id,
            std::time::Duration::from_secs_f64(query.timeout_seconds),
        )
        .await
        .map_err(execution_api_error)?;
    Ok(Json(ExecutionWaitResponse {
        job: outcome.job,
        timed_out: outcome.timed_out,
    }))
}

async fn cancel_execution(
    State(state): State<ApiState>,
    Path(execution_id): Path<String>,
    Json(body): Json<CancelExecutionRequest>,
) -> Result<Json<ExecutionJob>, ApiError> {
    if body.note.chars().count() > 2000 {
        return Err(ApiError(EngineError::Value(
            "note must not exceed 2000 characters".to_string(),
        )));
    }
    let job = state
        .execution_control
        .cancel(&execution_id, &body.note)
        .await
        .map_err(execution_api_error)?;
    Ok(Json(job))
}

const fn module_type_label(module_type: models::ModuleType) -> &'static str {
    match module_type {
        models::ModuleType::Builtin => "builtin",
        models::ModuleType::LocalTool => "local_tool",
        models::ModuleType::McpRemote => "mcp_remote",
    }
}

#[derive(Debug, Deserialize)]
struct CreateModuleRequest {
    name: String,
    #[serde(default)]
    module_type: Option<models::ModuleType>,
    #[serde(default)]
    domain: Option<models::ModuleDomain>,
    #[serde(default)]
    endpoint_url: Option<String>,
    #[serde(default)]
    transport: Option<models::ModuleTransport>,
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    profile: Option<models::ModuleProfile>,
    #[serde(default)]
    tool_allowlist: Vec<String>,
    #[serde(default)]
    capability_map: StrMap,
    #[serde(default)]
    metadata: Map<String, Value>,
}

#[derive(Debug, Deserialize)]
struct UpdateModuleRequest {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    module_type: Option<models::ModuleType>,
    #[serde(default)]
    domain: Option<models::ModuleDomain>,
    #[serde(default)]
    endpoint_url: Option<String>,
    #[serde(default)]
    transport: Option<models::ModuleTransport>,
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    profile: Option<models::ModuleProfile>,
    #[serde(default)]
    tool_allowlist: Option<Vec<String>>,
    #[serde(default)]
    capability_map: Option<StrMap>,
    #[serde(default)]
    metadata: Option<Map<String, Value>>,
}

async fn list_modules(
    State(state): State<ApiState>,
) -> Result<Json<Vec<models::ModuleConfig>>, ApiError> {
    Ok(Json(state.manager.repository().list_modules()?))
}

async fn create_module(
    State(state): State<ApiState>,
    Json(body): Json<CreateModuleRequest>,
) -> Result<(StatusCode, Json<models::ModuleConfig>), ApiError> {
    let mut module = models::ModuleConfig::new(body.name);
    if let Some(value) = body.module_type {
        module.module_type = value;
    }
    if let Some(value) = body.domain {
        module.domain = value;
    }
    module.endpoint_url = body.endpoint_url;
    if let Some(value) = body.transport {
        module.transport = value;
    }
    if let Some(value) = body.enabled {
        module.enabled = value;
    }
    if let Some(value) = body.profile {
        module.profile = value;
    }
    module.tool_allowlist = body.tool_allowlist;
    module.capability_map = body.capability_map;
    module.metadata = body.metadata;
    Ok((
        StatusCode::CREATED,
        Json(state.manager.create_module(module)?),
    ))
}

async fn get_module(
    State(state): State<ApiState>,
    Path(module_id): Path<String>,
) -> Result<Json<models::ModuleConfig>, ApiError> {
    let module = state
        .manager
        .repository()
        .get_module(&module_id)?
        .ok_or_else(|| EngineError::Value(format!("module not found: {module_id}")))?;
    Ok(Json(module))
}

async fn update_module(
    State(state): State<ApiState>,
    Path(module_id): Path<String>,
    Json(body): Json<UpdateModuleRequest>,
) -> Result<Json<models::ModuleConfig>, ApiError> {
    let mut module = state
        .manager
        .repository()
        .get_module(&module_id)?
        .ok_or_else(|| EngineError::Value(format!("module not found: {module_id}")))?;
    if let Some(name) = body.name.filter(|name| !name.trim().is_empty()) {
        module.name = name;
    }
    if let Some(value) = body.module_type {
        module.module_type = value;
    }
    if let Some(value) = body.domain {
        module.domain = value;
    }
    if body.endpoint_url.is_some() {
        module.endpoint_url = body.endpoint_url;
    }
    if let Some(value) = body.transport {
        module.transport = value;
    }
    if let Some(value) = body.enabled {
        module.enabled = value;
    }
    if let Some(value) = body.profile {
        module.profile = value;
    }
    if let Some(tool_allowlist) = body.tool_allowlist {
        module.tool_allowlist = tool_allowlist;
    }
    if let Some(capability_map) = body.capability_map {
        module.capability_map = capability_map;
    }
    if let Some(metadata) = body.metadata {
        module.metadata = metadata;
    }
    module.updated_at = models::utcnow();
    Ok(Json(state.manager.update_module(module)?))
}

async fn delete_module(
    State(state): State<ApiState>,
    Path(module_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    state.manager.delete_module(&module_id)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn module_health(
    State(state): State<ApiState>,
) -> Result<Json<Map<String, Value>>, ApiError> {
    let modules = state.manager.repository().list_modules()?;
    let tools = state.manager.available_tool_names();
    let mut result = Map::new();
    for module in modules {
        let ready = module.enabled
            && module.tool_allowlist.iter().all(|tool| {
                tools
                    .as_ref()
                    .is_some_and(|available| available.contains(tool))
            });
        result.insert(
            module.id.as_str().to_string(),
            serde_json::json!({
                "status": if ready { "ok" } else { "unavailable" },
                "available": ready,
                "module_type": module_type_label(module.module_type),
                "tool_allowlist": module.tool_allowlist,
            }),
        );
    }
    Ok(Json(result))
}

#[derive(Debug, Serialize)]
struct ProviderResponse {
    id: models::ProviderId,
    name: String,
    provider_type: ProviderType,
    base_url: Option<String>,
    model: Option<String>,
    api_key_ref: Option<String>,
    has_api_key: bool,
    default_headers: StrMap,
    timeout_seconds: u64,
    max_tokens: Option<u64>,
    temperature: Option<f64>,
    is_default: bool,
    enabled: bool,
    created_at: models::Timestamp,
    updated_at: models::Timestamp,
}

fn provider_response(provider: ProviderConfig) -> ProviderResponse {
    let has_api_key = provider.has_secret();
    ProviderResponse {
        id: provider.id,
        name: provider.name,
        provider_type: provider.provider_type,
        base_url: provider.base_url,
        model: provider.model,
        api_key_ref: provider.api_key_ref,
        has_api_key,
        default_headers: provider.default_headers,
        timeout_seconds: provider.timeout_seconds,
        max_tokens: provider.max_tokens,
        temperature: provider.temperature,
        is_default: provider.is_default,
        enabled: provider.enabled,
        created_at: provider.created_at,
        updated_at: provider.updated_at,
    }
}

#[derive(Debug, Deserialize)]
struct CreateProviderRequest {
    name: String,
    #[serde(default = "default_provider_type_api")]
    provider_type: ProviderType,
    #[serde(default)]
    base_url: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    api_key_ref: Option<String>,
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default)]
    default_headers: StrMap,
    #[serde(default = "default_provider_timeout")]
    timeout_seconds: u64,
    #[serde(default)]
    max_tokens: Option<u64>,
    #[serde(default)]
    temperature: Option<f64>,
    #[serde(default)]
    is_default: bool,
    #[serde(default = "default_enabled")]
    enabled: bool,
}

fn default_provider_timeout() -> u64 {
    60
}

fn default_provider_type_api() -> ProviderType {
    ProviderType::OpenaiCompatible
}

fn default_enabled() -> bool {
    true
}

#[derive(Debug, Deserialize)]
struct UpdateProviderRequest {
    name: Option<String>,
    provider_type: Option<ProviderType>,
    base_url: Option<String>,
    model: Option<String>,
    api_key_ref: Option<String>,
    api_key: Option<String>,
    default_headers: Option<StrMap>,
    timeout_seconds: Option<u64>,
    max_tokens: Option<u64>,
    temperature: Option<f64>,
    is_default: Option<bool>,
    enabled: Option<bool>,
}

async fn list_providers(
    State(state): State<ApiState>,
) -> Result<Json<Vec<ProviderResponse>>, ApiError> {
    let providers = state
        .manager
        .repository()
        .list_providers()?
        .into_iter()
        .map(provider_response)
        .collect();
    Ok(Json(providers))
}

async fn default_provider(
    State(state): State<ApiState>,
) -> Result<Json<Option<ProviderResponse>>, ApiError> {
    let provider = state
        .manager
        .repository()
        .list_providers()?
        .into_iter()
        .find(|provider| provider.enabled && provider.is_default)
        .map(provider_response);
    Ok(Json(provider))
}

async fn create_provider(
    State(state): State<ApiState>,
    Json(body): Json<CreateProviderRequest>,
) -> Result<(StatusCode, Json<ProviderResponse>), ApiError> {
    let mut provider = ProviderConfig::new(body.name, body.provider_type);
    provider.base_url = body.base_url;
    provider.model = body.model;
    provider.api_key_ref = body.api_key_ref;
    provider.encrypted_api_key = body.api_key;
    provider.default_headers = body.default_headers;
    provider.timeout_seconds = body.timeout_seconds;
    provider.max_tokens = body.max_tokens;
    provider.temperature = body.temperature;
    provider.is_default = body.is_default;
    provider.enabled = body.enabled;
    let saved = state.manager.create_provider(provider)?;
    Ok((StatusCode::CREATED, Json(provider_response(saved))))
}

async fn get_provider(
    State(state): State<ApiState>,
    Path(provider_id): Path<String>,
) -> Result<Json<ProviderResponse>, ApiError> {
    let provider = state
        .manager
        .repository()
        .get_provider(&provider_id)?
        .ok_or_else(|| EngineError::ProviderNotFound(provider_id.clone()))?;
    Ok(Json(provider_response(provider)))
}

async fn update_provider(
    State(state): State<ApiState>,
    Path(provider_id): Path<String>,
    Json(body): Json<UpdateProviderRequest>,
) -> Result<Json<ProviderResponse>, ApiError> {
    let mut provider = state
        .manager
        .repository()
        .get_provider(&provider_id)?
        .ok_or_else(|| EngineError::ProviderNotFound(provider_id.clone()))?;
    if let Some(name) = body.name {
        provider.name = name;
    }
    if let Some(provider_type) = body.provider_type {
        provider.provider_type = provider_type;
    }
    if let Some(base_url) = body.base_url {
        provider.base_url = Some(base_url);
    }
    if let Some(model) = body.model {
        provider.model = Some(model);
    }
    if let Some(api_key_ref) = body.api_key_ref {
        provider.api_key_ref = Some(api_key_ref);
    }
    if let Some(api_key) = body.api_key {
        provider.encrypted_api_key = Some(api_key);
    }
    if let Some(default_headers) = body.default_headers {
        provider.default_headers = default_headers;
    }
    if let Some(timeout_seconds) = body.timeout_seconds {
        provider.timeout_seconds = timeout_seconds;
    }
    if let Some(max_tokens) = body.max_tokens {
        provider.max_tokens = Some(max_tokens);
    }
    if let Some(temperature) = body.temperature {
        provider.temperature = Some(temperature);
    }
    if let Some(is_default) = body.is_default {
        provider.is_default = is_default;
    }
    if let Some(enabled) = body.enabled {
        provider.enabled = enabled;
    }
    provider.updated_at = models::utcnow();
    let saved = state.manager.update_provider(provider)?;
    Ok(Json(provider_response(saved)))
}

async fn delete_provider(
    State(state): State<ApiState>,
    Path(provider_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    state.manager.delete_provider(&provider_id)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
struct UpsertProviderRouteRequest {
    purpose: String,
    provider_id: models::ProviderId,
    #[serde(default)]
    model_override: Option<String>,
    #[serde(default)]
    priority: Option<i64>,
    #[serde(default)]
    weight: Option<i64>,
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    fallback_group: Option<String>,
    #[serde(default)]
    required_capabilities: Map<String, Value>,
    #[serde(default)]
    max_failures: Option<i64>,
    #[serde(default)]
    cooldown_seconds: Option<i64>,
    #[serde(default)]
    metadata: Map<String, Value>,
}

fn provider_route_from_request(
    request: UpsertProviderRouteRequest,
) -> Result<models::ProviderRouteBinding, ApiError> {
    let mut route = models::ProviderRouteBinding::new(&request.purpose, request.provider_id)
        .map_err(|error| ApiError(EngineError::ProviderConfigError(error.to_string())))?;
    route.model_override = request.model_override;
    if let Some(value) = request.priority {
        route.priority = value;
    }
    if let Some(value) = request.weight {
        route.weight = value;
    }
    if let Some(value) = request.enabled {
        route.enabled = value;
    }
    if let Some(value) = request.fallback_group {
        route.fallback_group = value;
    }
    route.required_capabilities = request.required_capabilities;
    if let Some(value) = request.max_failures {
        route.max_failures = value;
    }
    if let Some(value) = request.cooldown_seconds {
        route.cooldown_seconds = value;
    }
    route.metadata = request.metadata;
    route.updated_at = models::utcnow();
    Ok(route)
}

#[derive(Debug, Deserialize, Default)]
struct ProviderRouteQuery {
    purpose: Option<String>,
}

async fn list_provider_routes(
    State(state): State<ApiState>,
    Query(query): Query<ProviderRouteQuery>,
) -> Result<Json<Vec<models::ProviderRouteBinding>>, ApiError> {
    Ok(Json(
        state
            .manager
            .list_provider_routes(query.purpose.as_deref())?,
    ))
}

async fn create_provider_route(
    State(state): State<ApiState>,
    Json(body): Json<UpsertProviderRouteRequest>,
) -> Result<(StatusCode, Json<models::ProviderRouteBinding>), ApiError> {
    let route = provider_route_from_request(body)?;
    Ok((
        StatusCode::CREATED,
        Json(state.manager.create_provider_route(route)?),
    ))
}

async fn update_provider_route(
    State(state): State<ApiState>,
    Path(route_id): Path<String>,
    Json(body): Json<UpsertProviderRouteRequest>,
) -> Result<Json<models::ProviderRouteBinding>, ApiError> {
    let existing = state
        .manager
        .repository()
        .get_provider_route(&route_id)?
        .ok_or_else(|| EngineError::Value(format!("provider route not found: {route_id}")))?;
    let mut route = provider_route_from_request(body)?;
    route.id = existing.id;
    route.created_at = existing.created_at;
    route.failure_count = existing.failure_count;
    route.circuit_open_until = existing.circuit_open_until;
    Ok(Json(state.manager.update_provider_route(route)?))
}

async fn delete_provider_route(
    State(state): State<ApiState>,
    Path(route_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    state.manager.delete_provider_route(&route_id)?;
    Ok(StatusCode::NO_CONTENT)
}

#[allow(clippy::struct_excessive_bools)]
// These flags are part of the frozen provider-capability wire contract.
#[derive(Debug, Deserialize)]
struct UpsertModelCapabilityRequest {
    provider_id: String,
    model: String,
    #[serde(default)]
    context_window: Option<i64>,
    #[serde(default)]
    max_output_tokens: Option<i64>,
    #[serde(default)]
    supports_json: bool,
    #[serde(default)]
    supports_tools: bool,
    #[serde(default)]
    supports_vision: bool,
    #[serde(default)]
    supports_embeddings: bool,
    #[serde(default)]
    provider_native_tools: Vec<String>,
    #[serde(default)]
    metadata: Map<String, Value>,
}

#[derive(Debug, Deserialize, Default)]
struct ModelCapabilityQuery {
    provider_id: Option<String>,
}

async fn list_model_capabilities(
    State(state): State<ApiState>,
    Query(query): Query<ModelCapabilityQuery>,
) -> Result<Json<Vec<models::ModelCapability>>, ApiError> {
    Ok(Json(
        state
            .manager
            .list_model_capabilities(query.provider_id.as_deref())?,
    ))
}

async fn upsert_model_capability(
    State(state): State<ApiState>,
    Json(body): Json<UpsertModelCapabilityRequest>,
) -> Result<(StatusCode, Json<models::ModelCapability>), ApiError> {
    if body.context_window.is_some_and(|value| value < 1)
        || body.max_output_tokens.is_some_and(|value| value < 1)
    {
        return Err(ApiError(EngineError::Value(
            "model capability limits must be positive".to_string(),
        )));
    }
    let provider_id = models::ProviderId::new(body.provider_id);
    let capability = models::ModelCapability {
        id: models::ModelCapabilityId::new(model_capability_id(provider_id.as_str(), &body.model)),
        provider_id,
        model: body.model,
        context_window: body.context_window,
        max_output_tokens: body.max_output_tokens,
        supports_json: body.supports_json,
        supports_tools: body.supports_tools,
        supports_vision: body.supports_vision,
        supports_embeddings: body.supports_embeddings,
        provider_native_tools: body.provider_native_tools,
        metadata: body.metadata,
        created_at: models::utcnow(),
        updated_at: models::utcnow(),
    };
    Ok((
        StatusCode::CREATED,
        Json(state.manager.upsert_model_capability(capability)?),
    ))
}

#[derive(Debug, Deserialize)]
struct DiscoverProviderModelsRequest {
    #[serde(default)]
    provider_id: Option<String>,
    #[serde(default)]
    provider_type: Option<ProviderType>,
    #[serde(default)]
    base_url: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    api_key_ref: Option<String>,
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default)]
    default_headers: Option<StrMap>,
    #[serde(default)]
    timeout_seconds: Option<u64>,
}

async fn discover_models(
    State(state): State<ApiState>,
    Json(body): Json<DiscoverProviderModelsRequest>,
) -> Result<Json<ProviderModelDiscoveryResult>, ApiError> {
    let runtime = state
        .manager
        .provider_discovery_runtime()
        .ok_or(ApiError(EngineError::ProviderRuntimeUnavailable))?;
    let mut provider = if let Some(provider_id) = body.provider_id.as_deref() {
        state
            .manager
            .repository()
            .get_provider(provider_id)?
            .ok_or_else(|| EngineError::ProviderNotFound(provider_id.to_string()))?
    } else {
        ProviderConfig::new(
            "model discovery preview".to_string(),
            body.provider_type.unwrap_or(ProviderType::OpenaiCompatible),
        )
    };
    if let Some(provider_type) = body.provider_type {
        provider.provider_type = provider_type;
    }
    if let Some(base_url) = body.base_url {
        provider.base_url = Some(base_url);
    }
    if let Some(model) = body.model {
        provider.model = Some(model);
    }
    if let Some(api_key_ref) = body.api_key_ref {
        provider.api_key_ref = Some(api_key_ref);
    }
    if let Some(api_key) = body.api_key {
        provider.encrypted_api_key = Some(api_key);
    }
    if let Some(default_headers) = body.default_headers {
        provider.default_headers = default_headers;
    }
    if let Some(timeout_seconds) = body.timeout_seconds {
        provider.timeout_seconds = timeout_seconds;
    }
    let mut result = runtime
        .discover_models(&provider)
        .await
        .map_err(|error| ApiError(EngineError::ProviderConfigError(error.to_string())))?;
    result.provider_id = body.provider_id.map(models::ProviderId::new);
    if result.status == models::ModelInvocationStatus::Ok
        && let Some(provider_id) = result.provider_id.clone()
    {
        for model in &result.models {
            let now = models::utcnow();
            let capability = models::ModelCapability {
                id: models::ModelCapabilityId::new(model_capability_id(
                    provider_id.as_str(),
                    model,
                )),
                provider_id: provider_id.clone(),
                model: model.clone(),
                context_window: None,
                max_output_tokens: None,
                supports_json: false,
                supports_tools: false,
                supports_vision: false,
                supports_embeddings: false,
                provider_native_tools: Vec::new(),
                metadata: Map::from_iter([(
                    "source".to_string(),
                    Value::String("provider_discovery".to_string()),
                )]),
                created_at: now,
                updated_at: now,
            };
            state.manager.upsert_model_capability(capability)?;
        }
    }
    Ok(Json(result))
}

async fn test_provider(
    State(state): State<ApiState>,
    Path(provider_id): Path<String>,
) -> Result<Json<ProviderHealthResult>, ApiError> {
    state
        .manager
        .repository()
        .get_provider(&provider_id)?
        .ok_or_else(|| EngineError::ProviderNotFound(provider_id.clone()))?;
    let runtime = state
        .manager
        .provider_runtime()
        .ok_or(ApiError(EngineError::ProviderRuntimeUnavailable))?;
    let result = runtime
        .health_check(&provider_id)
        .await
        .map_err(|error| ApiError(EngineError::ProviderConfigError(error.to_string())))?;
    Ok(Json(result))
}

#[derive(Debug, Deserialize)]
struct CreateProjectRequest {
    name: String,
    audit_domain: AuditDomain,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    target: StrMap,
    #[serde(default)]
    goal: Option<String>,
}

async fn create_project(
    State(state): State<ApiState>,
    Json(body): Json<CreateProjectRequest>,
) -> Result<(StatusCode, Json<Project>), ApiError> {
    let project = state
        .manager
        .create_project(
            &body.name,
            body.audit_domain,
            body.description.as_deref(),
            Some(&body.target),
            body.goal.as_deref(),
        )
        .await?;
    Ok((StatusCode::CREATED, Json(project)))
}

async fn list_projects(State(state): State<ApiState>) -> Result<Json<Vec<Project>>, ApiError> {
    Ok(Json(state.manager.repository().list_projects()?))
}

async fn get_project(
    State(state): State<ApiState>,
    Path(project_id): Path<String>,
) -> Result<Json<Project>, ApiError> {
    let project = state
        .manager
        .repository()
        .get_project(&project_id)?
        .ok_or_else(|| EngineError::ProjectNotFound(format!("unknown project: {project_id}")))?;
    Ok(Json(project))
}

async fn delete_project(
    State(state): State<ApiState>,
    Path(project_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    state.manager.delete_project(&project_id)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
struct MissionListQuery {
    project_id: Option<String>,
}

async fn list_missions(
    State(state): State<ApiState>,
    Query(query): Query<MissionListQuery>,
) -> Result<Json<Vec<Mission>>, ApiError> {
    let project_id = query.project_id.map(ProjectId::new);
    Ok(Json(state.manager.list_missions(project_id.as_ref())?))
}

#[derive(Debug, Deserialize)]
struct CreateMissionRequest {
    user_goal: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    target: StrMap,
    #[serde(default)]
    project_id: Option<ProjectId>,
    #[serde(default)]
    constraints: Vec<String>,
    #[serde(default)]
    success_criteria: Vec<String>,
    #[serde(default)]
    goal_contract: Option<models::MissionGoalContract>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default = "default_approval_mode")]
    approval_mode: ApprovalMode,
    #[serde(default = "default_created_by")]
    created_by: String,
    #[serde(default)]
    metadata: Map<String, Value>,
}

#[derive(Debug, Deserialize)]
struct BatchMissionActionRequest {
    mission_ids: Vec<String>,
    action: String,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
}

fn default_created_by() -> String {
    "user".to_string()
}

fn default_approval_mode() -> ApprovalMode {
    ApprovalMode::AskForApproval
}

async fn create_mission(
    State(state): State<ApiState>,
    Json(body): Json<CreateMissionRequest>,
) -> Result<(StatusCode, Json<Mission>), ApiError> {
    if body.user_goal.trim().is_empty() {
        return Err(ApiError(EngineError::RequestValidation(
            serde_json::json!([
                {
                    "type": "string_too_short",
                    "loc": ["body", "user_goal"],
                    "msg": "String should have at least 1 character",
                    "input": body.user_goal,
                    "ctx": {"min_length": 1}
                }
            ]),
        )));
    }
    let target = body
        .target
        .iter()
        .map(|(key, value)| (key.to_string(), Value::String(value.to_string())))
        .collect();
    let input = CreateMissionInput {
        user_goal: body.user_goal,
        title: body.title,
        target: Some(target),
        project_id: body.project_id,
        constraints: body.constraints,
        success_criteria: body.success_criteria,
        goal_contract: body.goal_contract,
        tags: body.tags,
        category: body.category,
        approval_mode: body.approval_mode,
        created_by: body.created_by,
        metadata: body.metadata,
    };
    let mission = state.manager.create_mission(input).await?;
    let workspace =
        mission_workspace::workspace_path_from_mission(&state.mission_workspace_root, &mission)
            .map_err(|error| ApiError(EngineError::Value(error.to_string())))?;
    let mission = mission_workspace::mission_with_workspace_metadata(&mission, &workspace);
    let mission = state.manager.repository().update_mission(&mission)?;
    Ok((StatusCode::CREATED, Json(mission)))
}

async fn batch_update_missions(
    State(state): State<ApiState>,
    Json(body): Json<BatchMissionActionRequest>,
) -> Result<Json<Vec<Mission>>, ApiError> {
    let BatchMissionActionRequest {
        mission_ids: raw_ids,
        action,
        category,
        tags,
    } = body;
    let mission_ids = raw_ids.into_iter().map(MissionId::new).collect::<Vec<_>>();
    Ok(Json(
        state
            .manager
            .batch_update_missions(&mission_ids, &action, category.as_deref(), &tags)
            .await?,
    ))
}

async fn get_mission(
    State(state): State<ApiState>,
    Path(mission_id): Path<String>,
) -> Result<Json<Mission>, ApiError> {
    let mission = state
        .manager
        .repository()
        .get_mission(&mission_id)?
        .ok_or_else(|| EngineError::MissionNotFound(format!("unknown mission: {mission_id}")))?;
    Ok(Json(mission))
}

/// `GET /projects/{project_id}/agent-narratives` 的查询参数。
#[derive(Debug, Deserialize)]
struct AgentNarrativesQuery {
    run_id: Option<String>,
    mission_id: Option<String>,
    branch_id: Option<String>,
    limit: Option<usize>,
}

/// `GET /projects/{project_id}/agent-narratives`：人读 agent 叙事事件，
/// seq 升序（`contracts/openapi.json` 同名端点的 Rust 实现）。
async fn list_agent_narratives(
    State(state): State<ApiState>,
    Path(project_id): Path<String>,
    Query(query): Query<AgentNarrativesQuery>,
) -> Result<Json<Vec<models::AgentNarrativeEvent>>, ApiError> {
    Ok(Json(
        state.manager.repository().list_agent_narrative_events(
            &project_id,
            query.run_id.as_deref(),
            query.mission_id.as_deref(),
            query.branch_id.as_deref(),
            Some(query.limit.unwrap_or(100).min(1000)),
        )?,
    ))
}

/// `POST /projects/{project_id}/agent-narratives`：追加一条叙事事件，
/// id 与 created_at 由服务端生成。
async fn create_agent_narrative(
    State(state): State<ApiState>,
    Path(project_id): Path<String>,
    Json(body): Json<models::CreateAgentNarrativeRequest>,
) -> Result<Json<models::AgentNarrativeEvent>, ApiError> {
    let event = models::AgentNarrativeEvent::new(models::ProjectId::new(project_id), body);
    state
        .manager
        .repository()
        .add_agent_narrative_event(&event)?;
    Ok(Json(event))
}

#[derive(Debug, Deserialize)]
struct UpdateMissionRequest {
    user_goal: Option<String>,
    title: Option<String>,
    tags: Option<Vec<String>>,
    category: Option<String>,
    archived: Option<bool>,
    approval_mode: Option<ApprovalMode>,
    metadata: Option<Map<String, Value>>,
    goal_contract: Option<models::MissionGoalContract>,
}

async fn update_mission(
    State(state): State<ApiState>,
    Path(mission_id): Path<String>,
    Json(body): Json<UpdateMissionRequest>,
) -> Result<Json<Mission>, ApiError> {
    let input = runtime::mission_lifecycle::UpdateMissionInput {
        user_goal: body.user_goal,
        title: body.title,
        tags: body.tags,
        category: body.category,
        archived: body.archived,
        approval_mode: body.approval_mode,
        metadata: body.metadata,
        goal_contract: body.goal_contract,
    };
    Ok(Json(
        state
            .manager
            .update_mission_metadata(&mission_id, input)
            .await?,
    ))
}

async fn delete_mission(
    State(state): State<ApiState>,
    Path(mission_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    state.manager.delete_mission(&mission_id)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_mission_branches(
    State(state): State<ApiState>,
    Path(mission_id): Path<String>,
) -> Result<Json<Vec<models::Branch>>, ApiError> {
    let mission = state
        .manager
        .repository()
        .get_mission(&mission_id)?
        .ok_or_else(|| EngineError::MissionNotFound(format!("unknown mission: {mission_id}")))?;
    Ok(Json(state.manager.repository().list_branches(
        None,
        Some(mission.id.as_str()),
        None,
    )?))
}

#[derive(Debug, Deserialize, Default)]
struct BranchActionRequest {
    #[serde(default)]
    reason: String,
}

#[derive(Debug, Deserialize, Default)]
struct ResumeMissionRequest {
    #[serde(default)]
    new_requirement: Option<String>,
    #[serde(default)]
    config: Map<String, Value>,
}

#[derive(Debug, Deserialize)]
struct ApplyDirectiveRequest {
    directive_type: UserDirectiveType,
    content: String,
    #[serde(default)]
    branch_id: Option<String>,
    #[serde(default = "default_created_by")]
    created_by: String,
    #[serde(default)]
    metadata: Map<String, Value>,
}

#[derive(Debug, Deserialize)]
struct MissionSignalRequest {
    signal_type: MissionSignalType,
    content: String,
    #[serde(default)]
    branch_id: Option<String>,
    #[serde(default = "default_created_by")]
    created_by: String,
    #[serde(default)]
    metadata: Map<String, Value>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum MissionSignalType {
    Confirm,
    Reject,
    AddRequirement,
    Pause,
    Resume,
    AbandonBranch,
    PrioritizeBranch,
    InjectHint,
}

impl MissionSignalType {
    const fn directive_type(self) -> UserDirectiveType {
        match self {
            Self::AddRequirement => UserDirectiveType::AddRequirement,
            Self::Pause => UserDirectiveType::Pause,
            Self::Resume => UserDirectiveType::Resume,
            Self::AbandonBranch => UserDirectiveType::AbandonBranch,
            Self::PrioritizeBranch => UserDirectiveType::PrioritizeBranch,
            Self::Confirm | Self::Reject | Self::InjectHint => UserDirectiveType::AskQuestion,
        }
    }

    const fn requires_branch(self) -> bool {
        matches!(self, Self::AbandonBranch | Self::PrioritizeBranch)
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Confirm => "confirm",
            Self::Reject => "reject",
            Self::AddRequirement => "add_requirement",
            Self::Pause => "pause",
            Self::Resume => "resume",
            Self::AbandonBranch => "abandon_branch",
            Self::PrioritizeBranch => "prioritize_branch",
            Self::InjectHint => "inject_hint",
        }
    }
}

#[derive(Debug, Serialize)]
struct MissionSignalResponse {
    accepted: bool,
    signal_type: MissionSignalType,
    directive_id: Option<String>,
    mission_id: String,
    note: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AnswerDecisionGateRequest {
    #[serde(default)]
    option_id: Option<String>,
    #[serde(default)]
    freeform_text: Option<String>,
    #[serde(default = "default_created_by")]
    answered_by: String,
    #[serde(default)]
    rationale: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CancelDecisionGateRequest {
    #[serde(default = "default_created_by")]
    cancelled_by: String,
    #[serde(default)]
    rationale: Option<String>,
}

async fn pause_mission(
    State(state): State<ApiState>,
    Path(mission_id): Path<String>,
    body: Option<Json<BranchActionRequest>>,
) -> Result<Json<Mission>, ApiError> {
    let reason = body
        .as_ref()
        .map_or("Mission paused by user", |request| request.reason.as_str());
    Ok(Json(
        state
            .manager
            .pause_mission(&MissionId::new(mission_id), Some(reason), None)
            .await?,
    ))
}

async fn resume_mission(
    State(state): State<ApiState>,
    Path(mission_id): Path<String>,
    body: Option<Json<ResumeMissionRequest>>,
) -> Result<(StatusCode, Json<MissionStartResponse>), ApiError> {
    let request = body.map_or_else(ResumeMissionRequest::default, |Json(value)| value);
    let mission_id = MissionId::new(mission_id);
    let result = state
        .manager
        .resume_mission(
            &mission_id,
            request.new_requirement.as_deref(),
            Some(request.config),
            true,
        )
        .await?;
    let run = state
        .manager
        .repository()
        .get_run(result.run_id.as_str())?
        .ok_or_else(|| EngineError::RunNotFound(result.run_id.as_str().to_string()))?;
    Ok((
        StatusCode::ACCEPTED,
        Json(MissionStartResponse {
            mission: result.mission,
            run,
            branches: result.branches,
        }),
    ))
}

async fn reassess_mission_completion(
    State(state): State<ApiState>,
    Path(mission_id): Path<String>,
) -> Result<Json<models::TerminationAssessment>, ApiError> {
    Ok(Json(
        state
            .manager
            .reassess_mission_completion(&MissionId::new(mission_id))
            .await?,
    ))
}

async fn get_branch(
    State(state): State<ApiState>,
    Path(branch_id): Path<String>,
) -> Result<Json<models::Branch>, ApiError> {
    let branch = state
        .manager
        .repository()
        .get_branch(&branch_id)?
        .ok_or_else(|| EngineError::BranchNotFound(branch_id.clone()))?;
    Ok(Json(branch))
}

async fn prioritize_branch(
    State(state): State<ApiState>,
    Path(branch_id): Path<String>,
    body: Option<Json<BranchActionRequest>>,
) -> Result<Json<models::UserDirective>, ApiError> {
    let branch = state
        .manager
        .repository()
        .get_branch(&branch_id)?
        .ok_or_else(|| EngineError::BranchNotFound(branch_id.clone()))?;
    let reason = body
        .as_ref()
        .map_or("Prioritize this branch", |request| request.reason.as_str());
    Ok(Json(
        state
            .manager
            .apply_user_directive(
                &branch.mission_id,
                UserDirectiveType::PrioritizeBranch,
                reason,
                Some(&branch.id),
                "user",
                None,
            )
            .await?,
    ))
}

async fn abandon_branch(
    State(state): State<ApiState>,
    Path(branch_id): Path<String>,
    body: Option<Json<BranchActionRequest>>,
) -> Result<Json<models::Branch>, ApiError> {
    let reason = body.as_ref().map_or("", |request| request.reason.as_str());
    Ok(Json(
        state.manager.abandon_branch(&branch_id, reason).await?,
    ))
}

async fn reopen_branch(
    State(state): State<ApiState>,
    Path(branch_id): Path<String>,
    body: Option<Json<BranchActionRequest>>,
) -> Result<Json<models::Branch>, ApiError> {
    let reason = body.as_ref().map_or("", |request| request.reason.as_str());
    Ok(Json(state.manager.reopen_branch(&branch_id, reason).await?))
}

async fn list_mission_assets(
    State(state): State<ApiState>,
    Path(mission_id): Path<String>,
    Query(query): Query<MissionAssetQuery>,
) -> Result<Json<Vec<models::MissionAsset>>, ApiError> {
    let mission = state
        .manager
        .repository()
        .get_mission(&mission_id)?
        .ok_or_else(|| EngineError::MissionNotFound(format!("unknown mission: {mission_id}")))?;
    Ok(Json(state.manager.repository().list_mission_assets(
        Some(mission.id.as_str()),
        Some(mission.project_id.as_str()),
        query.sensitivity,
        query.asset_type,
    )?))
}

#[derive(Debug, Deserialize, Default)]
struct MissionAssetQuery {
    sensitivity: Option<models::MissionAssetSensitivity>,
    asset_type: Option<models::MissionAssetType>,
}

async fn list_mission_directives(
    State(state): State<ApiState>,
    Path(mission_id): Path<String>,
) -> Result<Json<Vec<models::UserDirective>>, ApiError> {
    let mission = state
        .manager
        .repository()
        .get_mission(&mission_id)?
        .ok_or_else(|| EngineError::MissionNotFound(format!("unknown mission: {mission_id}")))?;
    Ok(Json(state.manager.repository().list_user_directives(
        Some(mission.project_id.as_str()),
        Some(mission.id.as_str()),
        None,
        None,
    )?))
}

async fn apply_directive(
    State(state): State<ApiState>,
    Path(mission_id): Path<String>,
    Json(body): Json<ApplyDirectiveRequest>,
) -> Result<(StatusCode, Json<models::UserDirective>), ApiError> {
    if body.content.trim().is_empty() {
        return Err(ApiError(EngineError::Value(
            "content must not be blank".to_string(),
        )));
    }
    let branch_id = body
        .branch_id
        .as_deref()
        .map(|value| models::BranchId::new(value.to_string()));
    let directive = state
        .manager
        .apply_user_directive(
            &MissionId::new(mission_id),
            body.directive_type,
            &body.content,
            branch_id.as_ref(),
            &body.created_by,
            Some(body.metadata),
        )
        .await?;
    Ok((StatusCode::CREATED, Json(directive)))
}

/// POST /missions/{mission_id}/advise —— 只读任务顾问（Direct API，不占
/// worker 槽；就当前任务上下文回答操作员问题）。
#[derive(Debug, Deserialize)]
struct MissionAdviseRequest {
    question: String,
    /// 多轮历史（客户端本地维护，服务端无状态）。
    #[serde(default)]
    history: Vec<MissionAdviseTurn>,
}

#[derive(Debug, Deserialize)]
struct MissionAdviseTurn {
    role: String,
    content: String,
}

#[derive(Debug, Serialize)]
struct MissionAdviseResponse {
    answer: String,
    model: Option<String>,
}

async fn mission_advise(
    State(state): State<ApiState>,
    Path(mission_id): Path<String>,
    Json(body): Json<MissionAdviseRequest>,
) -> Result<Json<MissionAdviseResponse>, ApiError> {
    let question = body.question.trim().to_string();
    if question.is_empty() {
        return Err(ApiError(EngineError::Value(
            "advise question must not be empty".to_string(),
        )));
    }
    let mission = state
        .manager
        .repository()
        .get_mission(&mission_id)?
        .ok_or_else(|| EngineError::MissionNotFound(format!("unknown mission: {mission_id}")))?;
    let service = crate::intake::IntakeService::new(
        Arc::clone(&state.manager),
        state.mission_workspace_root.clone(),
    );
    let history: Vec<(String, String)> = body
        .history
        .iter()
        .map(|turn| (turn.role.clone(), turn.content.clone()))
        .collect();
    let (answer, agent) = service
        .advise(&mission, &question, &history)
        .await
        .map_err(crate::intake::map_intake_error)?;
    Ok(Json(MissionAdviseResponse {
        answer,
        model: agent,
    }))
}

/// `POST /missions/{mission_id}/findings/{finding_id}/retests` 请求体。
#[derive(Debug, Deserialize)]
struct StartRetestRequest {
    /// 复测补充说明（可选，最多 4000 字符）。
    #[serde(default)]
    notes: String,
}

/// `GET /missions/{mission_id}/findings/{finding_id}/retests`。
///
/// 返回该漏洞的全部复测记录（新的在前）。
async fn list_mission_finding_retests(
    State(state): State<ApiState>,
    Path((mission_id, finding_id)): Path<(String, String)>,
) -> Result<Json<Vec<models::FindingRetest>>, ApiError> {
    let items = state
        .manager
        .list_mission_finding_retests(&mission_id)?
        .into_iter()
        .filter(|retest| retest.finding_id.as_str() == finding_id)
        .collect();
    Ok(Json(items))
}

/// `GET /missions/{mission_id}/retests/active`。
///
/// 未收口复测（前端轮询"复测中"角标用；按 Mission 收窄）。
async fn list_active_mission_retests(
    State(state): State<ApiState>,
    Path(mission_id): Path<String>,
) -> Result<Json<Vec<models::FindingRetest>>, ApiError> {
    Ok(Json(
        state.manager.list_open_mission_finding_retests(&mission_id)?,
    ))
}

/// `POST /missions/{mission_id}/findings/{finding_id}/retests`。
///
/// 发起一次漏洞复测。同步等待顾问
/// worker 返回后落库结论——复测是一次性评估，不是长任务，没必要让前端
/// 再轮一次。
///
/// 已存在未收口复测时返回 200 + 已有记录（幂等），不并发拉起第二个 worker。
async fn start_mission_finding_retest(
    State(state): State<ApiState>,
    Path((mission_id, finding_id)): Path<(String, String)>,
    Json(body): Json<StartRetestRequest>,
) -> Result<(StatusCode, Json<models::FindingRetest>), ApiError> {
    let notes = body.notes.trim();
    if notes.chars().count() > 4000 {
        return Err(ApiError(EngineError::Value(
            "retest notes must contain at most 4000 characters".to_string(),
        )));
    }
    let created = state
        .manager
        .start_finding_retest(&mission_id, &finding_id, notes)?;
    if !created.status.is_open() || created.started_at.is_some() {
        // 已有未收口复测：幂等返回，不重复跑。
        return Ok((StatusCode::OK, Json(created)));
    }
    let mission = state
        .manager
        .repository()
        .get_mission(&mission_id)?
        .ok_or_else(|| EngineError::MissionNotFound(format!("unknown mission: {mission_id}")))?;
    let finding = state
        .manager
        .repository()
        .get_finding(&finding_id)?
        .ok_or_else(|| EngineError::FindingNotFound(format!("unknown finding: {finding_id}")))?;

    let retest = state
        .manager
        .mark_finding_retest_running(created.id.as_str(), None)?;
    let service = crate::intake::IntakeService::new(
        Arc::clone(&state.manager),
        state.mission_workspace_root.clone(),
    );
    // 只读上下文：直接从仓储把漏洞 + 证据 + 事实 + 分支 + 工具调用读出来。
    // 不复用 advise 的 MCP 三件套——那会让复测把结论写回任务白板。
    let context = crate::retest_context::build_retest_context(
        state.manager.repository().as_ref(),
        mission.project_id.as_str(),
        &finding,
    );
    let question = if crate::retest_context::context_has_no_evidence(&context) {
        // 空证据下的 verdict 必然是编造：直接判失败，不劳烦 worker。
        let finished = state
            .manager
            .finish_finding_retest(
                retest.id.as_str(),
                models::RetestStatus::Failed,
                None,
                "",
                "",
                "复测中止：该漏洞没有任何已登记证据，无法在不编造结论的前提下评估。",
                Some("none"),
            )
            .await?;
        return Ok((StatusCode::ACCEPTED, Json(finished)));
    } else {
        retest_prompt(&finding, &retest.notes)
    };
    let outcome = service
        .assess_readonly(&mission, &question, &crate::retest_context::render_retest_context(&context))
        .await;
    let retest = match outcome {
        Ok(assessment) => {
            let conclusion = parse_retest_conclusion(&assessment.answer);
            // 上下文来源如实记录：只拿到内联快照的结论置信度不同。
            let context_source = if assessment.had_whiteboard {
                "readonly_mcp_grant+inline_snapshot"
            } else {
                "inline_snapshot_only"
            };
            state
                .manager
                .finish_finding_retest(
                    retest.id.as_str(),
                    models::RetestStatus::Completed,
                    conclusion.verdict,
                    &conclusion.summary,
                    &conclusion.evidence,
                    &assessment.answer,
                    Some(context_source),
                )
                .await?
        }
        Err(error) => {
            let reason = crate::intake::map_intake_error(error).0.to_string();
            state
                .manager
                .finish_finding_retest(
                    retest.id.as_str(),
                    models::RetestStatus::Failed,
                    None,
                    "",
                    "",
                    &reason,
                    Some("inline_snapshot_only"),
                )
                .await?
        }
    };
    Ok((StatusCode::ACCEPTED, Json(retest)))
}

/// 复测提示词：三段式（漏洞快照 / 只读检查范围 / 期望输出格式）。
fn retest_prompt(finding: &models::Finding, notes: &str) -> String {
    let mut lines = vec![
        "【漏洞复测】请基于任务白板与知识库中已记录的原始证据，重新评估这个漏洞当前是否仍然成立。".to_string(),
        format!("漏洞标题：{}", finding.title),
        format!("严重度：{}", finding.severity.as_str()),
        format!("当前状态：{}", finding.status.as_str()),
    ];
    if let Some(description) = finding.description.as_deref()
        && !description.trim().is_empty()
    {
        lines.push(format!("原始描述：{}", description.trim()));
    }
    if !finding.evidence_ids.is_empty() {
        lines.push(format!(
            "已登记证据：{}",
            finding.evidence_ids.join(", ")
        ));
    }
    if !notes.trim().is_empty() {
        lines.push(format!("补充说明：{}", notes.trim()));
    }
    lines.push(
        "请按下面三行输出，第一行必须是结论标记：\n\
         VERDICT: reproduced|fixed|inconclusive\n\
         SUMMARY: 一句话结论\n\
         EVIDENCE: 你实际检查了哪些证据/路径，为什么得出上面的结论"
            .to_string(),
    );
    lines.join("\n")
}

/// 从顾问回答里解析出的结论。
struct RetestConclusion {
    /// 结论；解析不出为 `None`（此时不会改写漏洞状态）。
    verdict: Option<models::RetestVerdict>,
    /// 摘要。
    summary: String,
    /// 依据。
    evidence: String,
}

/// 解析顾问回答。
///
/// 只认显式的 `VERDICT:` 行——**猜不出结论就返回 `None`**，绝不为了让
/// 报告好看而挑一个。没有结论时调用方会拿到 `summary`/`evidence` 为空，
/// `finish_finding_retest` 会把复测评成 failed 并保留原始文本。
fn parse_retest_conclusion(answer: &str) -> RetestConclusion {
    let mut verdict = None;
    let mut summary = String::new();
    let mut evidence = String::new();
    for line in answer.lines() {
        // 顾问爱用 Markdown 强调（`**VERDICT:**`）或列表符（`- VERDICT:`），
        // 先把这些装饰剥掉再认前缀。
        let trimmed = line.trim().trim_start_matches(['*', '-', '#', '`', ' ']);
        for (prefix, slot) in [
            ("VERDICT:", 0usize),
            ("SUMMARY:", 1),
            ("EVIDENCE:", 2),
        ] {
            if let Some(rest) = trimmed.strip_prefix(prefix) {
                let value = rest.trim().trim_matches(['*', '`']);
                match slot {
                    0 => {
                        verdict = models::RetestVerdict::from_wire(
                            value.split_whitespace().next().unwrap_or("").to_lowercase().as_str(),
                        );
                    }
                    1 => summary = value.to_string(),
                    _ => evidence = value.to_string(),
                }
            }
        }
    }
    RetestConclusion {
        verdict,
        summary,
        evidence,
    }
}

#[cfg(test)]
mod retest_prompt_tests {
    use super::*;

    fn finding(title: &str) -> models::Finding {
        serde_json::from_value(serde_json::json!({
            "id": "find_eval",
            "project_id": "proj_x",
            "title": title,
            "description": "user input reaches eval",
            "severity": "high",
            "status": "confirmed",
            "evidence_ids": ["evd_1", "evd_2"],
            "created_at": "2026-08-24T12:00:00.123456Z",
            "updated_at": "2026-08-24T12:00:00.123456Z",
        }))
        .expect("Finding fixture must parse")
    }

    #[test]
    fn prompt_carries_the_finding_snapshot_and_demands_machine_readable_lines() {
        let prompt = retest_prompt(&finding("Eval injection"), "  用原账号再打一次  ");
        assert!(prompt.contains("漏洞标题：Eval injection"));
        assert!(prompt.contains("严重度：high"));
        assert!(prompt.contains("当前状态：confirmed"));
        assert!(prompt.contains("原始描述：user input reaches eval"));
        assert!(prompt.contains("已登记证据：evd_1, evd_2"));
        // 补充说明前后空白要裁掉。
        assert!(prompt.contains("补充说明：用原账号再打一次"));
        assert!(!prompt.contains("  用原账号"));
        // 期望输出格式必须写清楚，否则顾问给自由文本就没法解析。
        assert!(prompt.contains("VERDICT: reproduced|fixed|inconclusive"));
        assert!(prompt.contains("SUMMARY:"));
        assert!(prompt.contains("EVIDENCE:"));
    }

    #[test]
    fn prompt_omits_empty_sections() {
        let bare = serde_json::from_value::<models::Finding>(serde_json::json!({
            "id": "find_bare",
            "project_id": "proj_x",
            "title": "bare",
            "severity": "info",
            "status": "candidate",
            "created_at": "2026-08-24T12:00:00.123456Z",
            "updated_at": "2026-08-24T12:00:00.123456Z",
        }))
        .expect("Finding fixture must parse");
        let prompt = retest_prompt(&bare, "");
        assert!(!prompt.contains("原始描述："));
        assert!(!prompt.contains("已登记证据："));
        assert!(!prompt.contains("补充说明："));
    }

    #[test]
    fn parses_the_three_marker_lines() {
        let parsed = parse_retest_conclusion(
            "我先读了白板。\n**VERDICT:** fixed\nSUMMARY: 修复版本已部署，payload 不再执行\nEVIDENCE: 复测了 /api/v1/user，返回 400\n",
        );
        assert_eq!(parsed.verdict, Some(models::RetestVerdict::Fixed));
        assert_eq!(parsed.summary, "修复版本已部署，payload 不再执行");
        assert_eq!(parsed.evidence, "复测了 /api/v1/user，返回 400");
    }

    #[test]
    fn verdict_is_case_insensitive_and_takes_the_first_token() {
        let parsed = parse_retest_conclusion("VERDICT: REPRODUCED (still exploitable)\n");
        assert_eq!(parsed.verdict, Some(models::RetestVerdict::Reproduced));
        let noisy = parse_retest_conclusion("VERDICT: FIXED — verified\n");
        assert_eq!(noisy.verdict, Some(models::RetestVerdict::Fixed));
    }

    #[test]
    fn unknown_verdict_yields_none_rather_than_a_guess() {
        let parsed = parse_retest_conclusion("VERDICT: maybe-fixed\nSUMMARY: 不确定\nEVIDENCE: 没看到新证据\n");
        assert_eq!(parsed.verdict, None, "猜不出结论必须返回 None，不能挑一个顺眼的");
        // summary/evidence 仍然保留，调用方据此判 failed 并展示原文。
        assert_eq!(parsed.summary, "不确定");
    }

    #[test]
    fn free_form_answer_without_markers_yields_nothing() {
        let parsed = parse_retest_conclusion("我看了一下，好像修好了。\n");
        assert_eq!(parsed.verdict, None);
        assert!(parsed.summary.is_empty());
        assert!(parsed.evidence.is_empty());
    }
}

/// 运营者中断请求：给正在运行的 worker 注入一条指令（Ctrl+C 再输入）。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MissionInterruptRequest {
    /// 注入消息（非空；有界化在 manager 层按字符数记账）。
    message: String,
}

/// 中断响应：`status` 取 `interrupted` / `nothing_running` / `not_inflight`。
#[derive(Debug, Serialize)]
struct MissionInterruptResponse {
    /// 结局（wire 值）。
    status: String,
    /// 被打断的 worker run id（仅 `interrupted`）。
    #[serde(skip_serializing_if = "Option::is_none")]
    worker_run_id: Option<String>,
    /// 该 worker 的 runtime wire 值（仅 `interrupted`）。
    #[serde(skip_serializing_if = "Option::is_none")]
    runtime: Option<String>,
    /// 续跑是否保留会话记忆（true=resume；false=全量重发）。仅
    /// `interrupted` 返回——让前端能如实告诉用户续跑方式。
    #[serde(skip_serializing_if = "Option::is_none")]
    session_known: Option<bool>,
}

/// 中断前探询：该 mission 正在跑的 worker 有没有已上报的会话引用
/// （决定续跑是 resume 保留记忆还是 fresh start 全量重发）。
async fn interrupt_session_known(state: &ApiState, mission_id: &models::MissionId) -> Option<bool> {
    let selector = state.manager.worker_runtime()?;
    let mission = state
        .manager
        .repository()
        .get_mission(mission_id.as_str())
        .ok()
        .flatten()?;
    let run_id = mission.active_run_id.as_ref()?.as_str();
    let worker_run_id = state
        .manager
        .repository()
        .list_worker_runs(Some(mission.project_id.as_str()), Some(run_id), None, 50)
        .ok()?
        .into_iter()
        .find(|run| run.status == models::worker::WorkerRunStatus::Running)
        .map(|run| run.id)?;
    Some(
        selector
            .worker_session_ref(&worker_run_id)
            .await
            .is_some(),
    )
}

/// POST /missions/{id}/interrupt：中断正在跑的外部 worker 并注入消息。
///
/// 取消信号立即发；消息入注入表，派发层随 `cancelled` 结局取走并以
/// resume / fresh start 续跑（中断的是一次执行，不是任务）。
async fn interrupt_mission_worker(
    State(state): State<ApiState>,
    Path(mission_id): Path<String>,
    Json(body): Json<MissionInterruptRequest>,
) -> Result<(StatusCode, Json<MissionInterruptResponse>), ApiError> {
    let message = body.message.trim().to_string();
    if message.is_empty() {
        return Err(ApiError(EngineError::Value(
            "interrupt message must not be empty".to_string(),
        )));
    }
    if message.chars().count() > 4000 {
        return Err(ApiError(EngineError::Value(
            "interrupt message must be at most 4000 characters".to_string(),
        )));
    }
    let mission_id = models::MissionId::new(mission_id);
    // 先问 sink 有没有会话引用（中断前问，取消后 worker 已死、语义更清晰）。
    let session_known = interrupt_session_known(&state, &mission_id).await;
    let outcome = state
        .manager
        .interrupt_mission_worker(&mission_id, &message)
        .await?;
    let (status, response) = match outcome {
        runtime::MissionInterruptOutcome::Interrupted {
            worker_run_id,
            runtime,
            ..
        } => (
            StatusCode::ACCEPTED,
            MissionInterruptResponse {
                status: "interrupted".to_string(),
                worker_run_id: Some(worker_run_id),
                runtime: Some(runtime),
                session_known,
            },
        ),
        runtime::MissionInterruptOutcome::NothingRunning => (
            StatusCode::CONFLICT,
            MissionInterruptResponse {
                status: "nothing_running".to_string(),
                worker_run_id: None,
                runtime: None,
                session_known: None,
            },
        ),
        runtime::MissionInterruptOutcome::NotInflight => (
            StatusCode::CONFLICT,
            MissionInterruptResponse {
                status: "not_inflight".to_string(),
                worker_run_id: None,
                runtime: None,
                session_known: None,
            },
        ),
    };
    Ok((status, Json(response)))
}

async fn post_mission_signal(    State(state): State<ApiState>,
    Path(mission_id): Path<String>,
    Json(mut body): Json<MissionSignalRequest>,
) -> Result<Json<MissionSignalResponse>, ApiError> {
    if body.content.trim().is_empty() {
        return Err(ApiError(EngineError::Value(
            "content must not be blank".to_string(),
        )));
    }
    if body.signal_type.requires_branch() && body.branch_id.is_none() {
        return Err(ApiError(EngineError::Value(format!(
            "signal '{}' requires a branch_id",
            body.signal_type.as_str()
        ))));
    }
    body.metadata.insert(
        "signal_type".to_string(),
        Value::String(body.signal_type.as_str().to_string()),
    );
    let branch_id = body
        .branch_id
        .as_deref()
        .map(|value| models::BranchId::new(value.to_string()));
    let signal_type = body.signal_type;
    let directive = state
        .manager
        .apply_user_directive(
            &MissionId::new(mission_id.clone()),
            signal_type.directive_type(),
            &body.content,
            branch_id.as_ref(),
            &body.created_by,
            Some(body.metadata),
        )
        .await?;
    let note = matches!(
        signal_type,
        MissionSignalType::Confirm | MissionSignalType::Reject | MissionSignalType::InjectHint
    )
    .then(|| "signal recorded as user directive".to_string());
    Ok(Json(MissionSignalResponse {
        accepted: true,
        signal_type,
        directive_id: Some(directive.id.as_str().to_string()),
        mission_id,
        note,
    }))
}

#[derive(Debug, Deserialize, Default)]
struct DecisionGateListQuery {
    project_id: Option<String>,
    audit_run_id: Option<String>,
}

async fn list_decision_gates(
    State(state): State<ApiState>,
    Query(query): Query<DecisionGateListQuery>,
) -> Result<Json<Vec<models::DecisionGate>>, ApiError> {
    Ok(Json(state.manager.repository().list_decision_gates(
        query.project_id.as_deref(),
        query.audit_run_id.as_deref(),
    )?))
}

async fn get_decision_gate(
    State(state): State<ApiState>,
    Path(gate_id): Path<String>,
) -> Result<Json<models::DecisionGate>, ApiError> {
    let gate = state
        .manager
        .repository()
        .get_decision_gate(&gate_id)?
        .ok_or_else(|| EngineError::DecisionGateNotFound(gate_id.clone()))?;
    Ok(Json(gate))
}

async fn answer_decision_gate(
    State(state): State<ApiState>,
    Path(gate_id): Path<String>,
    Json(body): Json<AnswerDecisionGateRequest>,
) -> Result<Json<models::DecisionGate>, ApiError> {
    Ok(Json(
        state
            .manager
            .answer_decision_gate(
                &gate_id,
                DecisionAnswer {
                    option_id: body.option_id,
                    freeform_text: body.freeform_text,
                    answered_by: body.answered_by,
                    rationale: body.rationale,
                },
            )
            .await?,
    ))
}

async fn cancel_decision_gate(
    State(state): State<ApiState>,
    Path(gate_id): Path<String>,
    Json(body): Json<CancelDecisionGateRequest>,
) -> Result<Json<models::DecisionGate>, ApiError> {
    Ok(Json(
        state
            .manager
            .cancel_decision_gate(&gate_id, &body.cancelled_by, body.rationale)
            .await?,
    ))
}

async fn resume_audit_run(
    State(state): State<ApiState>,
    Path((project_id, run_id)): Path<(String, String)>,
) -> Result<Json<models::AuditRun>, ApiError> {
    let project = state
        .manager
        .repository()
        .get_project(&project_id)?
        .ok_or_else(|| EngineError::ProjectNotFound(format!("unknown project: {project_id}")))?;
    let run = state
        .manager
        .repository()
        .get_run(&run_id)?
        .ok_or_else(|| EngineError::RunNotFound(format!("unknown run: {run_id}")))?;
    if run.project_id != project.id {
        return Err(ApiError(EngineError::Value(format!(
            "run {run_id} does not belong to project {project_id}"
        ))));
    }
    // The run-level resume operation is deliberately separate from Mission
    // resume: it only clears a decision wait after all blocking gates resolve.
    Ok(Json(state.manager.resume_run(&run_id).await?))
}

async fn mission_timeline(
    State(state): State<ApiState>,
    Path(mission_id): Path<String>,
) -> Result<Json<Vec<models::AuditEvent>>, ApiError> {
    let mission = state
        .manager
        .repository()
        .get_mission(&mission_id)?
        .ok_or_else(|| EngineError::MissionNotFound(format!("unknown mission: {mission_id}")))?;
    let events = state.manager.repository().list_events(
        mission.project_id.as_str(),
        mission.active_run_id.as_ref().map(models::RunId::as_str),
        500,
        None,
    )?;
    let filtered = events
        .into_iter()
        .filter(|event| {
            event.run_id == mission.active_run_id
                || event
                    .data
                    .get("mission_id")
                    .and_then(Value::as_str)
                    .is_none_or(|id| id == mission.id.as_str())
        })
        .collect();
    Ok(Json(filtered))
}

#[derive(Debug, Deserialize, Default)]
struct OperationLogQuery {
    #[serde(default)]
    after_sequence: i64,
    #[serde(default = "default_operation_log_limit")]
    limit: i64,
    run_id: Option<String>,
    branch_id: Option<String>,
    task_id: Option<String>,
    worker_id: Option<String>,
    operation_type: Option<String>,
}

fn default_operation_log_limit() -> i64 {
    200
}

async fn mission_operation_log(
    State(state): State<ApiState>,
    Path(mission_id): Path<String>,
    Query(query): Query<OperationLogQuery>,
) -> Result<Json<storage::SwarmOperationPage>, ApiError> {
    let mission = state
        .manager
        .repository()
        .get_mission(&mission_id)?
        .ok_or_else(|| EngineError::MissionNotFound(format!("unknown mission: {mission_id}")))?;
    let log_dir = if let Some(run_id) = mission.active_run_id.as_ref() {
        state
            .manager
            .repository()
            .get_run(run_id.as_str())?
            .and_then(|run| {
                run.config
                    .get("log_dir")
                    .and_then(Value::as_str)
                    .map(PathBuf::from)
            })
    } else {
        None
    };
    let Some(log_dir) = log_dir else {
        return Ok(Json(storage::SwarmOperationPage::default()));
    };
    let page = state
        .manager
        .operation_journal()
        .read_page(
            &log_dir,
            storage::PageQuery {
                after_sequence: query.after_sequence.max(0),
                limit: query.limit.clamp(1, 1_000),
                run_id: query.run_id,
                branch_id: query.branch_id,
                task_id: query.task_id,
                worker_id: query.worker_id,
                operation_type: query.operation_type,
                tail: false,
            },
        )
        .map_err(|error| ApiError(EngineError::Value(error.to_string())))?;
    Ok(Json(page))
}

/// 探索链路节点（探索链路）：kind = begin/goal/fact/intent/hint/finding。
#[derive(Debug, serde::Serialize)]
struct ExplorationNode {
    id: String,
    kind: &'static str,
    title: String,
    summary: String,
    state: String,
    priority: i64,
    origin: String,
    ts: String,
}

/// 探索链路边：rel = spawns/derived_from/yields/proves。
#[derive(Debug, serde::Serialize)]
struct ExplorationEdge {
    src: String,
    dst: String,
    rel: &'static str,
}

/// `GET /missions/{mission_id}/exploration-graph` —— 把 mission 的
/// facts/intents/hints/findings/evidence 投影成探索链路图：
///
/// - 节点六类：OriginFact=起点、GoalFact=目标、Fact/Evidence=事实、
///   Intent=意图、Hint=提示、Finding=漏洞；
/// - 边全部来自真实存储关系：`derived_from`（fact 产出链 + intent 的
///   source_fact_ids）、`proves`（evidence 证明 fact/finding）、
///   `yields`（intent 认领的 task 产出的 evidence）、`spawns`
///   （mission 播种的 origin → goal）。
#[derive(Debug, serde::Serialize)]
struct ExplorationGraph {
    nodes: Vec<ExplorationNode>,
    edges: Vec<ExplorationEdge>,
}

async fn mission_exploration_graph(
    State(state): State<ApiState>,
    Path(mission_id): Path<String>,
) -> Result<Json<ExplorationGraph>, ApiError> {
    use models::fact::GraphNodeType;
    let mission = state
        .manager
        .repository()
        .get_mission(&mission_id)?
        .ok_or_else(|| EngineError::MissionNotFound(format!("unknown mission: {mission_id}")))?;
    let project_id = mission.project_id.as_str();
    let repository = state.manager.repository();

    let facts = repository.list_facts(project_id)?;
    let intents = repository.list_intents(project_id)?;
    let hints = repository.list_hints(project_id)?;
    let evidence_all = repository.list_evidence(project_id)?;
    let findings_all = repository.list_findings(project_id)?;

    let mission_scoped = |owner: &Option<models::MissionId>| -> bool {
        owner.as_ref().is_some_and(|id| id.as_str() == mission_id)
    };

    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let mut node_ids = std::collections::HashSet::new();

    let mut push_node = |node: ExplorationNode, ids: &mut std::collections::HashSet<String>| {
        ids.insert(node.id.clone());
        node
    };

    // ── facts：起点 / 目标 / 事实 ──
    let mission_facts: Vec<&models::Fact> = facts
        .iter()
        .filter(|fact| mission_scoped(&fact.mission_id))
        .collect();
    let mut origin_ids = Vec::new();
    let mut goal_ids = Vec::new();
    for fact in &mission_facts {
        let kind = match fact.node_type {
            GraphNodeType::OriginFact => {
                origin_ids.push(fact.id.as_str().to_string());
                "begin"
            }
            GraphNodeType::GoalFact => {
                goal_ids.push(fact.id.as_str().to_string());
                "goal"
            }
            _ => "fact",
        };
        let node = push_node(
            ExplorationNode {
                id: fact.id.as_str().to_string(),
                kind,
                title: fact.kind.clone(),
                summary: fact.statement.clone(),
                state: String::new(),
                priority: 0,
                origin: fact
                    .produced_by_task_id
                    .as_ref()
                    .map(|id| id.as_str().to_string())
                    .unwrap_or_else(|| "mission_seed".to_string()),
                ts: fact.created_at.isoformat(),
            },
            &mut node_ids,
        );
        nodes.push(node);
    }

    // ── intents：意图（含 source_fact_ids 意图链边）──
    let mission_intents: Vec<&models::Intent> = intents
        .iter()
        .filter(|intent| mission_scoped(&intent.mission_id))
        .collect();
    let mut intent_task_ids = std::collections::HashMap::new();
    for intent in &mission_intents {
        if let Some(task_id) = intent.claimed_by_task_id.as_ref() {
            intent_task_ids.insert(task_id.as_str().to_string(), intent.id.as_str().to_string());
        }
        for fact_id in &intent.source_fact_ids {
            if node_ids.contains(fact_id) {
                edges.push(ExplorationEdge {
                    src: fact_id.clone(),
                    dst: intent.id.as_str().to_string(),
                    rel: "derived_from",
                });
            }
        }
        let node = push_node(
            ExplorationNode {
                id: intent.id.as_str().to_string(),
                kind: "intent",
                title: intent.title.clone(),
                summary: intent.description.clone().unwrap_or_default(),
                state: intent.status.as_str().to_string(),
                priority: intent.priority,
                origin: intent.created_by.clone(),
                ts: intent.created_at.isoformat(),
            },
            &mut node_ids,
        );
        nodes.push(node);
    }

    // ── hints：提示（project 级；取最近 30 条避免淹没图面）──
    for hint in hints.iter().rev().take(30) {
        let node = push_node(
            ExplorationNode {
                id: hint.id.clone(),
                kind: "hint",
                title: hint
                    .category
                    .clone()
                    .unwrap_or_else(|| "提示".to_string()),
                summary: hint.text.clone(),
                state: "active".to_string(),
                priority: i64::from(hint.weight),
                origin: "operator".to_string(),
                ts: hint.created_at.isoformat(),
            },
            &mut node_ids,
        );
        nodes.push(node);
    }

    // ── findings：漏洞（mission scope；同时收集其 evidence 引用）──
    let mission_findings: Vec<&models::Finding> = findings_all
        .iter()
        .filter(|finding| mission_scoped(&finding.mission_id))
        .collect();
    let mut evidence_needed = std::collections::HashSet::new();
    for finding in &mission_findings {
        evidence_needed.extend(finding.evidence_ids.iter().cloned());
        let node = push_node(
            ExplorationNode {
                id: finding.id.as_str().to_string(),
                kind: "finding",
                title: finding.title.clone(),
                summary: finding.description.clone().unwrap_or_default(),
                state: finding.status.as_str().to_string(),
                priority: match finding.severity {
                    models::Severity::Critical => 10,
                    models::Severity::High => 8,
                    models::Severity::Medium => 5,
                    models::Severity::Low => 3,
                    models::Severity::Info => 1,
                },
                origin: finding
                    .produced_by_task_id
                    .as_ref()
                    .map(|id| id.as_str().to_string())
                    .unwrap_or_else(|| "solver".to_string()),
                ts: finding.created_at.isoformat(),
            },
            &mut node_ids,
        );
        nodes.push(node);
    }

    // ── evidence：事实/证明（mission scope 或被 mission findings 引用；
    //    intent 认领 task 的产出记 yields 边）──
    for ev in &evidence_all {
        let in_mission = mission_scoped(&ev.mission_id)
            || ev.supports_fact_ids.iter().any(|id| node_ids.contains(id))
            || evidence_needed.contains(ev.id.as_str());
        if !in_mission {
            continue;
        }
        for fact_id in &ev.supports_fact_ids {
            if node_ids.contains(fact_id) {
                edges.push(ExplorationEdge {
                    src: ev.id.as_str().to_string(),
                    dst: fact_id.clone(),
                    rel: "proves",
                });
            }
        }
        for finding in &mission_findings {
            if finding.evidence_ids.iter().any(|id| id == ev.id.as_str()) {
                edges.push(ExplorationEdge {
                    src: ev.id.as_str().to_string(),
                    dst: finding.id.as_str().to_string(),
                    rel: "proves",
                });
            }
        }
        if let Some(intent_id) = ev
            .produced_by_task_id
            .as_ref()
            .and_then(|task_id| intent_task_ids.get(task_id.as_str()))
            .cloned()
        {
            edges.push(ExplorationEdge {
                src: intent_id,
                dst: ev.id.as_str().to_string(),
                rel: "yields",
            });
        }
        let node = push_node(
            ExplorationNode {
                id: ev.id.as_str().to_string(),
                kind: "fact",
                title: "证据".to_string(),
                summary: ev.summary.clone(),
                state: String::new(),
                priority: 0,
                origin: ev
                    .produced_by_task_id
                    .as_ref()
                    .map(|id| id.as_str().to_string())
                    .unwrap_or_else(|| "evidence".to_string()),
                ts: ev.created_at.isoformat(),
            },
            &mut node_ids,
        );
        nodes.push(node);
    }

    // ── fact 产出链：derived_from（fact/intent/evidence → fact）──
    for fact in &mission_facts {
        for source in &fact.derived_from {
            if node_ids.contains(source) {
                edges.push(ExplorationEdge {
                    src: source.clone(),
                    dst: fact.id.as_str().to_string(),
                    rel: "derived_from",
                });
            }
        }
    }

    // ── spawns：mission 播种的 origin → goal ──
    for origin in &origin_ids {
        for goal in &goal_ids {
            edges.push(ExplorationEdge {
                src: origin.clone(),
                dst: goal.clone(),
                rel: "spawns",
            });
        }
    }

    Ok(Json(ExplorationGraph { nodes, edges }))
}

async fn mission_graph(
    State(state): State<ApiState>,
    Path(mission_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let mission = state
        .manager
        .repository()
        .get_mission(&mission_id)?
        .ok_or_else(|| EngineError::MissionNotFound(format!("unknown mission: {mission_id}")))?;
    let project_id = mission.project_id.as_str();
    let facts = state.manager.repository().list_facts(project_id)?;
    let intents = state.manager.repository().list_intents(project_id)?;
    let evidence = state.manager.repository().list_evidence(project_id)?;
    let findings = state.manager.repository().list_findings(project_id)?;
    let runs = state.manager.repository().list_runs(project_id)?;
    let mut edges = Vec::new();
    for item in &evidence {
        for fact_id in &item.supports_fact_ids {
            edges.push(serde_json::json!({
                "src": item.id,
                "dst": fact_id,
                "relation": "supports"
            }));
        }
    }
    for item in &findings {
        for evidence_id in &item.evidence_ids {
            edges.push(serde_json::json!({
                "src": item.id,
                "dst": evidence_id,
                "relation": "evidences"
            }));
        }
    }
    Ok(Json(serde_json::json!({
        "mission": mission,
        "project_graph": {
            "project_id": project_id,
            "facts": facts,
            "intents": intents,
            "evidence": evidence,
            "findings": findings,
            "runs": runs,
            "edges": edges
        }
    })))
}

/// GET /projects/{project_id}/worker-usage —— worker 用量聚合。
async fn project_worker_usage(
    State(state): State<ApiState>,
    Path(project_id): Path<String>,
) -> Result<Json<models::WorkerUsageSummary>, ApiError> {
    let summary = state.manager.repository().sum_worker_usage(Some(&project_id))?;
    Ok(Json(summary))
}

async fn list_project_runs(
    State(state): State<ApiState>,
    Path(project_id): Path<String>,
) -> Result<Json<Vec<models::AuditRun>>, ApiError> {
    state
        .manager
        .repository()
        .get_project(&project_id)?
        .ok_or_else(|| EngineError::ProjectNotFound(format!("unknown project: {project_id}")))?;
    Ok(Json(state.manager.repository().list_runs(&project_id)?))
}

async fn list_run_decision_gates(
    State(state): State<ApiState>,
    Path((project_id, run_id)): Path<(String, String)>,
) -> Result<Json<Vec<models::DecisionGate>>, ApiError> {
    let project = state
        .manager
        .repository()
        .get_project(&project_id)?
        .ok_or_else(|| EngineError::ProjectNotFound(format!("unknown project: {project_id}")))?;
    let run = state
        .manager
        .repository()
        .get_run(&run_id)?
        .ok_or_else(|| EngineError::RunNotFound(format!("unknown run: {run_id}")))?;
    if run.project_id != project.id {
        return Err(ApiError(EngineError::Value(format!(
            "run {run_id} does not belong to project {project_id}"
        ))));
    }
    Ok(Json(
        state
            .manager
            .repository()
            .list_decision_gates(Some(&project_id), Some(&run_id))?,
    ))
}

#[derive(Debug, Deserialize)]
struct ProjectEventsQuery {
    run_id: Option<String>,
    #[serde(default = "default_event_limit")]
    limit: i64,
    after_id: Option<String>,
}

fn default_event_limit() -> i64 {
    200
}

async fn list_project_events(
    State(state): State<ApiState>,
    Path(project_id): Path<String>,
    Query(query): Query<ProjectEventsQuery>,
) -> Result<Json<Vec<models::AuditEvent>>, ApiError> {
    state
        .manager
        .repository()
        .get_project(&project_id)?
        .ok_or_else(|| EngineError::ProjectNotFound(format!("unknown project: {project_id}")))?;
    Ok(Json(state.manager.repository().list_events(
        &project_id,
        query.run_id.as_deref(),
        query.limit,
        query.after_id.as_deref(),
    )?))
}

async fn list_project_decision_gates(
    State(state): State<ApiState>,
    Path(project_id): Path<String>,
) -> Result<Json<Vec<models::DecisionGate>>, ApiError> {
    state
        .manager
        .repository()
        .get_project(&project_id)?
        .ok_or_else(|| EngineError::ProjectNotFound(format!("unknown project: {project_id}")))?;
    Ok(Json(
        state
            .manager
            .repository()
            .list_decision_gates(Some(&project_id), None)?,
    ))
}

async fn list_project_termination_assessments(
    State(state): State<ApiState>,
    Path(project_id): Path<String>,
) -> Result<Json<Vec<models::TerminationAssessment>>, ApiError> {
    state
        .manager
        .repository()
        .get_project(&project_id)?
        .ok_or_else(|| EngineError::ProjectNotFound(format!("unknown project: {project_id}")))?;
    Ok(Json(
        state
            .manager
            .repository()
            .list_termination_assessments(&project_id, None)?,
    ))
}

async fn list_project_worker_leases(
    State(state): State<ApiState>,
    Path(project_id): Path<String>,
) -> Result<Json<Vec<models::WorkerLease>>, ApiError> {
    state
        .manager
        .repository()
        .get_project(&project_id)?
        .ok_or_else(|| EngineError::ProjectNotFound(format!("unknown project: {project_id}")))?;
    Ok(Json(
        state
            .manager
            .repository()
            .list_worker_leases(&project_id, None)?,
    ))
}

async fn list_project_reflector_reports(
    State(state): State<ApiState>,
    Path(project_id): Path<String>,
) -> Result<Json<Vec<models::ReflectorReport>>, ApiError> {
    state
        .manager
        .repository()
        .get_project(&project_id)?
        .ok_or_else(|| EngineError::ProjectNotFound(format!("unknown project: {project_id}")))?;
    Ok(Json(
        state
            .manager
            .repository()
            .list_reflector_reports(&project_id, None)?,
    ))
}

fn hex_prefix(bytes: &[u8], byte_limit: usize) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(byte_limit.saturating_mul(2));
    for byte in bytes.iter().take(byte_limit) {
        output.push(HEX[usize::from(byte >> 4)] as char);
        output.push(HEX[usize::from(byte & 0x0f)] as char);
    }
    output
}

fn model_capability_id(provider_id: &str, model: &str) -> String {
    let digest = Sha256::digest(format!("{provider_id}:{model}").as_bytes());
    format!("modelcap_{}", hex_prefix(&digest, 8))
}

#[derive(Debug, Deserialize, Default)]
struct ArtifactScopeQuery {
    project_id: Option<String>,
    run_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AddArtifactRequest {
    #[serde(default)]
    project_id: Option<String>,
    #[serde(default)]
    run_id: Option<String>,
    #[serde(default)]
    task_id: Option<String>,
    #[serde(default)]
    tool_invocation_id: Option<String>,
    #[serde(default)]
    model_invocation_id: Option<String>,
    #[serde(default)]
    evidence_id: Option<String>,
    #[serde(default)]
    finding_id: Option<String>,
    #[serde(default = "default_artifact_kind")]
    kind: models::ArtifactKind,
    uri: String,
    #[serde(default = "default_storage_backend")]
    storage_backend: String,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    mime_type: Option<String>,
    #[serde(default)]
    size_bytes: Option<i64>,
    #[serde(default)]
    sha256: Option<String>,
    #[serde(default)]
    metadata: Map<String, Value>,
}

fn default_artifact_kind() -> models::ArtifactKind {
    models::ArtifactKind::Other
}

fn default_storage_backend() -> String {
    "local_filesystem".to_string()
}

async fn add_artifact(
    State(state): State<ApiState>,
    Json(body): Json<AddArtifactRequest>,
) -> Result<(StatusCode, Json<models::ArtifactRecord>), ApiError> {
    let mut artifact = models::ArtifactRecord::new(body.uri);
    artifact.project_id = body.project_id.map(ProjectId::new);
    artifact.run_id = body.run_id.map(models::RunId::new);
    artifact.task_id = body.task_id.map(models::TaskId::new);
    artifact.tool_invocation_id = body.tool_invocation_id.map(models::ToolInvocationId::new);
    artifact.model_invocation_id = body.model_invocation_id.map(models::ModelInvocationId::new);
    artifact.evidence_id = body.evidence_id.map(models::EvidenceId::new);
    artifact.finding_id = body.finding_id.map(models::FindingId::new);
    artifact.kind = body.kind;
    artifact.storage_backend = body.storage_backend;
    artifact.summary = body.summary;
    artifact.mime_type = body.mime_type;
    artifact.size_bytes = body.size_bytes;
    artifact.sha256 = body.sha256;
    artifact.metadata = body.metadata;
    Ok((
        StatusCode::CREATED,
        Json(state.manager.add_artifact_record(&artifact)?),
    ))
}

async fn list_artifacts(
    State(state): State<ApiState>,
    Query(query): Query<ArtifactScopeQuery>,
) -> Result<Json<Vec<models::ArtifactRecord>>, ApiError> {
    Ok(Json(state.manager.repository().list_artifact_records(
        query.project_id.as_deref(),
        query.run_id.as_deref(),
    )?))
}

#[derive(Debug, Deserialize, Default)]
struct ToolInvocationScopeQuery {
    project_id: Option<String>,
}

async fn list_tool_invocations_global(
    State(state): State<ApiState>,
    Query(query): Query<ToolInvocationScopeQuery>,
) -> Result<Json<Vec<models::ToolInvocation>>, ApiError> {
    Ok(Json(
        state
            .manager
            .repository()
            .list_tool_invocations(query.project_id.as_deref())?,
    ))
}

#[derive(Debug, Deserialize)]
struct AddKnowledgeCardRequest {
    kind: models::KnowledgeCardKind,
    title: String,
    /// 兼容字段：缺省时视为与 `summary` 相同的历史入口。
    #[serde(default)]
    content: Option<String>,
    /// 注入用摘要（推荐；缺省回退 `content`）。
    #[serde(default)]
    summary: Option<String>,
    /// 完整知识正文（Retrieval Substrate；不受 2000 注入约束）。
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    parent_id: Option<String>,
    #[serde(default)]
    aliases: Vec<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    tool: Vec<String>,
    #[serde(default)]
    technique: Vec<String>,
    #[serde(default)]
    platform: Vec<String>,
    #[serde(default)]
    protocol: Vec<String>,
    #[serde(default)]
    prerequisites: Vec<String>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    source_locator: Option<String>,
    #[serde(default = "default_knowledge_priority")]
    priority: i64,
}

fn default_knowledge_priority() -> i64 {
    50
}

#[derive(Debug, Deserialize)]
struct SearchKnowledgeCardsRequest {
    query: models::KnowledgeRetrievalQuery,
}

async fn add_knowledge_card(
    State(state): State<ApiState>,
    Json(body): Json<AddKnowledgeCardRequest>,
) -> Result<(StatusCode, Json<models::KnowledgeCard>), ApiError> {
    // summary 缺省回退 content（老客户端只传 content 时语义不变）。
    let summary = body.summary.clone().or_else(|| body.content.clone());
    let parent_id = body
        .parent_id
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .map(|value| models::KnowledgeCardId::new(value.to_string()));
    Ok((
        StatusCode::CREATED,
        Json(
            state
                .manager
                .add_knowledge_unit(models::KnowledgeCardDraft {
                    kind: body.kind,
                    title: body.title,
                    summary: summary.unwrap_or_default(),
                    body: body.body.unwrap_or_default(),
                    parent_id,
                    aliases: body.aliases,
                    tags: body.tags,
                    tool: body.tool,
                    technique: body.technique,
                    platform: body.platform,
                    protocol: body.protocol,
                    prerequisites: body.prerequisites,
                    source: body.source,
                    source_locator: body.source_locator,
                    priority: body.priority,
                    id: None,
                })?,
        ),
    ))
}

async fn list_knowledge_cards(
    State(state): State<ApiState>,
) -> Result<Json<Vec<models::KnowledgeCard>>, ApiError> {
    Ok(Json(state.manager.list_knowledge_cards()?))
}

/// Search the global tactical knowledge corpus through normalized FTS5/BM25.
///
/// This endpoint does not search Project/Run evidence chunks; callers needing
/// scoped evidence use `/retrieval/search`.
async fn search_knowledge_cards(
    State(state): State<ApiState>,
    Json(body): Json<SearchKnowledgeCardsRequest>,
) -> Result<Json<Vec<models::KnowledgeRetrievalResult>>, ApiError> {
    Ok(Json(state.manager.search_knowledge_cards(&body.query)?))
}

/// 知识语料/FTS 索引状态（empty/ready/stale + 计数 + 上次同步时间）。
async fn knowledge_index_status(
    State(state): State<ApiState>,
) -> Result<Json<models::KnowledgeCorpusStatus>, ApiError> {
    Ok(Json(state.manager.knowledge_corpus_status()?))
}

/// 全量重建知识 FTS 索引（批量导入 / migration / 手动修复后调用）。
async fn knowledge_index_sync(
    State(state): State<ApiState>,
) -> Result<Json<models::KnowledgeCorpusStatus>, ApiError> {
    Ok(Json(state.manager.sync_knowledge_index()?))
}

async fn list_project_findings(
    State(state): State<ApiState>,
    Path(project_id): Path<String>,
) -> Result<Json<Vec<models::Finding>>, ApiError> {
    state
        .manager
        .repository()
        .get_project(&project_id)?
        .ok_or_else(|| EngineError::ProjectNotFound(format!("unknown project: {project_id}")))?;
    Ok(Json(state.manager.repository().list_findings(&project_id)?))
}

#[derive(Debug, Deserialize)]
struct AddFactRequest {
    kind: String,
    statement: String,
    #[serde(default)]
    data: Map<String, Value>,
    #[serde(default)]
    derived_from: Vec<String>,
    #[serde(default = "default_fact_confidence")]
    confidence: f64,
}

fn default_fact_confidence() -> f64 {
    1.0
}

#[derive(Debug, Deserialize)]
struct AddIntentRequest {
    title: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    source_fact_ids: Vec<String>,
    #[serde(default)]
    solver: Option<String>,
    #[serde(default = "default_intent_priority")]
    priority: i64,
}

fn default_intent_priority() -> i64 {
    50
}

#[derive(Debug, Deserialize)]
struct AddHintRequest {
    text: String,
    #[serde(default)]
    category: Option<String>,
    #[serde(default = "default_hint_weight")]
    weight: i64,
}

fn default_hint_weight() -> i64 {
    50
}

#[derive(Debug, Deserialize)]
struct AddEvidenceRequest {
    kind: EvidenceKind,
    summary: String,
    #[serde(default)]
    content: Map<String, Value>,
    #[serde(default)]
    supports_fact_ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct AddFindingRequest {
    title: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default = "default_finding_severity")]
    severity: Severity,
    #[serde(default)]
    cwe: Option<String>,
    #[serde(default)]
    rule_id: Option<String>,
    #[serde(default)]
    evidence_ids: Vec<String>,
    #[serde(default)]
    related_fact_ids: Vec<String>,
    #[serde(default)]
    source_label: Option<String>,
    #[serde(default)]
    sink_label: Option<String>,
}

fn default_finding_severity() -> Severity {
    Severity::Medium
}

async fn add_project_fact(
    State(state): State<ApiState>,
    Path(project_id): Path<String>,
    Json(body): Json<AddFactRequest>,
) -> Result<(StatusCode, Json<models::Fact>), ApiError> {
    let fact = state.manager.add_user_fact(
        &project_id,
        &body.kind,
        &body.statement,
        body.data,
        body.derived_from,
        body.confidence,
    )?;
    Ok((StatusCode::CREATED, Json(fact)))
}

async fn add_project_intent(
    State(state): State<ApiState>,
    Path(project_id): Path<String>,
    Json(body): Json<AddIntentRequest>,
) -> Result<(StatusCode, Json<models::Intent>), ApiError> {
    let intent = state.manager.add_user_intent(
        &project_id,
        &body.title,
        body.description,
        body.source_fact_ids,
        body.solver,
        body.priority,
    )?;
    Ok((StatusCode::CREATED, Json(intent)))
}

async fn add_project_hint(
    State(state): State<ApiState>,
    Path(project_id): Path<String>,
    Json(body): Json<AddHintRequest>,
) -> Result<(StatusCode, Json<models::Hint>), ApiError> {
    let hint = state
        .manager
        .add_user_hint(&project_id, &body.text, body.category, body.weight)?;
    Ok((StatusCode::CREATED, Json(hint)))
}

async fn add_project_evidence(
    State(state): State<ApiState>,
    Path(project_id): Path<String>,
    Json(body): Json<AddEvidenceRequest>,
) -> Result<(StatusCode, Json<models::Evidence>), ApiError> {
    let evidence = state.manager.add_manual_evidence(
        &project_id,
        body.kind,
        &body.summary,
        body.content,
        body.supports_fact_ids,
    )?;
    Ok((StatusCode::CREATED, Json(evidence)))
}

async fn add_project_finding(
    State(state): State<ApiState>,
    Path(project_id): Path<String>,
    Json(body): Json<AddFindingRequest>,
) -> Result<(StatusCode, Json<models::Finding>), ApiError> {
    let finding = state
        .manager
        .add_manual_finding(
            &project_id,
            &body.title,
            body.description,
            body.severity,
            body.cwe,
            body.rule_id,
            body.evidence_ids,
            body.related_fact_ids,
            body.source_label,
            body.sink_label,
        )
        .await?;
    Ok((StatusCode::CREATED, Json(finding)))
}

/// `PATCH /projects/{project_id}/findings/{finding_id}` 的请求体。
///
/// 两个字段都可选，但至少给一个（与漏洞 triage 同一套规则：`None`
/// 表示"不动这个字段"）。
#[derive(Debug, Deserialize)]
struct TriageFindingRequest {
    /// 目标状态 wire 值（`FindingStatus::ALL` 之一）。
    #[serde(default)]
    status: Option<String>,
    /// 目标严重度 wire 值。
    #[serde(default)]
    severity: Option<String>,
}

/// 人工 triage：改写 Finding 状态/严重度。
///
/// 与复测的差别：这是 Operator 的显式结论，直接生效；复测结论
/// （verdict=fixed）也是经由这里同一套写路径落到 `fixed`。
async fn triage_project_finding(
    State(state): State<ApiState>,
    Path((project_id, finding_id)): Path<(String, String)>,
    Json(body): Json<TriageFindingRequest>,
) -> Result<Json<models::Finding>, ApiError> {
    if body.status.is_none() && body.severity.is_none() {
        return Err(ApiError(EngineError::Value(
            "nothing to update: provide status and/or severity".to_string(),
        )));
    }
    let status = match body.status.as_deref() {
        Some(raw) => Some(models::FindingStatus::from_wire(raw).ok_or_else(|| {
            EngineError::Value(format!(
                "bad status: {raw}; expected one of {}",
                models::FindingStatus::ALL
                    .iter()
                    .map(|status| status.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        })?),
        None => None,
    };
    let severity = match body.severity.as_deref() {
        Some(raw) => Some(models::Severity::from_wire(raw).ok_or_else(|| {
            EngineError::Value(format!("bad severity: {raw}"))
        })?),
        None => None,
    };
    state
        .manager
        .repository()
        .get_project(&project_id)?
        .ok_or_else(|| EngineError::ProjectNotFound(format!("unknown project: {project_id}")))?;
    let finding = state
        .manager
        .triage_finding(&finding_id, status, severity)
        .await?;
    if finding.project_id.as_str() != project_id {
        return Err(ApiError(EngineError::FindingNotFound(format!(
            "finding {finding_id} does not belong to project {project_id}"
        ))));
    }
    Ok(Json(finding))
}

#[derive(Debug, Deserialize)]
struct RunFilterQuery {
    run_id: Option<String>,
}

async fn list_project_observations(
    State(state): State<ApiState>,
    Path(project_id): Path<String>,
    Query(query): Query<RunFilterQuery>,
) -> Result<Json<Vec<models::Observation>>, ApiError> {
    state
        .manager
        .repository()
        .get_project(&project_id)?
        .ok_or_else(|| EngineError::ProjectNotFound(format!("unknown project: {project_id}")))?;
    Ok(Json(
        state
            .manager
            .repository()
            .list_observations(&project_id, query.run_id.as_deref())?,
    ))
}

async fn list_project_tool_invocations(
    State(state): State<ApiState>,
    Path(project_id): Path<String>,
) -> Result<Json<Vec<models::ToolInvocation>>, ApiError> {
    state
        .manager
        .repository()
        .get_project(&project_id)?
        .ok_or_else(|| EngineError::ProjectNotFound(format!("unknown project: {project_id}")))?;
    Ok(Json(
        state
            .manager
            .repository()
            .list_tool_invocations(Some(&project_id))?,
    ))
}

#[allow(clippy::too_many_lines)]
async fn project_graph(
    State(state): State<ApiState>,
    Path(project_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    state
        .manager
        .repository()
        .get_project(&project_id)?
        .ok_or_else(|| EngineError::ProjectNotFound(format!("unknown project: {project_id}")))?;
    let runs = state.manager.repository().list_runs(&project_id)?;
    let mut tasks = Vec::new();
    for run in &runs {
        tasks.extend(state.manager.repository().list_tasks(run.id.as_str())?);
    }
    let facts = state.manager.repository().list_facts(&project_id)?;
    let intents = state.manager.repository().list_intents(&project_id)?;
    let evidence = state.manager.repository().list_evidence(&project_id)?;
    let findings = state.manager.repository().list_findings(&project_id)?;
    let hints = state.manager.repository().list_hints(&project_id)?;
    // Python `load_graph` only includes events attached to a run. Project-level
    // lifecycle notes remain available from `/projects/{id}/events`, but are
    // intentionally absent from the execution DAG snapshot.
    let mut events = Vec::new();
    for run in &runs {
        events.extend(state.manager.repository().list_events(
            &project_id,
            Some(run.id.as_str()),
            1000,
            None,
        )?);
    }
    let tool_invocations = state
        .manager
        .repository()
        .list_tool_invocations(Some(&project_id))?;
    let model_invocations = state
        .manager
        .repository()
        .list_model_invocations(Some(&project_id))?;
    let decision_gates = state
        .manager
        .repository()
        .list_decision_gates(Some(&project_id), None)?;
    let strategy_board_snapshots = state
        .manager
        .repository()
        .list_strategy_board_snapshots(&project_id, None)?;
    let fact_ids: HashSet<&str> = facts.iter().map(|item| item.id.as_str()).collect();
    let intent_ids: HashSet<&str> = intents.iter().map(|item| item.id.as_str()).collect();
    let evidence_ids: HashSet<&str> = evidence.iter().map(|item| item.id.as_str()).collect();
    let finding_ids: HashSet<&str> = findings.iter().map(|item| item.id.as_str()).collect();
    let run_ids: HashSet<&str> = runs.iter().map(|item| item.id.as_str()).collect();
    let task_ids: HashSet<&str> = tasks.iter().map(|item| item.id.as_str()).collect();
    let tool_ids: HashSet<&str> = tool_invocations
        .iter()
        .map(|item| item.id.as_str())
        .collect();
    let mut edges = Vec::new();
    let mut emit = |src: &str, dst: &str, relation: &str| {
        edges.push(serde_json::json!({
            "src": src,
            "dst": dst,
            "relation": relation,
        }));
    };
    for item in &facts {
        for parent in &item.derived_from {
            if fact_ids.contains(parent.as_str())
                || intent_ids.contains(parent.as_str())
                || evidence_ids.contains(parent.as_str())
            {
                emit(parent, item.id.as_str(), "derived_from");
            }
        }
    }
    for item in &intents {
        for fact_id in &item.source_fact_ids {
            if fact_ids.contains(fact_id.as_str()) {
                emit(fact_id, item.id.as_str(), "explores");
            }
        }
        if let Some(run_id) = item.run_id.as_ref()
            && run_ids.contains(run_id.as_str())
        {
            emit(run_id.as_str(), item.id.as_str(), "created_intent");
        }
    }
    for item in &evidence {
        for fact_id in &item.supports_fact_ids {
            if fact_ids.contains(fact_id.as_str()) {
                emit(item.id.as_str(), fact_id, "supports");
            }
        }
        if let Some(task_id) = item.produced_by_task_id.as_ref()
            && task_ids.contains(task_id.as_str())
        {
            emit(task_id.as_str(), item.id.as_str(), "produced_evidence");
        }
        if let Some(run_id) = item.run_id.as_ref()
            && run_ids.contains(run_id.as_str())
        {
            emit(run_id.as_str(), item.id.as_str(), "contains_evidence");
        }
    }
    for item in &findings {
        for evidence_id in &item.evidence_ids {
            if evidence_ids.contains(evidence_id.as_str()) {
                emit(evidence_id, item.id.as_str(), "evidences");
            }
        }
        for fact_id in &item.related_fact_ids {
            if fact_ids.contains(fact_id.as_str()) {
                emit(fact_id, item.id.as_str(), "relates_to");
            }
        }
        if let Some(task_id) = item.produced_by_task_id.as_ref()
            && task_ids.contains(task_id.as_str())
        {
            emit(task_id.as_str(), item.id.as_str(), "produced_finding");
        }
        if let Some(run_id) = item.run_id.as_ref()
            && run_ids.contains(run_id.as_str())
        {
            emit(run_id.as_str(), item.id.as_str(), "contains_finding");
        }
    }
    for run in &runs {
        for task_id in &run.task_ids {
            if task_ids.contains(task_id.as_str()) {
                emit(run.id.as_str(), task_id, "has_task");
            }
        }
    }
    for task in &tasks {
        if let Some(intent_id) = task.intent_id.as_deref()
            && intent_ids.contains(intent_id)
        {
            emit(intent_id, task.id.as_str(), "implemented_by");
        }
        for tool_id in &task.tool_invocation_ids {
            if tool_ids.contains(tool_id.as_str()) {
                emit(task.id.as_str(), tool_id, "used_tool");
            }
        }
        for fact_id in &task.produced_fact_ids {
            if fact_ids.contains(fact_id.as_str()) {
                emit(task.id.as_str(), fact_id, "produced_fact");
            }
        }
        for evidence_id in &task.produced_evidence_ids {
            if evidence_ids.contains(evidence_id.as_str()) {
                emit(task.id.as_str(), evidence_id, "produced_evidence");
            }
        }
        for finding_id in &task.produced_finding_ids {
            if finding_ids.contains(finding_id.as_str()) {
                emit(task.id.as_str(), finding_id, "produced_finding");
            }
        }
    }
    for tool in &tool_invocations {
        if let Some(run_id) = tool.run_id.as_ref()
            && run_ids.contains(run_id.as_str())
        {
            emit(
                run_id.as_str(),
                tool.id.as_str(),
                "contains_tool_invocation",
            );
        }
        if let Some(task_id) = tool.task_id.as_ref()
            && task_ids.contains(task_id.as_str())
        {
            emit(task_id.as_str(), tool.id.as_str(), "used_tool");
        }
    }
    for model in &model_invocations {
        if let Some(run_id) = model.run_id.as_ref()
            && run_ids.contains(run_id.as_str())
        {
            emit(run_id.as_str(), model.id.as_str(), "used_model");
        }
        if let Some(task_id) = model.task_id.as_ref()
            && task_ids.contains(task_id.as_str())
        {
            emit(task_id.as_str(), model.id.as_str(), "used_model");
        }
    }
    for event in &events {
        if let Some(run_id) = event.run_id.as_ref()
            && run_ids.contains(run_id.as_str())
        {
            emit(run_id.as_str(), event.id.as_str(), "recorded_event");
        }
        if let Some(task_id) = event.task_id.as_ref()
            && task_ids.contains(task_id.as_str())
        {
            emit(task_id.as_str(), event.id.as_str(), "recorded_event");
        }
        if let Some(tool_id) = event.tool_invocation_id.as_ref()
            && tool_ids.contains(tool_id.as_str())
        {
            emit(tool_id.as_str(), event.id.as_str(), "recorded_event");
        }
    }
    for gate in &decision_gates {
        if run_ids.contains(gate.audit_run_id.as_str()) {
            emit(
                gate.audit_run_id.as_str(),
                gate.id.as_str(),
                "requires_decision",
            );
        }
        for fact_id in &gate.related_fact_ids {
            if fact_ids.contains(fact_id.as_str()) {
                emit(gate.id.as_str(), fact_id, "decision_context");
            }
        }
        for evidence_id in &gate.related_evidence_ids {
            if evidence_ids.contains(evidence_id.as_str()) {
                emit(gate.id.as_str(), evidence_id, "decision_context");
            }
        }
        for finding_id in &gate.related_finding_ids {
            if finding_ids.contains(finding_id.as_str()) {
                emit(gate.id.as_str(), finding_id, "decision_context");
            }
        }
    }
    for snapshot in &strategy_board_snapshots {
        if let Some(run_id) = snapshot.run_id.as_ref()
            && run_ids.contains(run_id.as_str())
        {
            emit(
                run_id.as_str(),
                snapshot.id.as_str(),
                "strategy_board_snapshot",
            );
        }
    }
    Ok(Json(serde_json::json!({
        "project_id": project_id,
        "facts": facts,
        "intents": intents,
        "hints": hints,
        "evidence": evidence,
        "findings": findings,
        "runs": runs,
        "tasks": tasks,
        "events": events,
        "tool_invocations": tool_invocations,
        "model_invocations": model_invocations,
        "decision_gates": decision_gates,
        "strategy_board_snapshots": strategy_board_snapshots,
        "edges": edges
    })))
}

async fn latest_strategy_board(
    State(state): State<ApiState>,
    Path(project_id): Path<String>,
    Query(query): Query<RunFilterQuery>,
) -> Result<Json<models::StrategyBoardSnapshot>, ApiError> {
    let run_id = query.run_id.map(models::RunId::new);
    let snapshot = state
        .manager
        .get_or_create_strategy_board(
            &project_id,
            run_id.as_ref(),
            models::StrategyBoardDomain::General,
        )
        .await?;
    Ok(Json(snapshot))
}

async fn list_strategy_board_snapshots(
    State(state): State<ApiState>,
    Path(project_id): Path<String>,
    Query(query): Query<RunFilterQuery>,
) -> Result<Json<Vec<models::StrategyBoardSnapshot>>, ApiError> {
    state
        .manager
        .repository()
        .get_project(&project_id)?
        .ok_or_else(|| EngineError::ProjectNotFound(format!("unknown project: {project_id}")))?;
    Ok(Json(
        state
            .manager
            .repository()
            .list_strategy_board_snapshots(&project_id, query.run_id.as_deref())?,
    ))
}

async fn mission_evidence(
    State(state): State<ApiState>,
    Path(mission_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let mission = state
        .manager
        .repository()
        .get_mission(&mission_id)?
        .ok_or_else(|| EngineError::MissionNotFound(format!("unknown mission: {mission_id}")))?;
    let evidence = state
        .manager
        .repository()
        .list_evidence(mission.project_id.as_str())?;
    let findings = state
        .manager
        .repository()
        .list_findings(mission.project_id.as_str())?;
    let tools = state
        .manager
        .repository()
        .list_tool_invocations(Some(mission.project_id.as_str()))?;
    let evidence = evidence
        .into_iter()
        .filter(|item| {
            item.mission_id
                .as_ref()
                .is_some_and(|id| id.as_str() == mission_id)
        })
        .collect::<Vec<_>>();
    let findings = findings
        .into_iter()
        .filter(|item| {
            item.mission_id
                .as_ref()
                .is_some_and(|id| id.as_str() == mission_id)
        })
        .collect::<Vec<_>>();
    let tools = tools
        .into_iter()
        .filter(|item| {
            item.mission_id
                .as_ref()
                .is_some_and(|id| id.as_str() == mission_id)
        })
        .collect::<Vec<_>>();
    let citations = evidence
        .iter()
        .filter_map(|item| {
            item.evidence_path.as_ref().map(|path| {
                serde_json::json!({
                    "evidence_id": item.id,
                    "path": path,
                    "fingerprint": item.fingerprint,
                })
            })
        })
        .collect::<Vec<_>>();
    let evidence_count = evidence.len();
    let findings_count = findings.len();
    let tool_count = tools.len();
    let confirmed_count = findings
        .iter()
        .filter(|item| item.status == models::FindingStatus::Confirmed)
        .count();
    Ok(Json(serde_json::json!({
        "mission_id": mission_id,
        "evidence": evidence,
        "findings": findings,
        "tool_invocations": tools,
        "citations": citations,
        "counts": {
            "evidence": evidence_count,
            "findings": findings_count,
            "tool_invocations": tool_count,
            "confirmed": confirmed_count
        }
    })))
}

// The canvas endpoint mirrors the Python aggregation boundary so the response
// remains one deterministic snapshot for the UI; keep the orchestration in one
// handler while the remaining contract routes are migrated.
#[allow(clippy::too_many_lines)]
async fn mission_canvas(
    State(state): State<ApiState>,
    Path(mission_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let mission = state
        .manager
        .repository()
        .get_mission(&mission_id)?
        .ok_or_else(|| EngineError::MissionNotFound(format!("unknown mission: {mission_id}")))?;
    let project_id = mission.project_id.as_str();
    let run_history = state
        .manager
        .repository()
        .list_runs(project_id)?
        .into_iter()
        .filter(|run| run.mission_id.as_ref() == Some(&mission.id))
        .collect::<Vec<_>>();
    let current_run_id = mission
        .active_run_id
        .clone()
        .filter(|run_id| run_history.iter().any(|run| run.id == *run_id))
        .or_else(|| {
            run_history
                .iter()
                .max_by(|left, right| left.created_at.cmp(&right.created_at))
                .map(|run| run.id.clone())
        });
    let active_run_id = current_run_id.as_ref().map(models::RunId::as_str);
    let runs = run_history
        .iter()
        .filter(|run| current_run_id.as_ref().is_some_and(|id| id == &run.id))
        .cloned()
        .collect::<Vec<_>>();
    let branches = state.manager.repository().list_branches(
        Some(project_id),
        Some(mission_id.as_str()),
        active_run_id,
    )?;
    let branch_ids = branches
        .iter()
        .map(|branch| branch.id.as_str().to_string())
        .collect::<HashSet<_>>();
    let mut tasks = Vec::new();
    for run in &runs {
        tasks.extend(state.manager.repository().list_tasks(run.id.as_str())?);
    }
    let task_ids = tasks
        .iter()
        .map(|task| task.id.as_str().to_string())
        .collect::<HashSet<_>>();
    let current_run_scoped = |item_run_id: Option<&models::RunId>| {
        item_run_id.is_none_or(|id| active_run_id == Some(id.as_str()))
    };
    let intents = state
        .manager
        .repository()
        .list_intents(project_id)?
        .into_iter()
        .filter(|item| {
            current_run_scoped(item.run_id.as_ref())
                && (item.mission_id.as_ref() == Some(&mission.id)
                    || item
                        .branch_id
                        .as_ref()
                        .is_some_and(|id| branch_ids.contains(id.as_str()))
                    || item
                        .claimed_by_task_id
                        .as_ref()
                        .is_some_and(|id| task_ids.contains(id.as_str())))
        })
        .collect::<Vec<_>>();
    let evidence = state
        .manager
        .repository()
        .list_evidence(project_id)?
        .into_iter()
        .filter(|item| {
            current_run_scoped(item.run_id.as_ref())
                && (item.mission_id.as_ref() == Some(&mission.id)
                    || item
                        .branch_id
                        .as_ref()
                        .is_some_and(|id| branch_ids.contains(id.as_str()))
                    || item
                        .produced_by_task_id
                        .as_ref()
                        .is_some_and(|id| task_ids.contains(id.as_str())))
        })
        .collect::<Vec<_>>();
    let findings = state
        .manager
        .repository()
        .list_findings(project_id)?
        .into_iter()
        .filter(|item| {
            current_run_scoped(item.run_id.as_ref())
                && (item.mission_id.as_ref() == Some(&mission.id)
                    || item
                        .branch_id
                        .as_ref()
                        .is_some_and(|id| branch_ids.contains(id.as_str()))
                    || item
                        .produced_by_task_id
                        .as_ref()
                        .is_some_and(|id| task_ids.contains(id.as_str())))
        })
        .collect::<Vec<_>>();
    let observations = state
        .manager
        .repository()
        .list_observations(project_id, active_run_id)?
        .into_iter()
        .filter(|item| {
            item.mission_id.as_ref() == Some(&mission.id)
                || item
                    .branch_id
                    .as_ref()
                    .is_some_and(|id| branch_ids.contains(id.as_str()))
                || item
                    .task_id
                    .as_ref()
                    .is_some_and(|id| task_ids.contains(id.as_str()))
        })
        .collect::<Vec<_>>();
    let tools = state
        .manager
        .repository()
        .list_tool_invocations(Some(project_id))?
        .into_iter()
        .filter(|item| {
            current_run_scoped(item.run_id.as_ref())
                && (item.mission_id.as_ref() == Some(&mission.id)
                    || item
                        .branch_id
                        .as_ref()
                        .is_some_and(|id| branch_ids.contains(id.as_str()))
                    || item
                        .task_id
                        .as_ref()
                        .is_some_and(|id| task_ids.contains(id.as_str())))
        })
        .collect::<Vec<_>>();
    let termination = state
        .manager
        .repository()
        .list_termination_assessments(project_id, active_run_id)?;
    let directives = state.manager.repository().list_user_directives(
        Some(project_id),
        Some(mission_id.as_str()),
        active_run_id,
        None,
    )?;
    let decision_gates = state
        .manager
        .repository()
        .list_decision_gates(Some(project_id), active_run_id)?;
    let snapshots = state
        .manager
        .repository()
        .list_strategy_board_snapshots(project_id, active_run_id)?;
    let strategy_board_latest = snapshots
        .iter()
        .max_by_key(|item| (item.version, item.created_at))
        .cloned();
    let mut edges = Vec::new();
    for branch in &branches {
        if let Some(parent) = &branch.parent_branch_id {
            edges.push(serde_json::json!({
                "src": parent,
                "dst": branch.id,
                "relation": "parent"
            }));
        }
    }
    for item in &evidence {
        for fact_id in &item.supports_fact_ids {
            edges.push(serde_json::json!({
                "src": item.id,
                "dst": fact_id,
                "relation": "supports"
            }));
        }
    }
    let assets = state
        .manager
        .repository()
        .list_mission_assets(Some(mission_id.as_str()), None, None, None)?
        .into_iter()
        .filter(|asset| current_run_scoped(asset.run_id.as_ref()))
        .collect::<Vec<_>>();
    Ok(Json(serde_json::json!({
        "canvas": {
            "mission": mission,
            "assets": assets,
            "branches": branches,
            "decision_gates": decision_gates,
            "directives": directives,
            "edges": edges,
            "evidence": evidence,
            "findings": findings,
            "intents": intents,
            "observations": observations,
            "run_history": run_history,
            "runs": runs,
            "strategy_board_latest": strategy_board_latest,
            "tasks": tasks,
            "termination_assessments": termination,
            "tool_invocations": tools
        }
    })))
}

#[derive(Debug, Deserialize)]
struct StartMissionRequest {
    #[serde(default)]
    config: Map<String, Value>,
    #[serde(default = "default_auto_start")]
    auto_start_runtime: bool,
    max_concurrent_branches: Option<i64>,
    max_total_steps: Option<i64>,
}

fn default_auto_start() -> bool {
    true
}

#[derive(Debug, Serialize)]
struct MissionStartResponse {
    mission: Mission,
    run: models::AuditRun,
    branches: Vec<models::Branch>,
}

async fn start_mission(
    State(state): State<ApiState>,
    Path(mission_id): Path<String>,
    body: Option<Json<StartMissionRequest>>,
) -> Result<(StatusCode, Json<MissionStartResponse>), ApiError> {
    let request = body.map_or_else(
        || StartMissionRequest {
            config: Map::new(),
            auto_start_runtime: true,
            max_concurrent_branches: None,
            max_total_steps: None,
        },
        |Json(request)| request,
    );
    // Mission-start is also a public entry point (not only intake). Inject the
    // canonical per-mission workspace here so direct API callers receive the
    // same artifact/log/scratch contract as the Python runtime and intake.
    let mission = state
        .manager
        .repository()
        .get_mission(&mission_id)?
        .ok_or_else(|| EngineError::MissionNotFound(mission_id.clone()))?;
    let config = mission_workspace::config_with_mission_workspace(
        request.config,
        &state.mission_workspace_root,
        &mission,
    )
    .map_err(|error| ApiError(EngineError::Value(error.to_string())))?;
    let result: MissionStartResult = state
        .manager
        .start_mission(
            &MissionId::new(mission_id),
            Some(config),
            request.auto_start_runtime,
            request.auto_start_runtime,
            request.max_concurrent_branches,
            request.max_total_steps,
        )
        .await?;
    let run = state
        .manager
        .repository()
        .get_run(result.run_id.as_str())?
        .ok_or_else(|| EngineError::RunNotFound(result.run_id.as_str().to_string()))?;
    let status = if request.auto_start_runtime {
        StatusCode::ACCEPTED
    } else {
        StatusCode::OK
    };
    Ok((
        status,
        Json(MissionStartResponse {
            mission: result.mission,
            run,
            branches: result.branches,
        }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{Body, to_bytes};
    use axum::http::Request;
    use engines::default_solver_registry;
    use runtime::InMemoryTaskBackend;
    use storage::SqliteRepository;
    use tower::ServiceExt;

    async fn request_json(
        app: &Router,
        method: axum::http::Method,
        uri: impl AsRef<str>,
        body: Option<&str>,
    ) -> (StatusCode, Value) {
        let mut builder = Request::builder().method(method).uri(uri.as_ref());
        if body.is_some() {
            builder = builder.header("content-type", "application/json");
        }
        let response = app
            .clone()
            .oneshot(
                builder
                    .body(body.map_or_else(Body::empty, |value| Body::from(value.to_owned())))
                    .unwrap_or_else(|error| panic!("request must build: {error}")),
            )
            .await
            .unwrap_or_else(|error| panic!("request must respond: {error}"));
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap_or_else(|error| panic!("response body must read: {error}"));
        let json = serde_json::from_slice(&bytes)
            .unwrap_or_else(|error| panic!("response body must parse: {error}"));
        (status, json)
    }

    async fn wait_for_terminal_canvas(app: &Router, mission_id: &str) -> Value {
        for _ in 0..100 {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let (status, parsed) = request_json(
                app,
                axum::http::Method::GET,
                format!("/missions/{mission_id}/canvas"),
                None,
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            let tasks = parsed["canvas"]["tasks"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            if !tasks.is_empty()
                && tasks
                    .iter()
                    .all(|task| task["status"] == "failed" || task["status"] == "succeeded")
            {
                return parsed;
            }
        }
        panic!("binary mission tasks did not reach a terminal state");
    }

    #[tokio::test]
    async fn binary_mission_routes_through_agent_chain_and_fails_closed() {
        // Rust-only E2E: POST /missions -> POST /missions/{id}/start ->
        // branch dispatch -> binary_analysis solver -> Agent Tool Harness.
        // With no IDA endpoint configured, the harness fails closed: intents
        // dispatch to binary_analysis, tasks never succeed, and no findings
        // are fabricated.
        let directory =
            tempfile::tempdir().unwrap_or_else(|error| panic!("tempdir must create: {error}"));
        let state = test_state_with_tool_path(&directory.path().join("local-tools.json"));
        let app = router(state);

        let (status, mission) = request_json(
            &app,
            axum::http::Method::POST,
            "/missions",
            Some(
                r#"{"user_goal":"Analyze the binary for memory hazards",
                    "target":{"binary":"C:/chal.exe"}}"#,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let mission_id = mission["id"].as_str().expect("mission id").to_string();

        let (status, start) = request_json(
            &app,
            axum::http::Method::POST,
            format!("/missions/{mission_id}/start"),
            Some(r#"{"auto_start_runtime":true}"#),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        let branches = start["branches"].as_array().expect("branches");
        assert!(!branches.is_empty(), "binary branches materialize");
        assert!(
            branches
                .iter()
                .all(|branch| branch["metadata"]["branch_kind"]
                    .as_str()
                    .unwrap_or_default()
                    .starts_with("binary.")),
            "binary mission generates binary.* branches"
        );

        let canvas_json = wait_for_terminal_canvas(&app, &mission_id).await;
        let canvas = &canvas_json["canvas"];
        // 二进制分支自己的 intent 必须派给 binary_analysis。收口链的第二波
        // 可能派发其他 solver（mission 跑完不再早停，见 termination 的
        // needs_contract_review → Complete），所以按 branch_id 过滤而非
        // 要求全场只有二进制 solver。
        let binary_branch_ids: HashSet<&str> = branches
            .iter()
            .filter_map(|branch| {
                let kind = branch["metadata"]["branch_kind"].as_str().unwrap_or_default();
                if kind.starts_with("binary.") {
                    branch["id"].as_str()
                } else {
                    None
                }
            })
            .collect();
        let dispatched: Vec<&str> = canvas["intents"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter(|intent| {
                        intent["branch_id"]
                            .as_str()
                            .is_some_and(|id| binary_branch_ids.contains(id))
                    })
                    .filter_map(|intent| intent["solver"].as_str())
                    .collect()
            })
            .unwrap_or_default();
        assert!(!dispatched.is_empty(), "at least one intent dispatched");
        assert!(
            dispatched.iter().all(|solver| *solver == "binary_analysis"),
            "binary branches dispatch to binary_analysis: {dispatched:?}"
        );
        let statuses: Vec<&str> = canvas["tasks"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|task| task["status"].as_str())
                    .collect()
            })
            .unwrap_or_default();
        assert!(!statuses.is_empty(), "solver tasks ran through the chain");
        let tool_invocations = canvas["tool_invocations"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert!(
            tool_invocations.is_empty(),
            "no IDA configured: zero tool invocations may run"
        );
        assert!(
            canvas["findings"].as_array().is_none_or(Vec::is_empty),
            "no findings may be fabricated without evidence"
        );
        assert!(
            !dispatched.is_empty() && dispatched.iter().all(|solver| *solver == "binary_analysis"),
            "binary branches dispatch to binary_analysis: {dispatched:?}"
        );
    }

    /// 分支生成用的假 provider：恒返回 binary.* 分支。
    ///
    /// 分支生成改为 LLM 驱动后，E2E 测试必须显式提供一个 runtime——否则
    /// `start` 会以 `ProviderConfigError` 失败关闭，测试就失去意义了。
    struct BranchStubProvider;

    #[async_trait::async_trait]
    impl agents::llm::ProviderRuntime for BranchStubProvider {
        async fn list_providers(
            &self,
        ) -> Result<Vec<models::provider::ProviderConfig>, agents::llm::ProviderCallError> {
            Ok(Vec::new())
        }

        async fn get_provider(
            &self,
            _provider_id: &str,
        ) -> Result<Option<models::provider::ProviderConfig>, agents::llm::ProviderCallError> {
            Ok(None)
        }

        async fn resolve_default_provider(
            &self,
        ) -> Result<Option<models::provider::ProviderConfig>, agents::llm::ProviderCallError> {
            Ok(None)
        }

        async fn health_check(
            &self,
            _provider_id: &str,
        ) -> Result<models::provider::ProviderHealthResult, agents::llm::ProviderCallError> {
            unreachable!("branch stub never health-checks")
        }

        async fn generate_text(
            &self,
            _request: agents::llm::TextGenerationRequest<'_>,
        ) -> Result<agents::llm::LlmResponse, agents::llm::ProviderCallError> {
            unreachable!("branch stub only serves structured calls")
        }

        async fn generate_structured(
            &self,
            _request: agents::llm::StructuredGenerationRequest<'_>,
        ) -> Result<serde_json::Map<String, serde_json::Value>, agents::llm::ProviderCallError> {
            let branch = |title: &str, kind: &str| {
                serde_json::json!({
                    "title": title,
                    "hypothesis": "The binary exposes at least one untested assumption.",
                    "rationale": "Grounding later branches.",
                    "branch_kind": kind,
                    "priority": 80,
                    "confidence": 0.6
                })
            };
            serde_json::json!({ "branches": [
                branch("Map parser input surface", "binary.parser_surface"),
                branch("Trace dangerous API usage", "binary.dangerous_api"),
                branch("Probe heap and stack handling", "binary.heap_stack"),
            ]})
            .as_object()
            .cloned()
            .ok_or_else(|| agents::llm::ProviderCallError::new("stub branches must be an object"))
        }
    }

    fn test_state_with_tool_path(config_path: &std::path::Path) -> ApiState {
        let repository = Arc::new(
            SqliteRepository::open(":memory:")
                .unwrap_or_else(|error| panic!("in-memory repository must open: {error}")),
        );
        let manager = Arc::new(AuditManager::new(
            repository,
            default_solver_registry(),
            Arc::new(InMemoryTaskBackend::default()),
        ));
        manager.set_provider_runtime(Some(Arc::new(BranchStubProvider)));
        let execution_control = Arc::new(
            ExecutionControlPlane::safe_local(Arc::clone(manager.repository()))
                .unwrap_or_else(|error| panic!("execution control must initialize: {error}")),
        );
        let tool_installs = Arc::new(
            ToolInstallCoordinator::new(config_path.to_path_buf())
                .unwrap_or_else(|error| panic!("tool coordinator must initialize: {error}")),
        );
        ApiState::with_services(
            manager,
            execution_control,
            tool_installs,
            config_path.to_path_buf(),
            config_path
                .parent()
                .map_or_else(|| PathBuf::from("uploads"), FsPath::to_path_buf),
            config_path
                .parent()
                .map_or_else(|| PathBuf::from("data/missions"), FsPath::to_path_buf),
        )
    }

    #[test]
    fn error_mapping_preserves_contract_messages() {
        assert_eq!(
            ApiError(EngineError::MissionNotFound("m".to_string()))
                .0
                .to_string(),
            "unknown mission: m"
        );
        assert_eq!(
            ApiError(EngineError::Value("bad".to_string()))
                .0
                .to_string(),
            "bad"
        );
    }

    #[test]
    fn create_request_defaults_are_python_compatible() {
        let request: CreateMissionRequest = serde_json::from_str(r#"{"user_goal":"find flag"}"#)
            .unwrap_or_else(|error| panic!("valid request must parse: {error}"));
        assert_eq!(request.created_by, "user");
        assert!(request.target.is_empty());
        assert!(request.constraints.is_empty());
        assert_eq!(request.approval_mode, ApprovalMode::AskForApproval);
    }

    #[tokio::test]
    async fn native_web_recon_is_available_without_external_tools() {
        let directory =
            tempfile::tempdir().unwrap_or_else(|error| panic!("tempdir must create: {error}"));
        let state = test_state_with_tool_path(&directory.path().join("local-tools.json"));
        let response = router(state)
            .oneshot(
                Request::builder()
                    .uri("/capabilities")
                    .body(Body::empty())
                    .unwrap_or_else(|error| panic!("request must build: {error}")),
            )
            .await
            .unwrap_or_else(|error| panic!("router must respond: {error}"));
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap_or_else(|error| panic!("capabilities body must read: {error}"));
        let capabilities: Vec<Value> = serde_json::from_slice(&body)
            .unwrap_or_else(|error| panic!("capabilities must parse: {error}"));
        let web_recon = capabilities
            .iter()
            .find(|capability| capability["id"] == "web_recon")
            .unwrap_or_else(|| panic!("web_recon capability must exist"));
        assert_eq!(web_recon["status"], "available");
        assert_eq!(web_recon["available"], true);
    }

    #[tokio::test]
    async fn execution_routes_are_durable_and_fail_closed() {
        let directory =
            tempfile::tempdir().unwrap_or_else(|error| panic!("tempdir must create: {error}"));
        let state = test_state_with_tool_path(&directory.path().join("local-tools.json"));
        let app = router(state);
        let request = Request::builder()
            .method("POST")
            .uri("/executions")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"request":{"tool_name":"scanner"}}"#))
            .unwrap_or_else(|error| panic!("request must build: {error}"));
        let response = app
            .clone()
            .oneshot(request)
            .await
            .unwrap_or_else(|error| panic!("router must respond: {error}"));
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap_or_else(|error| panic!("response body must read: {error}"));
        let queued: ExecutionJob = serde_json::from_slice(&body)
            .unwrap_or_else(|error| panic!("execution response must parse: {error}"));
        assert_eq!(queued.status, ExecutionStatus::Queued);

        let wait = Request::builder()
            .method("POST")
            .uri(format!("/executions/{}/wait?timeout_seconds=1", queued.id))
            .body(Body::empty())
            .unwrap_or_else(|error| panic!("request must build: {error}"));
        let response = app
            .oneshot(wait)
            .await
            .unwrap_or_else(|error| panic!("router must respond: {error}"));
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap_or_else(|error| panic!("response body must read: {error}"));
        let value: Value = serde_json::from_slice(&body)
            .unwrap_or_else(|error| panic!("wait response must parse: {error}"));
        assert_eq!(value["timed_out"], false);
        assert_eq!(value["job"]["status"], "denied");
        assert!(value["job"]["tool_invocation_id"].is_string());
    }

    #[tokio::test]
    async fn public_execution_route_rejects_privileged_backends() {
        let directory =
            tempfile::tempdir().unwrap_or_else(|error| panic!("tempdir must create: {error}"));
        let state = test_state_with_tool_path(&directory.path().join("local-tools.json"));
        let app = router(state);
        let request = Request::builder()
            .method("POST")
            .uri("/executions")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"request":{"tool_name":"scanner","backend_type":"docker"}}"#,
            ))
            .unwrap_or_else(|error| panic!("request must build: {error}"));
        let response = app
            .oneshot(request)
            .await
            .unwrap_or_else(|error| panic!("router must respond: {error}"));
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn tool_catalog_routes_configure_and_sync_local_module() {
        let directory =
            tempfile::tempdir().unwrap_or_else(|error| panic!("tempdir must create: {error}"));
        let config_path = directory.path().join("local-tools.json");
        let state = test_state_with_tool_path(&config_path);
        let app = router(state.clone());

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/tool-catalog")
                    .body(Body::empty())
                    .unwrap_or_else(|error| panic!("request must build: {error}")),
            )
            .await
            .unwrap_or_else(|error| panic!("router must respond: {error}"));
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap_or_else(|error| panic!("catalog body must read: {error}"));
        let catalog: Vec<ToolCatalogEntry> = serde_json::from_slice(&body)
            .unwrap_or_else(|error| panic!("catalog must parse: {error}"));
        assert!(!catalog.is_empty());

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/tool-catalog/ffuf/configure")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"executable_path":"C:\\Tools\\ffuf.exe","enabled":true}"#,
                    ))
                    .unwrap_or_else(|error| panic!("request must build: {error}")),
            )
            .await
            .unwrap_or_else(|error| panic!("router must respond: {error}"));
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap_or_else(|error| panic!("configure body must read: {error}"));
        let configured: ToolCatalogEntry = serde_json::from_slice(&body)
            .unwrap_or_else(|error| panic!("configured entry must parse: {error}"));
        assert_eq!(
            configured.detection.executable_path.as_deref(),
            Some("C:\\Tools\\ffuf.exe")
        );
        assert!(config_path.is_file());
        let module = state
            .manager
            .repository()
            .list_modules()
            .unwrap_or_else(|error| panic!("modules must list: {error}"))
            .into_iter()
            .find(|module| module.metadata.get("tool_name").and_then(Value::as_str) == Some("ffuf"))
            .unwrap_or_else(|| panic!("configured tool must be synced into module storage"));
        assert_eq!(module.module_type, ModuleType::LocalTool);
        assert_eq!(module.domain.as_str(), "content_discovery");

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/tool-catalog/not-a-tool/install")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap_or_else(|error| panic!("request must build: {error}")),
            )
            .await
            .unwrap_or_else(|error| panic!("router must respond: {error}"));
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn tool_catalog_health_fails_closed_on_fake_path() {
        let directory =
            tempfile::tempdir().unwrap_or_else(|error| panic!("tempdir must create: {error}"));
        let state = test_state_with_tool_path(&directory.path().join("local-tools.json"));
        let app = router(state);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/tool-catalog/ffuf/configure")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"executable_path":"C:\\Tools\\fake-ffuf.exe","enabled":true}"#,
                    ))
                    .unwrap_or_else(|error| panic!("request must build: {error}")),
            )
            .await
            .unwrap_or_else(|error| panic!("router must respond: {error}"));
        assert_eq!(response.status(), StatusCode::OK);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/tool-catalog/ffuf/test")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap_or_else(|error| panic!("request must build: {error}")),
            )
            .await
            .unwrap_or_else(|error| panic!("router must respond: {error}"));
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap_or_else(|error| panic!("health body must read: {error}"));
        let health: ToolHealthResult = serde_json::from_slice(&body)
            .unwrap_or_else(|error| panic!("health must parse: {error}"));
        assert!(!health.ok);
        assert_eq!(health.status, "error");
    }

    #[tokio::test]
    async fn tool_catalog_manual_install_job_reaches_terminal_state() {
        let directory =
            tempfile::tempdir().unwrap_or_else(|error| panic!("tempdir must create: {error}"));
        let state = test_state_with_tool_path(&directory.path().join("local-tools.json"));
        let app = router(state);

        // A manual recipe queues an observable job and reaches a terminal
        // state without executing anything.
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/tool-catalog/wappalyzergo/install")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap_or_else(|error| panic!("request must build: {error}")),
            )
            .await
            .unwrap_or_else(|error| panic!("router must respond: {error}"));
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap_or_else(|error| panic!("job body must read: {error}"));
        let queued: ToolInstallJob =
            serde_json::from_slice(&body).unwrap_or_else(|error| panic!("job must parse: {error}"));
        assert_eq!(queued.tool_id, "wappalyzergo");
        assert_eq!(queued.method, "manual");

        let mut terminal = queued.clone();
        for _ in 0..500 {
            if terminal.status.is_terminal() {
                break;
            }
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(format!("/tool-catalog/installations/{}", queued.id))
                        .body(Body::empty())
                        .unwrap_or_else(|error| panic!("request must build: {error}")),
                )
                .await
                .unwrap_or_else(|error| panic!("router must respond: {error}"));
            assert_eq!(response.status(), StatusCode::OK);
            let body = to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap_or_else(|error| panic!("job body must read: {error}"));
            terminal = serde_json::from_slice(&body)
                .unwrap_or_else(|error| panic!("job must parse: {error}"));
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }
        assert_eq!(terminal.id, queued.id);
        assert_eq!(
            terminal.status,
            engines::tool_catalog::ToolInstallJobStatus::Manual
        );
        assert!(terminal.message.contains("github.com/projectdiscovery/wappalyzergo"));
        assert!(terminal.finished_at.is_some());

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/tool-catalog/installations?tool_id=wappalyzergo")
                    .body(Body::empty())
                    .unwrap_or_else(|error| panic!("request must build: {error}")),
            )
            .await
            .unwrap_or_else(|error| panic!("router must respond: {error}"));
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap_or_else(|error| panic!("jobs body must read: {error}"));
        let jobs: Vec<ToolInstallJob> = serde_json::from_slice(&body)
            .unwrap_or_else(|error| panic!("jobs must parse: {error}"));
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].id, queued.id);
    }
}
#[cfg(test)]
mod agent_preset_api_tests {
    #![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
    use super::*;
    use axum::extract::Path;
    use engines::default_solver_registry;
    use runtime::InMemoryTaskBackend;
    use storage::SqliteRepository;

    fn test_state() -> ApiState {
        // 自包含构造：内存库 + 空工具目录路径（与其余 api 测试同构）。
        let repository = Arc::new(
            SqliteRepository::open(":memory:")
                .unwrap_or_else(|error| panic!("in-memory repository must open: {error}")),
        );
        let manager = Arc::new(AuditManager::new(
            repository,
            default_solver_registry(),
            Arc::new(InMemoryTaskBackend::default()),
        ));
        let execution_control = Arc::new(
            ExecutionControlPlane::safe_local(Arc::clone(manager.repository()))
                .unwrap_or_else(|error| panic!("execution control must initialize: {error}")),
        );
        let tool_installs = Arc::new(
            ToolInstallCoordinator::new(PathBuf::from("build/test-local-tools.json"))
                .unwrap_or_else(|error| panic!("tool coordinator must initialize: {error}")),
        );
        ApiState::with_services(
            manager,
            execution_control,
            tool_installs,
            PathBuf::from("build/test-local-tools.json"),
            PathBuf::from("build/test-uploads"),
            PathBuf::from("build/test-workspace"),
        )
    }

    #[tokio::test]
    async fn seeding_is_first_insert_only_and_lists_six_builtins() {
        let state = test_state();
        let listed = list_agent_presets(State(state.clone()))
            .await
            .expect("list must succeed");
        assert_eq!(
            listed.len(),
            models::agent_preset::BUILTIN_KEYS.len(),
            "启动播种必须产出全部内置预设"
        );
        for key in models::agent_preset::BUILTIN_KEYS {
            assert!(
                listed.iter().any(|preset| preset.key == key && preset.builtin),
                "缺少内置预设 {key}"
            );
        }

        // 用户编辑内置预设后重复播种：编辑不得被覆盖。
        let mut advisor = state
            .manager
            .repository()
            .get_agent_preset(models::agent_preset::PRESET_MISSION_ADVISOR)
            .expect("query")
            .expect("seeded");
        advisor.instruction_template = "自定义顾问模板 {{question}}".to_string();
        advisor.variables = models::agent_preset::extract_variables(&advisor.instruction_template);
        state
            .manager
            .repository()
            .upsert_agent_preset(&advisor)
            .expect("edit");
        seed_agent_presets(&state.manager);
        let after = state
            .manager
            .repository()
            .get_agent_preset(models::agent_preset::PRESET_MISSION_ADVISOR)
            .expect("query")
            .expect("exists");
        assert_eq!(after.instruction_template, "自定义顾问模板 {{question}}");
    }

    #[tokio::test]
    async fn custom_preset_crud_preview_and_builtin_guardrails() {
        let state = test_state();

        // 创建：key 与内置冲突 → 拒绝。
        let conflict = create_agent_preset(
            State(state.clone()),
            axum::Json(CreateAgentPresetRequest {
                key: models::agent_preset::PRESET_WORKER_INSTRUCTION.to_string(),
                name: "x".to_string(),
                description: None,
                instruction_template: "T {{q}}".to_string(),
                wrapup_template: None,
                model_alias: None,
                max_turns: None,
                skills: Vec::new(),
                tools: Vec::new(),
            }),
        )
        .await;
        assert!(conflict.is_err(), "内置 key 必须保留");

        // 创建自定义预设。
        let created = create_agent_preset(
            State(state.clone()),
            axum::Json(CreateAgentPresetRequest {
                key: "team_worker".to_string(),
                name: "Team worker".to_string(),
                description: Some("团队定制".to_string()),
                instruction_template: "执行 {{solver_name}}，预算 {{budget_steps}}。".to_string(),
                wrapup_template: None,
                model_alias: Some("gpt-main".to_string()),
                max_turns: Some(12),
                skills: Vec::new(),
                tools: Vec::new(),
            }),
        )
        .await
        .expect("create must succeed");
        assert!(!created.builtin);
        assert_eq!(created.variables, ["solver_name", "budget_steps"]);

        // 模板变更直接覆盖当前模板，不产生历史副本。
        let updated = update_agent_preset(
            State(state.clone()),
            Path("team_worker".to_string()),
            axum::Json(UpdateAgentPresetRequest {
                name: None,
                description: None,
                enabled: None,
                model_alias: None,
                max_turns: None,
                wrapup_template: None,
                skills: None,
                tools: None,
                instruction_template: Some("执行 {{solver_name}}（v2）预算 {{budget_steps}}".to_string()),
            }),
        )
        .await
        .expect("update must succeed");
        assert_eq!(updated.instruction_template, "执行 {{solver_name}}（v2）预算 {{budget_steps}}");
        assert_eq!(updated.variables, ["solver_name", "budget_steps"]);

        // 停用后 get 仍可见（列表不过滤，运行时解析才过滤）。
        let _ = update_agent_preset(
            State(state.clone()),
            Path("team_worker".to_string()),
            axum::Json(UpdateAgentPresetRequest {
                name: None,
                description: None,
                enabled: Some(false),
                model_alias: None,
                max_turns: None,
                wrapup_template: None,
                skills: None,
                tools: None,
                instruction_template: None,
            }),
        )
        .await
        .expect("disable");

        // preview 渲染（变量缺失 → 空串）。
        let mut vars = serde_json::Map::new();
        vars.insert("solver_name".to_string(), serde_json::json!("web_recon"));
        vars.insert("budget_steps".to_string(), serde_json::json!("40"));
        let preview = preview_agent_preset(
            State(state.clone()),
            Path("team_worker".to_string()),
            axum::Json(PreviewAgentPresetRequest { variables: vars }),
        )
        .await
        .expect("preview");
        assert_eq!(
            preview["rendered"], "执行 web_recon（v2）预算 40",
            "渲染按当前版本模板"
        );

        // 删除：内置拒绝、自定义可删。
        let builtin_delete = delete_agent_preset(
            State(state.clone()),
            Path(models::agent_preset::PRESET_WORKER_INSTRUCTION.to_string()),
        )
        .await;
        assert!(builtin_delete.is_err(), "内置预设不可删除");
        delete_agent_preset(State(state.clone()), Path("team_worker".to_string()))
            .await
            .expect("delete custom");
        assert!(
            state
                .manager
                .repository()
                .get_agent_preset("team_worker")
                .expect("query")
                .is_none()
        );
    }
}
