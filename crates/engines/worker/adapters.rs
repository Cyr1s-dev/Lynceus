//! 外部 Worker Runtime 适配器：Codex / Claude Code / Pi / DeepSeek
//! Harness 四个**显式**实现（不提供任意命令模板式通用 worker）。
//!
//! 每个适配器只负责三件事：
//! 1. 官方 headless 接口的命令行构造（版本感知，不做永久硬编码）；
//! 2. Lynceus Connection → 该 CLI 认证环境变量的**适配器私有映射**
//!    （不同 CLI 的变量名不同，公共层绝不硬编码通用变量）；
//! 3. 输出解析：session 引用（continuation 锚点）+ 有界受控摘要。
//!
//! 版本门：Developer Preview 协议不稳，探测无法确认兼容性时显式返回
//! Unsupported / Unavailable，**绝不猜协议**。

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use agents::worker::WorkerExecutionOutcome;
use agents::worker::WorkerExecutionRequest;
use agents::worker::WorkerRuntime;
use agents::worker::WorkerRuntimeError;
use agents::worker::WorkerRuntimeErrorKind;
use async_trait::async_trait;
use models::provider::ProviderType;
use models::worker::ResolvedWorkerConnection;
use models::worker::WorkerAvailability;
use models::worker::WorkerEventKind;
use models::worker::WorkerInvocation;
use models::worker::WorkerInvocationPurpose;
use models::worker::WorkerProbe;
use models::worker::WorkerRun;
use models::worker::WorkerRunStatus;
use models::worker::WorkerRuntimeProfile;
use models::worker::WorkerRuntimeType;

use super::process::ProcessSpec;
use super::process::locate_executable;
use super::process::override_env_name;
use super::process::run_process;

/// 默认单次执行超时（run config / profile 可覆盖）。
pub(crate) const DEFAULT_TIMEOUT_SECONDS: u64 = 900;

/// 进程级会话引用 sink：`worker_run_id → session_ref`。
///
/// 与 `gateway::global_gateway()` 同一先例：WorkerRegistry 是进程单例，
/// 这张表由 registry 与全部 adapter 共享（流式解析写入，中断定位 /
/// resume / 运行簿记读取）。测试用 [`clear_session_sink`] 隔离。
static SESSION_SINK: std::sync::OnceLock<std::sync::Arc<Mutex<std::collections::HashMap<String, String>>>> =
    std::sync::OnceLock::new();

/// 会话 sink 的共享句柄（adapter 构造与 registry 查询共用）。
pub(crate) fn session_sink_arc() -> std::sync::Arc<Mutex<std::collections::HashMap<String, String>>> {
    std::sync::Arc::clone(SESSION_SINK.get_or_init(|| std::sync::Arc::new(Mutex::new(std::collections::HashMap::new()))))
}

/// 会话 sink 访问器（直接读写）。
pub(crate) fn session_sink() -> &'static Mutex<std::collections::HashMap<String, String>> {
    &*SESSION_SINK.get_or_init(|| std::sync::Arc::new(Mutex::new(std::collections::HashMap::new())))
}

/// 清空会话 sink（测试隔离用）。
#[cfg(test)]
pub(crate) fn clear_session_sink() {
    if let Ok(mut sink) = session_sink().lock() {
        sink.clear();
    }
}

/// 进行中会话的取消注册表：worker run id → 取消信号 / 会话引用。
///
/// `cancel()` 查到信号即触发子进程终止；查不到说明会话已结束或未知，
/// 返回 `false`（不伪造成功）。`report_session` 由流式解析在首次拿到
/// 会话引用（codex thread id / claude session id）时调用——它同时是
/// “这个 worker 正在跑”的登记簿，中断定位与 resume 都读它。
#[derive(Default)]
pub(crate) struct InflightRegistry {
    senders: Mutex<std::collections::HashMap<String, tokio::sync::watch::Sender<bool>>>,
    sessions: Mutex<std::collections::HashMap<String, String>>,
}

impl InflightRegistry {
    fn register(&self, worker_run_id: &str) -> tokio::sync::watch::Receiver<bool> {
        let (sender, receiver) = tokio::sync::watch::channel(false);
        if let Ok(mut senders) = self.senders.lock() {
            senders.insert(worker_run_id.to_string(), sender);
        }
        receiver
    }

    fn unregister(&self, worker_run_id: &str) {
        if let Ok(mut senders) = self.senders.lock() {
            senders.remove(worker_run_id);
        }
        // 会话引用**不**在这里清：进程结束后派发层还要读它做 resume，
        // 清理走 `forget`（派发层消费完结终态时调）。
    }

    /// 登记运行中的 worker 及其会话引用。
    pub(crate) fn report_session(&self, worker_run_id: &str, session_ref: &str) {
        if let Ok(mut sessions) = self.sessions.lock() {
            sessions.insert(worker_run_id.to_string(), session_ref.to_string());
        }
    }

    /// 读取已上报的会话引用（resume 用）。
    pub(crate) fn session_ref(&self, worker_run_id: &str) -> Option<String> {
        self.sessions.lock().ok()?.get(worker_run_id).cloned()
    }

    /// 终结清理：取消信号与会话引用一起清，防止簿记泄漏。
    pub(crate) fn forget(&self, worker_run_id: &str) {
        if let Ok(mut senders) = self.senders.lock() {
            senders.remove(worker_run_id);
        }
        if let Ok(mut sessions) = self.sessions.lock() {
            sessions.remove(worker_run_id);
        }
    }

    fn cancel(&self, worker_run_id: &str) -> bool {
        if let Ok(senders) = self.senders.lock()
            && let Some(sender) = senders.get(worker_run_id)
        {
            return sender.send(true).is_ok();
        }
        false
    }
}

/// adapter 共享上下文。
pub(crate) struct AdapterContext {
    pub runtime_type: WorkerRuntimeType,
    pub binary_names: &'static [&'static str],
    pub resolver: Arc<dyn agents::worker::WorkerConnectionResolver>,
    pub inflight: Arc<InflightRegistry>,
    /// 会话引用 sink：流式解析首次拿到 codex thread id / claude session
    /// id 时写入 `worker_run_id → session_ref`。registry 与 adapter 共处
    /// 一个进程，共享这张表（中断定位、resume、运行簿记都读它）。
    pub session_sink: Arc<Mutex<std::collections::HashMap<String, String>>>,
}

/// 单次执行的命令构造产物（adapter 私有映射的结果）。
pub(crate) struct BuiltCommand {
    pub binary_names: &'static [&'static str],
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    /// 经 stdin 传入的数据（多行指令必须走 stdin——Windows `.cmd` shim
    /// 会在 argv 换行处截断）。
    pub stdin: Option<String>,
}

/// 从输出中提取会话引用的正则（codex JSONL / claude JSON 共用的宽松匹配）。
fn extract_session_ref(output: &str) -> Option<String> {
    let pattern = regex::Regex::new(r#""session_id"\s*:\s*"([^"]+)""#).ok()?;
    pattern
        .captures(output)
        .and_then(|captures| captures.get(1))
        .map(|match_| match_.as_str().to_string())
}

/// 通用执行流程：Profile → Connection → 命令构造 → 受控运行 → 审计收口。
async fn execute(
    context: &AdapterContext,
    request: WorkerExecutionRequest,
    purpose: WorkerInvocationPurpose,
    build: impl FnOnce(
        &ResolvedWorkerConnection,
        &WorkerRuntimeProfile,
        &WorkerExecutionRequest,
    ) -> Result<BuiltCommand, WorkerRuntimeError>,
    stream: Option<StreamFlavor>,
) -> Result<WorkerExecutionOutcome, WorkerRuntimeError> {
    if purpose == WorkerInvocationPurpose::Resume && request.session_ref.is_none() {
        return Err(WorkerRuntimeError::new(
            WorkerRuntimeErrorKind::NotReady,
            "resume requires a session reference",
        ));
    }
    if purpose == WorkerInvocationPurpose::Start && request.session_ref.is_some() {
        return Err(WorkerRuntimeError::new(
            WorkerRuntimeErrorKind::NotReady,
            "start cannot carry a resume session reference",
        ));
    }
    // 认证链路：Profile（绑定）→ Connection（Lynceus 配置）→ 注入。
    let (profile, connection) = {
        let Some(profile) = context.resolver.profile(context.runtime_type).await? else {
            return Err(WorkerRuntimeError::new(
                WorkerRuntimeErrorKind::NotReady,
                format!(
                    "worker runtime '{}' has no bound profile: configuration required",
                    context.runtime_type.as_str()
                ),
            ));
        };
        if !profile.enabled {
            return Err(WorkerRuntimeError::new(
                WorkerRuntimeErrorKind::NotReady,
                "worker profile is disabled: configuration required",
            ));
        }
        if profile.execution_environment == models::WorkerExecutionEnvironment::Container {
            return Err(WorkerRuntimeError::new(
                WorkerRuntimeErrorKind::Unsupported,
                "container execution is not supported in phase 1; bind a local profile",
            ));
        }
        let mut connection = context
            .resolver
            .validate_connection(profile.connection_id.as_str())
            .await?;
        // LiteLLM Gateway 改写（与 registry 探测路径同一语义）：CLI 看到网关
        // 端口 + 绑定别名，模型名改写与协议转换由网关完成。改写生效时
        // CLI→网关段协议即 CLI 原生协议，上游 provider 协议不再约束本 CLI。
        if let Some(gateway) = super::gateway::global_gateway() {
            if gateway.rewrite_connection(
                context.runtime_type,
                &mut connection.base_url,
                &mut connection.default_model,
            ) && let Some(native) =
                super::gateway::GatewayManager::native_protocol(context.runtime_type)
            {
                connection.protocol = native;
            }
        }
        (profile, connection)
    };

    let built_command = build(&connection, &profile, &request)?;
    // 隔离 config 目录必须真实存在：codex 在 CODEX_HOME 指向不存在路径时
    // 直接报错退出；其余 CLI 也依赖该目录可写。
    let config_dir = worker_config_dir(&request, context.runtime_type);
    std::fs::create_dir_all(&config_dir).map_err(|error| {
        WorkerRuntimeError::new(
            WorkerRuntimeErrorKind::Internal,
            format!(
                "cannot create worker config dir {}: {error}",
                config_dir.display()
            ),
        )
    })?;
    let Some(program) = locate_executable(
        built_command
            .binary_names
            .first()
            .copied()
            .unwrap_or_default(),
        std::env::var(override_env_name(context.runtime_type))
            .ok()
            .as_deref(),
    ) else {
        return Err(WorkerRuntimeError::new(
            WorkerRuntimeErrorKind::NotInstalled,
            format!(
                "executable '{}' not found on PATH (or override env {})",
                built_command
                    .binary_names
                    .first()
                    .copied()
                    .unwrap_or_default(),
                override_env_name(context.runtime_type)
            ),
        ));
    };

    let mut run = WorkerRun::new(
        models::ProjectId::new("worker-standalone".to_string()),
        context.runtime_type,
        request.instruction.clone(),
    );
    if let Some(worker_run_id) = request
        .worker_run_id
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        run.id = worker_run_id.to_string();
    }
    run.profile_id = Some(profile.id.clone());
    run.connection_id = Some(connection.connection_id.clone());
    let model = profile.effective_model(connection.default_model.as_deref());
    run.model = model.map(str::to_string);
    run.execution_environment = profile.execution_environment;
    run.mark_started();

    let mut invocation = WorkerInvocation::new(context.runtime_type, purpose, Some(run.id.clone()));
    invocation.connection_id = run.connection_id.clone();
    invocation.model = run.model.clone();

    let timeout = Duration::from_secs(request.timeout_seconds.max(1));
    // 取消键优先用派发预分配的 worker_run_id：它同时是库里骨架行的 id，
    // API 层的中断端点据此寻址（invocation.id 跑完才可见，中断等不到）。
    let cancel_key = request
        .worker_run_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map_or_else(|| invocation.id.clone(), str::to_string);
    let cancel_receiver = context.inflight.register(&cancel_key);
    // 流量录制注入：代理在运行时（P3）把 HTTP(S)_PROXY + 自签 CA 信任
    // 变量并进子进程环境——CLI 自己发的请求和它 spawn 的 curl/python 才
    // 会流经 MITM。NO_PROXY 保证 LLM 网关（127.0.0.1）不走代理。
    let mut child_env = built_command.env;
    if let Some(traffic) = crate::traffic::global_traffic() {
        child_env.extend(traffic.proxy_env());
    }
    let spec = ProcessSpec {
        program,
        args: built_command.args,
        env: child_env,
        working_dir: request.workdir.clone(),
        timeout,
        stdin_data: built_command.stdin,
    };
    // JSONL 事件流（claude/codex）：逐行脱敏后解析为结构化事件与用量；
    // 解析失败/未知事件行静默忽略（schema 无版本标记，必须容错）。
    let stream_collector = stream.map(|flavor| {
        let shared = Arc::new(std::sync::Mutex::new(StreamCollector::default()));
        let hook = Arc::clone(&shared);
        let provider_key = connection.api_key.clone();
        let mcp_token = request.mcp_bearer_token.clone();
        let session_sink = Arc::clone(&context.session_sink);
        let session_owner = cancel_key.clone();
        let handler: super::process::StreamHandler = Box::new(move |line: &str| {
            let redacted =
                redact_worker_output(line, provider_key.as_deref(), mcp_token.as_deref());
            if let Ok(mut guard) = hook.lock() {
                match flavor {
                    StreamFlavor::Claude => absorb_claude_line(&mut guard, &redacted),
                    StreamFlavor::Codex => absorb_codex_line(&mut guard, &redacted),
                    StreamFlavor::Pi => absorb_pi_line(&mut guard, &redacted),
                }
                // 首次拿到会话引用立即上报 sink：中断定位与 resume 都要
                // 在 worker 还在跑的时候就能按 worker_run_id 找到它。
                if let Some(session_ref) = guard.session_ref.clone() {
                    let already = session_sink
                        .lock()
                        .map(|sink| sink.contains_key(&session_owner))
                        .unwrap_or(true);
                    if !already {
                        tracing::info!(
                            worker_run_id = %session_owner,
                            session_ref = %session_ref,
                            "worker session ref reported to sink"
                        );
                        if let Ok(mut sink) = session_sink.lock() {
                            sink.insert(session_owner.clone(), session_ref);
                        }
                    }
                }
            }
        });
        (shared, handler)
    });
    let shared_collector = stream_collector.as_ref().map(|(shared, _)| Arc::clone(shared));
    let outcome = if let Some((_, mut handler)) = stream_collector {
        super::process::run_process_streaming(&spec, cancel_receiver, move |line: &str| {
            handler(line)
        })
        .await
    } else {
        super::process::run_process_with_cancel(&spec, cancel_receiver).await
    };
    context.inflight.unregister(&cancel_key);
    let outcome = outcome.map_err(|error| {
        let message = format!("process spawn failed: {error}");
        run.finish(WorkerRunStatus::Failed, None);
        run.error = Some(message.clone());
        invocation.finish(WorkerRunStatus::Failed);
        WorkerRuntimeError::new(WorkerRuntimeErrorKind::Unavailable, message)
    })?;

    // transcript 原始字节（有界）：交由派发层密封为工件。
    let transcript = outcome.transcript_bytes();
    run.exit_code = outcome.exit_code;
    let redacted_stdout = redact_worker_output(
        &outcome.stdout,
        connection.api_key.as_deref(),
        request.mcp_bearer_token.as_deref(),
    );
    let redacted_stderr = redact_worker_output(
        &outcome.stderr,
        connection.api_key.as_deref(),
        request.mcp_bearer_token.as_deref(),
    );
    if !redacted_stdout.is_empty() {
        run.record_event(WorkerEventKind::Output, excerpt(&redacted_stdout, 1200));
    }
    if !redacted_stderr.is_empty() {
        run.record_event(WorkerEventKind::Error, excerpt(&redacted_stderr, 800));
    }
    // 流式解析产物：结构化事件先行入列，session 引用与用量覆写粗提取。
    // `stream_final_message` 同步提到外层：超时 / 失败 / 成功三条出口都要
    // 能读到终稿（成功时作为 summary，另两条作为收尾素材）。
    let mut stream_final_message: Option<String> = None;
    let stream_failed = if let Some(shared) = shared_collector.as_ref() {
        match shared.lock() {
            Ok(collected) => {
                for (kind, message) in &collected.events {
                    run.record_event(*kind, message.clone());
                }
                if collected.session_ref.is_some() {
                    run.session_ref = collected.session_ref.clone();
                }
                if collected.usage.is_some() {
                    run.usage = collected.usage.clone();
                }
                stream_final_message = collected.final_message.clone();
                collected.failed
            }
            Err(_) => false,
        }
    } else {
        false
    };

    let outcome_of = |run: WorkerRun, invocation: WorkerInvocation| WorkerExecutionOutcome {
        run,
        invocation,
        transcript: transcript.clone(),
    };

    if outcome.timed_out {
        run.record_event(WorkerEventKind::Error, "process killed after timeout");
        // 超时前 worker 可能已经说过结论（长跑后半段才吐终稿）。留住它，
        // 收尾路径才有东西可展示，而不是只留一个 "timed out"。
        if let Some(text) = stream_final_message.as_deref() {
            invocation.summary = Some(excerpt(text, WORKER_FINAL_MESSAGE_CHARS));
        }
        run.finish(WorkerRunStatus::Timeout, None);
        invocation.finish(WorkerRunStatus::Timeout);
        return Ok(outcome_of(run, invocation));
    }

    if outcome.cancelled {
        run.record_event(
            WorkerEventKind::State,
            "process terminated by cancel signal",
        );
        run.finish(WorkerRunStatus::Cancelled, None);
        invocation.finish(WorkerRunStatus::Cancelled);
        return Ok(outcome_of(run, invocation));
    }

    if !outcome.succeeded() || stream_failed {
        let detail = if redacted_stderr.trim().is_empty() && stream_failed {
            "stream reported failure (see events)".to_string()
        } else {
            excerpt(&redacted_stderr, 800)
        };
        run.error = Some(detail.clone());
        // 失败也要留住终稿：orchestrator 的收尾路径靠它给用户一句"做到
        // 哪一步、为什么停"，否则失败 run 在用户眼前是一片空白。无流式
        // 终稿的 runtime（DSH one-shot：stdout 即纯答案，2026-09-23 实跑
        // 确认）退到 stdout 兜底，与成功路径同构。
        if let Some(text) = stream_final_message.as_deref() {
            invocation.summary = Some(excerpt(text, WORKER_FINAL_MESSAGE_CHARS));
        } else if !redacted_stdout.trim().is_empty() {
            invocation.summary = Some(excerpt(&redacted_stdout, WORKER_FINAL_MESSAGE_CHARS));
        }
        run.finish(WorkerRunStatus::Failed, None);
        invocation.error = Some(detail);
        invocation.finish(WorkerRunStatus::Failed);
        return Ok(outcome_of(run, invocation));
    }

    // 成功路径：adapter 提取 session 引用与受控摘要。**终稿优先**——worker
    // 最后一句面向用户的纯文本才是要回给用户的内容；stdout 粗提取只是流式
    // 解析没拿到终稿时的兜底（stdout 含 JSON 事件噪声，直接展示不可读）。
    if run.session_ref.is_none() {
        run.session_ref = extract_session_ref(&redacted_stdout);
    }
    let summary = match stream_final_message.as_deref() {
        Some(text) => excerpt(text, WORKER_FINAL_MESSAGE_CHARS),
        None => excerpt(&redacted_stdout, 2000),
    };
    invocation.summary = Some(summary.clone());
    // finish 的第二个参数负责落 summary（内部截断到 MAX_WORKER_SUMMARY_CHARS）。
    run.finish(WorkerRunStatus::Succeeded, Some(summary));
    invocation.finish(WorkerRunStatus::Succeeded);
    Ok(outcome_of(run, invocation))
}

/// 终稿文本落 `WorkerInvocation.summary` 的上界。比事件摘录（300-600）宽，
/// 比裸 stdout 兜底（2000）同量级：够装一句完整结论，又不把整条 stdout
/// 灌进审计列。
pub(crate) const WORKER_FINAL_MESSAGE_CHARS: usize = 2000;

fn excerpt(text: &str, max: usize) -> String {
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out: String = compact.chars().take(max).collect();
    if compact.chars().count() > max {
        out.push('…');
    }
    out
}

fn redact_worker_output(text: &str, provider_key: Option<&str>, mcp_token: Option<&str>) -> String {
    let mut redacted = crate::model_providers::redact_secrets(text);
    for secret in [provider_key, mcp_token].into_iter().flatten() {
        if !secret.is_empty() {
            redacted = redacted.replace(secret, "********");
        }
    }
    redacted
}

// ── JSONL 事件流：实时解析为结构化事件与用量 ─────────────────────────────

/// 支持 JSONL 事件流的 runtime（流式解析器选择）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamFlavor {
    Claude,
    Codex,
    Pi,
}

/// 单次执行的流式收集产物（session 引用 / 结构化事件 / 用量 / 终稿文本）。
///
/// `final_message` 是 worker 本轮**最后一句面向用户的纯文本**（Codex 的
/// `agent_message` item / Claude Code 的 `result.result`）。它与 `events`
/// 里的 Output 摘录是两回事：摘录是给人看的执行痕迹，有界且可能被截；
/// 终稿是**要回给用户的那一句**，必须完整留住，否则外部 worker 跑完了
/// 但用户眼前一句话都没有（settlement 契约）。
#[derive(Default)]
struct StreamCollector {
    session_ref: Option<String>,
    usage: Option<models::WorkerUsage>,
    events: Vec<(WorkerEventKind, String)>,
    /// 终稿纯文本（最后一次 agent_message / result）。有界化在落库处做。
    final_message: Option<String>,
    failed: bool,
}

impl StreamCollector {
    fn record(&mut self, kind: WorkerEventKind, message: impl Into<String>) {
        // 事件在 commit 路径经 run.record_event 再次有界化。
        self.events.push((kind, message.into()));
    }

    /// 记住终稿文本。后到的终稿覆盖先到的（worker 可能分多轮发言，最后
    /// 一句才是本轮结论）；空文本不覆盖已有值。
    fn record_final_message(&mut self, text: &str) {
        if text.trim().is_empty() {
            return;
        }
        self.final_message = Some(text.to_string());
    }
}

/// 解析一行 Claude Code `--output-format stream-json --verbose` 事件。
fn absorb_claude_line(collector: &mut StreamCollector, line: &str) {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        return;
    };
    let kind = value.get("type").and_then(serde_json::Value::as_str);
    match kind {
        Some("system") => {
            if value.get("subtype").and_then(serde_json::Value::as_str) == Some("init") {
                if let Some(session) = value.get("session_id").and_then(as_str_opt) {
                    collector.session_ref = Some(session.to_string());
                }
                let model = value
                    .get("model")
                    .and_then(as_str_opt)
                    .unwrap_or("unknown-model");
                collector.record(
                    WorkerEventKind::State,
                    format!("session ready (model {model})"),
                );
            }
        }
        Some("assistant") => {
            if let Some(content) = value
                .pointer("/message/content")
                .and_then(serde_json::Value::as_array)
            {
                for block in content {
                    match block.get("type").and_then(as_str_opt) {
                        Some("text") => {
                            let text = block.get("text").and_then(as_str_opt).unwrap_or("");
                            if !text.trim().is_empty() {
                                collector.record(WorkerEventKind::Output, excerpt(text, 300));
                            }
                        }
                        Some("tool_use") => {
                            let name =
                                block.get("name").and_then(as_str_opt).unwrap_or("unknown");
                            collector
                                .record(WorkerEventKind::State, format!("tool {name} started"));
                        }
                        _ => {}
                    }
                }
            }
        }
        Some("user") => {
            if let Some(content) = value
                .pointer("/message/content")
                .and_then(serde_json::Value::as_array)
            {
                for block in content {
                    if block.get("type").and_then(as_str_opt) == Some("tool_result") {
                        collector.record(WorkerEventKind::Output, "tool finished");
                    }
                }
            }
        }
        Some("result") => {
            collector.usage = Some(claude_usage_of(&value));
            // 终稿文本（advise/问答类 run 的答案载体；普通 run 同样要回给
            // 用户——orchestrator 通过 WorkerInvocation.summary 消费，不再
            // 被忽略）。截断语义与其他 Output 事件一致。
            if let Some(text) = value
                .get("result")
                .and_then(as_str_opt)
                .filter(|text| !text.trim().is_empty())
            {
                collector.record_final_message(text);
                collector.record(WorkerEventKind::Output, excerpt(text, 600));
            }
            let subtype = value
                .get("subtype")
                .and_then(as_str_opt)
                .unwrap_or("unknown");
            if value.get("is_error").and_then(serde_json::Value::as_bool) == Some(true)
                || subtype.starts_with("error_")
            {
                collector.failed = true;
                collector.record(WorkerEventKind::Error, format!("result: {subtype}"));
            } else {
                collector.record(WorkerEventKind::State, format!("result: {subtype}"));
            }
        }
        _ => {}
    }
}

/// Claude result 事件 → [`models::WorkerUsage`]。
/// token 口径优先 `modelUsage`（全量计费视图），回退 `usage`（主循环）。
fn claude_usage_of(result: &serde_json::Value) -> models::WorkerUsage {
    let mut usage = models::WorkerUsage::default();
    usage.num_turns = result
        .get("num_turns")
        .and_then(serde_json::Value::as_i64);
    usage.duration_api_ms = result
        .get("duration_api_ms")
        .and_then(serde_json::Value::as_i64);
    usage.cost_usd = result
        .get("total_cost_usd")
        .and_then(serde_json::Value::as_f64);
    if let Some(model_usage) = result.get("modelUsage").and_then(serde_json::Value::as_object) {
        for (model, entry) in model_usage {
            usage.requested_model.get_or_insert_with(|| model.clone());
            usage.input_tokens += entry.get("input_tokens").and_then(as_i64).unwrap_or(0);
            usage.output_tokens += entry.get("output_tokens").and_then(as_i64).unwrap_or(0);
            usage.cached_input_tokens += entry
                .get("cache_read_input_tokens")
                .and_then(as_i64)
                .unwrap_or(0);
            usage
                .cost_usd
                .get_or_insert(entry.get("cost_usd").and_then(serde_json::Value::as_f64).unwrap_or(0.0));
        }
    }
    if usage.input_tokens == 0 && usage.output_tokens == 0 {
        if let Some(base) = result.get("usage").and_then(serde_json::Value::as_object) {
            usage.input_tokens = base.get("input_tokens").and_then(as_i64).unwrap_or(0);
            usage.output_tokens = base.get("output_tokens").and_then(as_i64).unwrap_or(0);
            usage.cached_input_tokens = base
                .get("cache_read_input_tokens")
                .and_then(as_i64)
                .unwrap_or(0);
        }
    }
    usage
}

/// 解析一行 Codex `exec --json` 事件。
fn absorb_codex_line(collector: &mut StreamCollector, line: &str) {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        return;
    };
    match value.get("type").and_then(as_str_opt) {
        Some("thread.started") => {
            if let Some(thread) = value.get("thread_id").and_then(as_str_opt) {
                collector.session_ref = Some(thread.to_string());
            }
        }
        Some("item.started") | Some("item.completed") => {
            let item = value.get("item");
            let item_type = item
                .and_then(|item| item.get("type"))
                .and_then(as_str_opt)
                .unwrap_or("");
            match item_type {
                "command_execution" => {
                    let command = item
                        .and_then(|item| item.get("command"))
                        .and_then(as_str_opt)
                        .unwrap_or("");
                    let exit = item
                        .and_then(|item| item.get("exit_code"))
                        .and_then(as_i64);
                    collector.record(
                        WorkerEventKind::State,
                        format!("cmd: {}", excerpt(command, 160)),
                    );
                    if value.get("type").and_then(as_str_opt) == Some("item.completed") {
                        collector.record(
                            WorkerEventKind::Output,
                            format!(
                                "cmd finished (exit {})",
                                exit.map_or_else(|| "n/a".to_string(), |code| code.to_string())
                            ),
                        );
                    }
                }
                "agent_message" => {
                    if value.get("type").and_then(as_str_opt) == Some("item.completed") {
                        let text = item
                            .and_then(|item| item.get("text"))
                            .and_then(as_str_opt)
                            .unwrap_or("");
                        // 终稿完整留住（供 orchestrator 回给用户），事件流仍留
                        // 有界摘录（供人看执行痕迹）。
                        collector.record_final_message(text);
                        collector.record(WorkerEventKind::Output, excerpt(text, 300));
                    }
                }
                "mcp_tool_call" => {
                    let tool = item
                        .and_then(|item| item.get("tool"))
                        .and_then(as_str_opt)
                        .unwrap_or("unknown");
                    collector.record(
                        WorkerEventKind::State,
                        format!("mcp tool {tool} {}",
                            value.get("type").and_then(as_str_opt).unwrap_or("")),
                    );
                }
                "file_change" => {
                    let count = item
                        .and_then(|item| item.get("changes"))
                        .and_then(serde_json::Value::as_array)
                        .map(|changes| changes.len())
                        .unwrap_or(0);
                    collector.record(
                        WorkerEventKind::State,
                        format!("file change ({count} path(s))"),
                    );
                }
                _ => {}
            }
        }
        Some("turn.completed") => {
            let usage_json = value.get("usage");
            let mut usage = models::WorkerUsage::default();
            if let Some(base) = usage_json.and_then(serde_json::Value::as_object) {
                usage.input_tokens = base.get("input_tokens").and_then(as_i64).unwrap_or(0);
                usage.output_tokens = base.get("output_tokens").and_then(as_i64).unwrap_or(0);
                usage.cached_input_tokens = base
                    .get("cached_input_tokens")
                    .and_then(as_i64)
                    .unwrap_or(0);
                usage.reasoning_tokens = base
                    .get("reasoning_output_tokens")
                    .and_then(as_i64)
                    .unwrap_or(0);
            }
            // Codex JSONL 不上报美元成本：cost_usd 保持 None，绝不伪造。
            collector.usage = Some(usage);
            collector.record(WorkerEventKind::State, "turn completed");
        }
        Some("turn.failed") => {
            collector.failed = true;
            let message = value
                .pointer("/error/message")
                .and_then(as_str_opt)
                .unwrap_or("turn failed");
            collector.record(WorkerEventKind::Error, excerpt(message, 300));
        }
        Some("error") => {
            let message = value.get("message").and_then(as_str_opt).unwrap_or("");
            // "Reconnecting..." 类重连提示是非致命 notice。
            if message.starts_with("Reconnecting") {
                collector.record(WorkerEventKind::Notice, excerpt(message, 200));
            } else {
                collector.record(WorkerEventKind::Error, excerpt(message, 300));
            }
        }
        _ => {}
    }
}

/// 解析一行 Pi `--mode json` 事件（earendil-works/pi JSONL）。
fn absorb_pi_line(collector: &mut StreamCollector, line: &str) {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        return;
    };
    match value.get("type").and_then(as_str_opt) {
        Some("session") => {
            if let Some(session) = value.get("id").and_then(as_str_opt) {
                collector.session_ref = Some(session.to_string());
            }
            collector.record(WorkerEventKind::State, "session started");
        }
        Some("tool_execution_start") => {
            let name = value
                .get("toolName")
                .and_then(as_str_opt)
                .unwrap_or("unknown");
            collector.record(WorkerEventKind::State, format!("tool {name} started"));
        }
        Some("tool_execution_end") => {
            let name = value
                .get("toolName")
                .and_then(as_str_opt)
                .unwrap_or("unknown");
            let is_error = value
                .get("isError")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            if is_error {
                collector.record(WorkerEventKind::Error, format!("tool {name} failed"));
            } else {
                collector.record(WorkerEventKind::Output, format!("tool {name} finished"));
            }
        }
        Some("message_update") => {
            if let Some(usage) = value.get("usage") {
                collector.usage.get_or_insert_with(|| pi_usage_of(usage));
            }
        }
        Some("message_end") => {
            if let Some(usage) = value.pointer("/message/usage") {
                collector.usage = Some(pi_usage_of(usage));
            }
            if let Some(text) = pi_message_text(value.get("message")) {
                // 终稿必须留住全文：这里拿到的已经是完整 assistant 消息
                // （`pi_message_text` 拼接全部 text block，非摘录）。此前只
                // `record(Output, excerpt(text, 300))`——300 字符截录进事件
                // 流，**全文随即丢弃**，于是 pi run 的 summary 只能退化成
                // stdout 粗提取（对 JSONL CLI 就是一串协议噪声）。
                //
                // 事件照旧记（它是执行痕迹），但 final_message 另存一份。
                // pi 可能分多轮发言，`record_final_message` 后到覆盖先到，
                // 最后一个 message_end 即终稿。
                collector.record_final_message(&text);
                collector.record(WorkerEventKind::Output, excerpt(&text, 300));
            }
            let stop = value
                .pointer("/message/stopReason")
                .and_then(as_str_opt)
                .unwrap_or("");
            if stop == "error" {
                collector.failed = true;
                collector.record(WorkerEventKind::Error, "assistant message errored");
            }
        }
        Some("agent_end") => collector.record(WorkerEventKind::State, "agent finished"),
        Some("extension_error") => {
            collector.record(WorkerEventKind::Error, "extension error");
        }
        _ => {}
    }
}

/// Pi usage 形状：{input, output, cacheRead, cacheWrite, cost:{...}}。
/// cost 取 `total` 字段（缺失则 None，绝不伪造）。
fn pi_usage_of(usage: &serde_json::Value) -> models::WorkerUsage {
    let mut out = models::WorkerUsage::default();
    out.input_tokens = usage.get("input").and_then(as_i64).unwrap_or(0);
    out.output_tokens = usage.get("output").and_then(as_i64).unwrap_or(0);
    out.cached_input_tokens = usage.get("cacheRead").and_then(as_i64).unwrap_or(0);
    if let Some(cost) = usage.get("cost") {
        out.cost_usd = cost
            .get("total")
            .and_then(serde_json::Value::as_f64)
            .or_else(|| cost.as_f64());
    }
    out
}

/// 提取 Pi message 里的最终文本块。
fn pi_message_text(message: Option<&serde_json::Value>) -> Option<String> {
    let blocks = message?.get("content")?.as_array()?;
    let mut text = String::new();
    for block in blocks {
        if block.get("type").and_then(as_str_opt) == Some("text")
            && let Some(part) = block.get("text").and_then(as_str_opt)
        {
            text.push_str(part);
        }
    }
    if text.trim().is_empty() {
        None
    } else {
        Some(text)
    }
}

fn as_str_opt(value: &serde_json::Value) -> Option<&str> {
    value.as_str()
}

fn as_i64(value: &serde_json::Value) -> Option<i64> {
    value.as_i64()
}

/// 通用探测流程：二进制发现 + 版本 probe（显式覆盖 env 优先）。
async fn probe(
    context: &AdapterContext,
    version_args: &[&str],
    parse_version: impl Fn(&str) -> Option<String>,
    capabilities: Vec<String>,
) -> WorkerProbe {
    let override_path = std::env::var(override_env_name(context.runtime_type)).ok();
    let Some(program) = locate_executable(
        context.binary_names.first().copied().unwrap_or_default(),
        override_path.as_deref(),
    ) else {
        let mut probe = WorkerProbe::new(
            context.runtime_type,
            WorkerAvailability::NotInstalled,
            capabilities,
        );
        probe.detail = Some(format!(
            "executable '{}' not found on PATH (or override env {})",
            context.binary_names.first().copied().unwrap_or_default(),
            override_env_name(context.runtime_type)
        ));
        return probe;
    };
    let spec = ProcessSpec {
        // clone：`program` 后面还要用于探测失败的诊断信息（哪条命令、
        // 什么输出），不能在这里被 move 掉。
        program: program.clone(),
        args: version_args
            .iter()
            .map(|value| (*value).to_string())
            .collect(),
        env: BTreeMap::new(),
        working_dir: None,
        timeout: Duration::from_secs(15),
        stdin_data: None,
    };
    let outcome = match run_process(&spec).await {
        Ok(outcome) => outcome,
        Err(error) => {
            let mut probe = WorkerProbe::new(
                context.runtime_type,
                WorkerAvailability::Unavailable,
                capabilities,
            );
            probe.detail = Some(format!("version probe failed to start: {error}"));
            return probe;
        }
    };
    let mut probe = WorkerProbe::new(
        context.runtime_type,
        WorkerAvailability::Available,
        capabilities,
    );
    if outcome.timed_out {
        probe.availability = WorkerAvailability::Unavailable;
        probe.detail = Some("version probe timed out".to_string());
        return probe;
    }
    let combined = if outcome.stdout.trim().is_empty() {
        outcome.stderr.clone()
    } else {
        outcome.stdout.clone()
    };
    match parse_version(&combined) {
        Some(version) => {
            probe.version = Some(version);
            probe
        }
        None => {
            probe.availability = WorkerAvailability::Unsupported;
            // 探测跑过了但解析不出版本。这里必须把真实输出带出来：
            // `'node' is not recognized`（PATHEXT 被砍，CLI 根本没起来）和
            // "版本号长得不认识" 是两件完全不同的事。早先一律报成
            // "protocol compatibility is unknown (developer preview gate:
            //  refusing to guess)"，把环境故障误导成协议不兼容——让人以为
            // 要改协议适配，实际只要修子进程环境。协议映射在
            // `native_protocol()`，与版本字符串无关。
            let observed = first_non_empty_line(&combined).unwrap_or_else(|| "<no output>".to_string());
            probe.detail = Some(format!(
                "version probe could not be parsed from `{} {}`: {observed}; \
                 protocol compatibility is unknown (developer preview gate: refusing to guess)",
                program.display(),
                version_args.join(" ")
            ));
            probe
        }
    }
}

/// 取输出的第一行非空内容（探测失败时用于呈现真实原因）。
fn first_non_empty_line(output: &str) -> Option<String> {
    output
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(|line| {
            if line.chars().count() > 200 {
                line.chars().take(200).collect::<String>() + "…"
            } else {
                line.to_string()
            }
        })
}

// ---------------------------------------------------------------------------
// Claude Code
// ---------------------------------------------------------------------------

/// Claude Code CLI（headless `-p` 模式，Anthropic 协议）。
pub struct ClaudeCodeWorker {
    context: AdapterContext,
}

impl ClaudeCodeWorker {
    /// 构造（resolver 注入 Connection 解析）。
    #[must_use]
    pub fn new(resolver: Arc<dyn agents::worker::WorkerConnectionResolver>) -> Self {
        Self {
            context: AdapterContext {
                runtime_type: WorkerRuntimeType::ClaudeCode,
                binary_names: &["claude"],
                resolver,
                inflight: Arc::new(InflightRegistry::default()),
                session_sink: session_sink_arc(),
            },
        }
    }
}

#[async_trait]
impl WorkerRuntime for ClaudeCodeWorker {
    fn runtime_type(&self) -> WorkerRuntimeType {
        WorkerRuntimeType::ClaudeCode
    }

    async fn probe(&self) -> WorkerProbe {
        probe(
            &self.context,
            &["--version"],
            |output| parse_semver_prefix(output, "claude"),
            vec![
                "headless-print".to_string(),
                "json-output".to_string(),
                "session-resume".to_string(),
                "transcript".to_string(),
            ],
        )
        .await
    }

    fn capabilities(&self) -> Vec<String> {
        vec![
            "headless-print".to_string(),
            "json-output".to_string(),
            "session-resume".to_string(),
        ]
    }

    async fn start(
        &self,
        request: WorkerExecutionRequest,
    ) -> Result<WorkerExecutionOutcome, WorkerRuntimeError> {
        execute(
            &self.context,
            request,
            WorkerInvocationPurpose::Start,
            |connection, profile, request| {
                if connection.protocol != ProviderType::Anthropic {
                    return Err(WorkerRuntimeError::new(
                        WorkerRuntimeErrorKind::Unsupported,
                        format!(
                            "claude code cli requires an anthropic-compatible connection, got '{}'",
                            connection.protocol.as_str()
                        ),
                    ));
                }
                let Some(model) = profile.effective_model(connection.default_model.as_deref())
                else {
                    return Err(WorkerRuntimeError::new(
                        WorkerRuntimeErrorKind::NotReady,
                        "no model configured: set model_override on the profile or default_model \
                     on the connection",
                    ));
                };
                // start/resume 共用同一命令构造器：provider/model/MCP/config
                // 的差异只能来自可信的 WorkerExecutionRequest。
                let args = claude_args(model, request)?;
                Ok(BuiltCommand {
                    binary_names: self.context.binary_names,
                    args,
                    env: anthropic_env_for_request(connection, request, Some(model)),
                    stdin: Some(request.instruction.clone()),
                })
            },
            Some(StreamFlavor::Claude),
        )
        .await
    }

    async fn resume(
        &self,
        request: WorkerExecutionRequest,
    ) -> Result<WorkerExecutionOutcome, WorkerRuntimeError> {
        execute(
            &self.context,
            request,
            WorkerInvocationPurpose::Resume,
            |connection, profile, request| {
                if connection.protocol != ProviderType::Anthropic {
                    return Err(WorkerRuntimeError::new(
                        WorkerRuntimeErrorKind::Unsupported,
                        "claude code cli requires an anthropic-compatible connection",
                    ));
                }
                let Some(model) = profile.effective_model(connection.default_model.as_deref())
                else {
                    return Err(WorkerRuntimeError::new(
                        WorkerRuntimeErrorKind::NotReady,
                        "no model configured",
                    ));
                };
                let Some(_session_ref) = request.session_ref.as_ref() else {
                    return Err(WorkerRuntimeError::new(
                        WorkerRuntimeErrorKind::NotReady,
                        "resume requires a session reference",
                    ));
                };
                let args = claude_args(model, request)?;
                Ok(BuiltCommand {
                    binary_names: self.context.binary_names,
                    args,
                    env: anthropic_env_for_request(connection, request, Some(model)),
                    stdin: Some(request.instruction.clone()),
                })
            },
            Some(StreamFlavor::Claude),
        )
        .await
    }

    async fn events(
        &self,
        session_ref: &str,
    ) -> Result<Vec<models::worker::WorkerEvent>, WorkerRuntimeError> {
        // 单次 headless 调用没有独立事件流；事件在 run 记录内持久化。
        let _ = session_ref;
        Ok(Vec::new())
    }

    async fn cancel(&self, session_ref: &str) -> Result<bool, WorkerRuntimeError> {
        Ok(self.context.inflight.cancel(session_ref))
    }
}

/// Anthropic 协议环境变量映射（adapter 私有；对齐 Claude Code 官方
/// 环境变量接口）。密钥只在启动注入，绝不落日志/事件/prompt。
///
/// **登录态隔离红线**：`CLAUDE_CONFIG_DIR` 强制指向干净目录——否则
/// Claude Code 回退到 `~/.claude` 并加载其 settings.json env 块（用户
/// 本机 CLI 登录/OAuth/网关指向），认证来源就不再只是 Lynceus
/// Connection。`IS_SANDBOX` 跳过首次信任对话。
fn anthropic_env_for_request(
    connection: &ResolvedWorkerConnection,
    request: &WorkerExecutionRequest,
    model: Option<&str>,
) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    if let Some(base_url) = connection
        .base_url
        .as_deref()
        .filter(|url| !url.trim().is_empty())
    {
        env.insert("ANTHROPIC_BASE_URL".to_string(), base_url.to_string());
    }
    if let Some(api_key) = connection
        .api_key
        .as_deref()
        .filter(|key| !key.trim().is_empty())
    {
        env.insert("ANTHROPIC_AUTH_TOKEN".to_string(), api_key.to_string());
        env.insert("ANTHROPIC_API_KEY".to_string(), api_key.to_string());
    }
    env.insert(
        "CLAUDE_CONFIG_DIR".to_string(),
        worker_config_dir(request, WorkerRuntimeType::ClaudeCode)
            .to_string_lossy()
            .into_owned(),
    );
    insert_isolated_home(&mut env, request, WorkerRuntimeType::ClaudeCode);
    if let Some(token) = mcp_token(request) {
        env.insert("LYNCEUS_MCP_TOKEN".to_string(), token);
    }
    env.insert("IS_SANDBOX".to_string(), "1".to_string());
    // 网关模型发现：Connection 的 base_url 背后可能是任意 Anthropic
    // 兼容网关，模型名不受 Claude Code 内置白名单约束。开启 discovery
    // 后 CLI 接受网关自定义模型名（`--model` 仍必须显式传递——无
    // discovery 时未知模型名会被 CLI 回退为内置默认模型）。
    env.insert(
        "CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY".to_string(),
        "1".to_string(),
    );
    env.insert(
        "CLAUDE_CODE_PROVIDER_MANAGED_BY_HOST".to_string(),
        "1".to_string(),
    );
    // 关闭非必要遥测/统计请求：这些请求与推理同源（打网关/中转站），
    // 而中转站普遍只放行 /v1/messages，遥测会被 403 并污染事件流。
    env.insert(
        "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC".to_string(),
        "1".to_string(),
    );
    // 模型档位映射：后台辅助调用（会话标题生成、子代理等）不读
    // `--model`，而是按内置 fable/opus/sonnet/haiku 档位解析。网关自定义
    // 模型名（GLM/Kimi/DeepSeek 等 Anthropic 兼容集成）必须占满全部档
    // 位，否则辅助调用会以 unrecognized_model 失败。
    if let Some(model) = model.filter(|model| !model.trim().is_empty()) {
        for key in [
            "ANTHROPIC_MODEL",
            "ANTHROPIC_DEFAULT_FABLE_MODEL",
            "ANTHROPIC_DEFAULT_OPUS_MODEL",
            "ANTHROPIC_DEFAULT_SONNET_MODEL",
            "ANTHROPIC_DEFAULT_HAIKU_MODEL",
            "CLAUDE_CODE_SUBAGENT_MODEL",
        ] {
            env.insert(key.to_string(), model.to_string());
        }
    }
    env
}

fn anthropic_env(connection: &ResolvedWorkerConnection) -> BTreeMap<String, String> {
    let request = WorkerExecutionRequest::start("", DEFAULT_TIMEOUT_SECONDS);
    anthropic_env_for_request(connection, &request, connection.default_model.as_deref())
}

/// 根据 Worker 请求取得稳定的隔离 CLI home/config。start 与 resume 只要
/// 复用同一个 `config_dir` 就会使用同一个 session 配置；缺省时不回退到
/// 用户 home，而是落在 Lynceus data/workspace 下的 runtime 目录。
fn worker_config_dir(
    request: &WorkerExecutionRequest,
    runtime: WorkerRuntimeType,
) -> std::path::PathBuf {
    request.config_dir.clone().unwrap_or_else(|| {
        let base = request
            .workdir
            .clone()
            .or_else(|| std::env::var_os("LYNCEUS_WORKSPACE_DIR").map(std::path::PathBuf::from))
            .unwrap_or_else(|| std::path::PathBuf::from("data"));
        base.join(".lynceus-worker").join(runtime.as_str())
    })
}

fn insert_isolated_home(
    env: &mut BTreeMap<String, String>,
    request: &WorkerExecutionRequest,
    runtime: WorkerRuntimeType,
) {
    let home = worker_config_dir(request, runtime);
    let home = home.to_string_lossy().into_owned();
    env.insert("HOME".to_string(), home.clone());
    env.insert("USERPROFILE".to_string(), home.clone());
    env.insert("XDG_CONFIG_HOME".to_string(), home.clone());
    env.insert("APPDATA".to_string(), home.clone());
    env.insert("LOCALAPPDATA".to_string(), home);
}

fn mcp_token(request: &WorkerExecutionRequest) -> Option<String> {
    request
        .mcp_bearer_token
        .clone()
        .or_else(|| std::env::var("LYNCEUS_MCP_TOKEN").ok())
        .filter(|value| !value.trim().is_empty())
}

fn mcp_url(request: &WorkerExecutionRequest) -> Option<String> {
    request
        .mcp_url
        .clone()
        .or_else(|| std::env::var("LYNCEUS_MCP_URL").ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn mcp_connection(
    request: &WorkerExecutionRequest,
) -> Result<Option<(String, String)>, WorkerRuntimeError> {
    match (mcp_url(request), mcp_token(request)) {
        (None, None) => Ok(None),
        (Some(url), Some(token)) => Ok(Some((url, token))),
        (Some(_), None) | (None, Some(_)) => Err(WorkerRuntimeError::new(
            WorkerRuntimeErrorKind::NotReady,
            "Lynceus MCP requires both endpoint and short-lived Worker grant",
        )),
    }
}

/// 生成 lynceus-mcp 注入配置（`--mcp-config` 文件）。配置文件只含
/// endpoint 和环境变量引用；短期 bearer 只进入当前子进程环境，绝不写入
/// 共享明文 MCP 配置。
///
/// 统一工具入口：所有 CLI 共用同一个 MCP server；CLI 适配器只负责注入。
fn lynceus_mcp_config_path(
    request: &WorkerExecutionRequest,
) -> Result<Option<std::path::PathBuf>, WorkerRuntimeError> {
    let Some((url, _token)) = mcp_connection(request)? else {
        return Ok(None);
    };
    let config_dir = worker_config_dir(request, WorkerRuntimeType::ClaudeCode);
    std::fs::create_dir_all(&config_dir).map_err(|error| {
        WorkerRuntimeError::new(
            WorkerRuntimeErrorKind::Internal,
            format!("failed to create isolated Claude config directory: {error}"),
        )
    })?;
    let config = serde_json::json!({
        "mcpServers": {
            "lynceus": {
                "type": "http",
                "url": url,
                "headers": {"Authorization": "Bearer ${LYNCEUS_MCP_TOKEN}"},
            }
        }
    });
    let path = config_dir.join("lynceus-mcp.json");
    std::fs::write(&path, config.to_string()).map_err(|error| {
        WorkerRuntimeError::new(
            WorkerRuntimeErrorKind::Internal,
            format!("failed to write isolated Claude MCP config: {error}"),
        )
    })?;
    Ok(Some(path))
}

fn claude_args(
    model: &str,
    request: &WorkerExecutionRequest,
) -> Result<Vec<String>, WorkerRuntimeError> {
    // 指令经 stdin 传入（`claude -p` 无位置参数时读 stdin）；Windows 上
    // 多行 argv 途经 cmd shim 会截断，故不把 prompt 放入 argv。
    let mut args = vec![
        "-p".to_string(),
        "--output-format".to_string(),
        "stream-json".to_string(),
        "--verbose".to_string(),
        "--model".to_string(),
        model.to_string(),
    ];
    if let Some(session_ref) = request.session_ref.as_ref() {
        args.push("--resume".to_string());
        args.push(session_ref.clone());
    }
    let mcp_config_path = lynceus_mcp_config_path(request)?;
    if let Some(config_path) = mcp_config_path.as_ref() {
        args.push("--mcp-config".to_string());
        args.push(config_path.to_string_lossy().into_owned());
        args.push("--strict-mcp-config".to_string());
    }
    // headless 模式下 CLI 默认审批会静默阻塞 MCP 调用（实测顾问回答
    // "没有权限"）与 Bash 类执行工具（审计 worker 必须能跑命令），统一
    // 显式放行。cwd 是 Mission 工作区（仓库外），Read/Edit/Glob 的项目级
    // 免审批面因此不覆盖 Lynceus 源码与 data/。
    let mut allowed = vec![
        "Bash".to_string(),
        "Read".to_string(),
        "Edit".to_string(),
        "Write".to_string(),
        "Grep".to_string(),
        "Glob".to_string(),
    ];
    if mcp_config_path.is_some() {
        allowed.push("mcp__lynceus__*".to_string());
    }
    args.push("--allowedTools".to_string());
    args.push(allowed.join(","));
    // 命令禁则（真拦截层）：`--disallowedTools` 的 Bash 前缀模式在
    // Claude Code 进程内、工具执行前生效——这是四个黑盒 CLI 里唯一有
    // per-command 原生 deny 面的。deny 胜过 allow：上面放行的 Bash
    // 仍受这些前缀约束。
    if !request.denied_command_prefixes.is_empty() {
        args.push("--disallowedTools".to_string());
        args.push(
            request
                .denied_command_prefixes
                .iter()
                .map(|prefix| format!("Bash({prefix} *)"))
                .collect::<Vec<_>>()
                .join(","),
        );
    }
    Ok(args)
}

// ---------------------------------------------------------------------------
// Codex
// ---------------------------------------------------------------------------

/// Codex CLI（`codex exec` 非交互模式，OpenAI 协议族）。
///
/// 全放行启动：`--dangerously-bypass-approvals-and-sandbox`。理由见
/// [`FULL_ACCESS_FLAG`] 与 `codex_args` 内的注释——headless 下 CLI 的审批
/// 与 Windows 提权沙箱 helper 都会把 worker 变成哑巴。
pub struct CodexWorker {
    context: AdapterContext,
}

impl CodexWorker {
    /// 构造。
    #[must_use]
    pub fn new(resolver: Arc<dyn agents::worker::WorkerConnectionResolver>) -> Self {
        Self {
            context: AdapterContext {
                runtime_type: WorkerRuntimeType::Codex,
                binary_names: &["codex"],
                resolver,
                inflight: Arc::new(InflightRegistry::default()),
                session_sink: session_sink_arc(),
            },
        }
    }
}

fn codex_protocol_supported(protocol: ProviderType) -> bool {
    matches!(
        protocol,
        ProviderType::Openai | ProviderType::OpenaiCompatible
    )
}

/// 各 adapter 对 Connection 协议的静态兼容性（`start()` 内协议校验的镜像）。
///
/// 探测期即暴露「绑定连接协议不受支持」（[`WorkerAvailability::Unsupported`]），
/// 而不是等到任务派发才失败。dsh 双协议皆可（运行时按协议选择环境映射）。
#[must_use]
pub fn adapter_supports_protocol(
    runtime_type: WorkerRuntimeType,
    protocol: ProviderType,
) -> bool {
    match runtime_type {
        WorkerRuntimeType::ClaudeCode => protocol == ProviderType::Anthropic,
        WorkerRuntimeType::Codex => codex_protocol_supported(protocol),
        WorkerRuntimeType::Pi | WorkerRuntimeType::DeepSeekHarness => true,
    }
}

fn openai_env(connection: &ResolvedWorkerConnection) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    if let Some(api_key) = connection
        .api_key
        .as_deref()
        .filter(|key| !key.trim().is_empty())
    {
        env.insert("OPENAI_API_KEY".to_string(), api_key.to_string());
    }
    env
}

fn openai_env_for_worker(
    connection: &ResolvedWorkerConnection,
    request: &WorkerExecutionRequest,
) -> BTreeMap<String, String> {
    let mut env = openai_env(connection);
    insert_isolated_home(&mut env, request, WorkerRuntimeType::Codex);
    if let Some(token) = mcp_token(request) {
        env.insert("LYNCEUS_MCP_TOKEN".to_string(), token);
    }
    env.insert(
        "CODEX_HOME".to_string(),
        worker_config_dir(request, WorkerRuntimeType::Codex)
            .to_string_lossy()
            .into_owned(),
    );
    env
}

fn toml_string(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

/// 预写隔离 `CODEX_HOME` 的最小 `config.toml`。
///
/// 只写 Windows 沙箱 helper 的提权档位，不含任何密钥。合法值只有
/// `elevated` / `unelevated`。
///
/// 历史上这里是 `elevated`（codex 0.150.1 实测：没有 `[windows] sandbox`
/// 时 workspace-write 沙箱会静默降级 read-only、`--sandbox` flag 被无视）。
/// 现在 `codex_args` 走全放行，不再有 workspace-write 沙箱；而 `elevated`
/// 会请回一个提权 helper，headless 子进程里 UAC 无人应答，`exec_command`
/// 直接以 Win32 1223 失败——实测 pwsh 与 git bash 两条路都是这个错。
/// 因此固定 `unelevated`。
///
/// Codex CLI 全放行开关：跳过所有确认提示、不做沙箱。
///
/// `codex exec --help` 原文："Skip all confirmation prompts and execute
/// commands without sandboxing. EXTREMELY DANGEROUS. Intended solely for
/// running in environments that are externally sandboxed." Lynceus 就是那个
/// 外部沙箱——授权与工具管控都在 MCP 服务侧完成，CLI 的交互式审批在
/// headless 下只会把所有调用判失败。
pub(crate) const FULL_ACCESS_FLAG: &str = "--dangerously-bypass-approvals-and-sandbox";

fn write_codex_home_config(
    request: &WorkerExecutionRequest,
) -> Result<(), WorkerRuntimeError> {
    if !cfg!(target_os = "windows") {
        return Ok(());
    }
    let home = worker_config_dir(request, WorkerRuntimeType::Codex);
    std::fs::create_dir_all(&home).map_err(|error| {
        WorkerRuntimeError::new(
            WorkerRuntimeErrorKind::Internal,
            format!("cannot create codex home {}: {error}", home.display()),
        )
    })?;
    let path = home.join("config.toml");
    // `elevated` 会让 Codex 走 ShellExecuteExW 拉起一个提权 helper，
    // headless 子进程里那个 UAC 提示无人应答，exec_command 直接以
    // Win32 1223（ERROR_CANCELLED）orchestrator_helper_launch_canceled
    // 失败——实测 pwsh 与 git bash 两条路都是这个错。全放行模式下沙箱本来
    // 就不启用，这里保持 unelevated，别再把提权 helper 请回来。
    // 合法值只有 `elevated` / `unelevated`（codex 配置校验会列出这两个）。
    std::fs::write(
        &path,
        "[windows]\nsandbox = \"unelevated\"\n",
    )
    .map_err(|error| {
        WorkerRuntimeError::new(
            WorkerRuntimeErrorKind::Internal,
            format!("cannot write {}: {error}", path.display()),
        )
    })
}

fn codex_args(
    model: &str,
    connection: &ResolvedWorkerConnection,
    request: &WorkerExecutionRequest,
    resume: bool,
) -> Result<Vec<String>, WorkerRuntimeError> {
    // 隔离 CODEX_HOME 仍要预写：Windows 上沙箱 helper 的提权档位只从这份
    // 配置读（全放行模式下沙箱不启用，但 helper 档位若为 elevated 仍会被
    // 请回来弹 UAC，见 write_codex_home_config 文档）。
    write_codex_home_config(request)?;
    // headless 全放行：`codex exec` 非交互，CLI 的审批提示永远等不到回答。
    // 实测不放开时每次 MCP 调用都以
    // "MCP tool call requires approval, but approval policy is never"
    // 失败（tool_list / blackboard_read 全军覆没），shell 也被沙箱挡掉，
    // 于是 worker 既没工具也没命令，只能输出"没有可执行面"。
    //
    // 授权已经发生在 Lynceus 侧，CLI 这一层再问一遍只是把 worker 变哑巴：
    // WorkerGrant scope（mission/run/task/worker）+ grant allowlist 与
    // preset.tools 取交集 + tool_execute 守卫链（allowlist / 保留字 /
    // schema / 预算 / 重复指纹）+ 无 shell 的 ToolGateway。
    //
    // 用户是在自己的机器上跑本地后端、用自己的 key，等同于直接在
    // PowerShell 里敲 codex；Claude 适配器同样显式放行
    // Bash/Read/Edit/Write/Grep/Glob 与 mcp__lynceus__*。
    let mut args = if resume {
        let Some(session_ref) = request.session_ref.as_ref() else {
            return Err(WorkerRuntimeError::new(
                WorkerRuntimeErrorKind::NotReady,
                "resume requires a session reference",
            ));
        };
        vec![
            "exec".to_string(),
            "resume".to_string(),
            session_ref.clone(),
            "--json".to_string(),
            "--skip-git-repo-check".to_string(),
            FULL_ACCESS_FLAG.to_string(),
        ]
    } else {
        vec![
            "exec".to_string(),
            "--json".to_string(),
            "--skip-git-repo-check".to_string(),
            FULL_ACCESS_FLAG.to_string(),
        ]
    };

    // 官方 `-c` TOML 覆盖：start/resume 共用 provider、base URL、model
    // 和 env_key；secret 只通过显式 OPENAI_API_KEY 环境注入。
    args.extend([
        "-c".to_string(),
        "model_provider=\"lynceus\"".to_string(),
        "-c".to_string(),
        "model_providers.lynceus.name=\"Lynceus Gateway\"".to_string(),
    ]);
    // 关掉 reasoning summary。codex 默认发 `reasoning.summary="auto"`，而
    // LiteLLM 把 `/v1/responses` 桥接成 chat/completions 时翻不动这个键：
    // 实测 summary 取 auto/none/concise/detailed **任一值都 400**
    // (`invalid request format`)，`effort` 单独在却没事，`{}` 也能过。
    // 这不是协议选错——`/v1/responses` + `gpt-main` 直连网关是 200——而是
    // 桥接层缺一个字段的翻译。设成 none 后 codex 改发 `reasoning: {}`，
    // 请求即可通过（2026-09-23 对 litellm + step-5-preview 实测）。
    args.extend([
        "-c".to_string(),
        "model_reasoning_summary=\"none\"".to_string(),
    ]);
    if let Some(base_url) = connection
        .base_url
        .as_deref()
        .filter(|url| !url.trim().is_empty())
    {
        args.push("-c".to_string());
        args.push(format!(
            "model_providers.lynceus.base_url={}",
            toml_string(base_url)
        ));
    }
    args.extend([
        "-c".to_string(),
        "model_providers.lynceus.env_key=\"OPENAI_API_KEY\"".to_string(),
    ]);

    // Codex 官方 streamable HTTP 配置支持 bearer_token_env_var：argv 只
    // 出现环境变量名，不出现 grant 原文。
    if let Some((url, _token)) = mcp_connection(request)? {
        args.extend([
            "-c".to_string(),
            format!("mcp_servers.lynceus.url={}", toml_string(&url)),
            "-c".to_string(),
            "mcp_servers.lynceus.bearer_token_env_var=\"LYNCEUS_MCP_TOKEN\"".to_string(),
            "-c".to_string(),
            "mcp_servers.lynceus.required=true".to_string(),
        ]);
    }
    args.extend(["--model".to_string(), model.to_string()]);
    Ok(args)
}

#[async_trait]
impl WorkerRuntime for CodexWorker {
    fn runtime_type(&self) -> WorkerRuntimeType {
        WorkerRuntimeType::Codex
    }

    async fn probe(&self) -> WorkerProbe {
        probe(
            &self.context,
            &["--version"],
            |output| parse_codex_version(output),
            vec![
                "exec".to_string(),
                "full-access".to_string(),
                "session-resume".to_string(),
            ],
        )
        .await
    }

    fn capabilities(&self) -> Vec<String> {
        vec![
            "exec".to_string(),
            "full-access".to_string(),
            "session-resume".to_string(),
        ]
    }

    async fn start(
        &self,
        request: WorkerExecutionRequest,
    ) -> Result<WorkerExecutionOutcome, WorkerRuntimeError> {
        execute(
            &self.context,
            request,
            WorkerInvocationPurpose::Start,
            |connection, profile, request| {
                if !codex_protocol_supported(connection.protocol) {
                    return Err(WorkerRuntimeError::new(
                        WorkerRuntimeErrorKind::Unsupported,
                        format!(
                            "codex cli requires an openai-compatible connection, got '{}'",
                            connection.protocol.as_str()
                        ),
                    ));
                }
                let Some(model) = profile.effective_model(connection.default_model.as_deref())
                else {
                    return Err(WorkerRuntimeError::new(
                        WorkerRuntimeErrorKind::NotReady,
                        "no model configured: set model_override on the profile or default_model \
                     on the connection",
                    ));
                };
                let mut args = codex_args(model, connection, request, false)?;
                // prompt 走 stdin，**不传任何 prompt 位置参数**：codex
                // `exec` 的约定是"未提供 PROMPT 参数（或传 `-`）时从
                // stdin 读取"（`codex exec --help` 原文）。2026-09-23 对
                // codex-cli 0.150.1 实测：挂在选项后面的 `-` 会被 clap 以
                // `unexpected argument '-' found` 拒绝（exit 2、零事件，
                // worker 根本没跑）；完全省略 prompt 参数则 stdin 自动
                // 读取、exit 0。多行指令绝不经 argv（cmd shim 会在换行处
                // 截断）。
                Ok(BuiltCommand {
                    binary_names: self.context.binary_names,
                    args,
                    env: openai_env_for_worker(connection, request),
                    stdin: Some(request.instruction.clone()),
                })
            },
            Some(StreamFlavor::Codex),
        )
        .await
    }

    async fn resume(
        &self,
        request: WorkerExecutionRequest,
    ) -> Result<WorkerExecutionOutcome, WorkerRuntimeError> {
        execute(
            &self.context,
            request,
            WorkerInvocationPurpose::Resume,
            |connection, profile, request| {
                if !codex_protocol_supported(connection.protocol) {
                    return Err(WorkerRuntimeError::new(
                        WorkerRuntimeErrorKind::Unsupported,
                        "codex cli requires an openai-compatible connection",
                    ));
                }
                let Some(model) = profile.effective_model(connection.default_model.as_deref())
                else {
                    return Err(WorkerRuntimeError::new(
                        WorkerRuntimeErrorKind::NotReady,
                        "no model configured",
                    ));
                };
                let Some(_session_ref) = request.session_ref.as_ref() else {
                    return Err(WorkerRuntimeError::new(
                        WorkerRuntimeErrorKind::NotReady,
                        "resume requires a session reference",
                    ));
                };
                let args = codex_args(model, connection, request, true)?;
                // prompt 走 stdin，不传 prompt 位置参数（原因同 start：
                // 0.150.1 拒绝挂在选项后的 `-`，省略则自动读 stdin）。
                Ok(BuiltCommand {
                    binary_names: self.context.binary_names,
                    args,
                    env: openai_env_for_worker(connection, request),
                    stdin: Some(request.instruction.clone()),
                })
            },
            Some(StreamFlavor::Codex),
        )
        .await
    }

    async fn events(
        &self,
        session_ref: &str,
    ) -> Result<Vec<models::worker::WorkerEvent>, WorkerRuntimeError> {
        let _ = session_ref;
        Ok(Vec::new())
    }

    async fn cancel(&self, session_ref: &str) -> Result<bool, WorkerRuntimeError> {
        Ok(self.context.inflight.cancel(session_ref))
    }
}

// ---------------------------------------------------------------------------

// DeepSeek Harness
// ---------------------------------------------------------------------------

/// DeepSeek Harness（`dsh --profile headless`；npm @deepseek-ai/dsh，
/// Developer Preview，配置面在 0.1.1-rc.2 实机验证）。
///
/// 版本感知：`dsh --version` 失败或输出无法解析 → Unsupported（拒绝猜
/// 协议）。模型与端点选择**不经 CLI flag**（headless app 无 `--model`，
/// 实测 `unknown option '--model'`），而是运行前预写隔离 `$DSH_HOME`
/// 下的 `settings.yaml`：`llm-pi-ai.providers` 声明网关路由（其 LLM 层
/// 即 pi-ai，openai-completions 在 baseURL 后拼 `/chat/completions`，
/// anthropic-messages 拼 `/v1/messages`），`agent-default-model` 钉定
/// 路由与模型。密钥经 `apiKeyEnv` 引用，真实值只进子进程环境。
///
/// 终局消息（2026-09-23 对 0.1.1-rc.2 + litellm 实跑验证）：headless
/// one-shot 的 **stdout 就是纯净答案**（多行 markdown 原文，无进度噪
/// 声、无 JSONL 协议帧），因此 execute 传 `None`（不挂 JSONL 流解析）
/// 是对的——终稿由 stdout 兜底落 `WorkerInvocation.summary`（成功与
/// 失败两条路径都兜）。
pub struct DeepSeekHarnessWorker {
    context: AdapterContext,
}

impl DeepSeekHarnessWorker {
    /// 构造。
    #[must_use]
    pub fn new(resolver: Arc<dyn agents::worker::WorkerConnectionResolver>) -> Self {
        Self {
            context: AdapterContext {
                runtime_type: WorkerRuntimeType::DeepSeekHarness,
                binary_names: &["dsh"],
                resolver,
                inflight: Arc::new(InflightRegistry::default()),
                session_sink: session_sink_arc(),
            },
        }
    }
}

#[async_trait]
impl WorkerRuntime for DeepSeekHarnessWorker {
    fn runtime_type(&self) -> WorkerRuntimeType {
        WorkerRuntimeType::DeepSeekHarness
    }

    async fn probe(&self) -> WorkerProbe {
        // 官方 probe 约定：`dsh --version` 优先，输出无法解析时回退
        // `dsh --help`（Developer Preview 早期版本可能只有 help 文本）。
        let primary = probe(
            &self.context,
            &["--version"],
            |output| parse_dsh_version(output),
            vec!["one-shot-run".to_string(), "headless-profile".to_string()],
        )
        .await;
        if primary.availability == WorkerAvailability::Unsupported {
            probe(
                &self.context,
                &["--help"],
                |output| parse_dsh_version(output),
                vec!["one-shot-run".to_string(), "headless-profile".to_string()],
            )
            .await
        } else {
            primary
        }
    }

    fn capabilities(&self) -> Vec<String> {
        vec!["one-shot-run".to_string(), "headless-profile".to_string()]
    }

    async fn start(
        &self,
        request: WorkerExecutionRequest,
    ) -> Result<WorkerExecutionOutcome, WorkerRuntimeError> {
        execute(
            &self.context,
            request,
            WorkerInvocationPurpose::Start,
            |connection, profile, request| {
                self.dsh_command(connection, profile, request)
            },
            None,
        )
        .await
    }

    async fn resume(
        &self,
        _request: WorkerExecutionRequest,
    ) -> Result<WorkerExecutionOutcome, WorkerRuntimeError> {
        // DSH preview 的 canonical 接口是 one-shot；continuation 待官方
        // 会话语义稳定后启用——显式 Unsupported，绝不猜协议。
        Err(WorkerRuntimeError::new(
            WorkerRuntimeErrorKind::Unsupported,
            "deepseek harness continuation is pending official session semantics \
             (developer preview); only one-shot runs are supported",
        ))
    }

    async fn events(
        &self,
        _session_ref: &str,
    ) -> Result<Vec<models::worker::WorkerEvent>, WorkerRuntimeError> {
        Ok(Vec::new())
    }

    async fn cancel(&self, session_ref: &str) -> Result<bool, WorkerRuntimeError> {
        Ok(self.context.inflight.cancel(session_ref))
    }
}

impl DeepSeekHarnessWorker {
    /// headless one-shot 命令构造：预写 `$DSH_HOME/settings.yaml`（网关
    /// 路由 + 默认模型），指令按官方接口走 argv（stdin 传任务未经官方
    /// 确认；多行指令在 Windows `.cmd` shim 下的截断风险记录在案）。
    fn dsh_command(
        &self,
        connection: &ResolvedWorkerConnection,
        profile: &WorkerRuntimeProfile,
        request: &WorkerExecutionRequest,
    ) -> Result<BuiltCommand, WorkerRuntimeError> {
        let Some(model) = profile.effective_model(connection.default_model.as_deref()) else {
            return Err(WorkerRuntimeError::new(
                WorkerRuntimeErrorKind::NotReady,
                "no model configured: set model_override on the profile or default_model \
                 on the connection",
            ));
        };
        let Some(base_url) =
            connection.base_url.as_deref().filter(|raw| !raw.trim().is_empty())
        else {
            return Err(WorkerRuntimeError::new(
                WorkerRuntimeErrorKind::NotReady,
                "deepseek harness requires a base_url connection (LiteLLM gateway or \
                 OpenAI/Anthropic-compatible endpoint)",
            ));
        };
        let config_dir = worker_config_dir(request, WorkerRuntimeType::DeepSeekHarness);
        let dsh_home = dsh_home_dir(&config_dir);
        write_dsh_settings(&dsh_home, connection, base_url, model, DSH_KEY_ENV)?;
        Ok(BuiltCommand {
            binary_names: self.context.binary_names,
            args: vec![
                // 0.1.1-rc.2 headless app 无 --model 等任何模型 flag；选择
                // 全在 settings.yaml 的 agent-default-model。
                "--profile".to_string(),
                "headless".to_string(),
                request.instruction.clone(),
            ],
            env: dsh_env(connection, request, &dsh_home),
            stdin: None,
        })
    }
}

/// DSH 隔离 home：profiles/settings/credentials/sessions 全部落在这里，
/// 与真实 `~/.dsh` 互不污染（登录态隔离）。
fn dsh_home_dir(config_dir: &std::path::Path) -> PathBuf {
    config_dir.join("dsh-home")
}

/// DSH 网关路由的密钥环境变量名（settings.yaml 只写引用，不写值）。
const DSH_KEY_ENV: &str = "LYNCEUS_GATEWAY_API_KEY";

/// 预写 `$DSH_HOME/settings.yaml`：`llm-pi-ai.providers` 把名为
/// `lynceus` 的路由指向 Lynceus Connection，`agent-default-model` 钉定
/// 该路由与模型。密钥只出现 `apiKeyEnv` 引用，真实值仅经进程环境注入。
///
/// baseURL 按协议适配 pi-ai 的路径拼接（实测）：openai-completions 在
/// baseURL 后拼 `/chat/completions`（要求 /v1 在 baseURL 内），
/// anthropic-messages 拼 `/v1/messages`（baseURL 不带 /v1）。
fn write_dsh_settings(
    dsh_home: &std::path::Path,
    connection: &ResolvedWorkerConnection,
    base_url: &str,
    model: &str,
    key_env: &str,
) -> Result<(), WorkerRuntimeError> {
    std::fs::create_dir_all(dsh_home).map_err(|error| {
        WorkerRuntimeError::new(
            WorkerRuntimeErrorKind::Internal,
            format!("cannot create dsh home {}: {error}", dsh_home.display()),
        )
    })?;
    let anthropic = connection.protocol == ProviderType::Anthropic;
    let api = if anthropic {
        "anthropic-messages"
    } else {
        "openai-completions"
    };
    let trimmed = base_url.trim_end_matches('/');
    // 无路径的裸主机（网关根）补 /v1；带自定义路径的端点按原样尊重。
    let openai_base = if trimmed.ends_with("/v1") {
        std::borrow::Cow::Borrowed(trimmed)
    } else {
        let after_scheme = trimmed
            .split_once("://")
            .map(|(_, rest)| rest)
            .unwrap_or(trimmed);
        if after_scheme.contains('/') {
            std::borrow::Cow::Borrowed(trimmed)
        } else {
            std::borrow::Cow::Owned(format!("{trimmed}/v1"))
        }
    };
    let base_url = if anthropic {
        std::borrow::Cow::Owned(
            openai_base
                .trim_end_matches('/')
                .strip_suffix("/v1")
                .unwrap_or(&openai_base)
                .to_owned(),
        )
    } else {
        openai_base
    };
    let settings = serde_json::json!({
        "llm-pi-ai": {
            "providers": {
                "lynceus": {
                    "displayName": "Lynceus Gateway",
                    "apiKeyEnv": key_env,
                    "api": api,
                    "baseURL": base_url,
                    "models": [
                        { "id": model, "contextWindow": 200_000, "maxTokens": 16_384 }
                    ]
                }
            }
        },
        "agent-default-model": { "provider": "lynceus", "model": model }
    });
    let body = serde_yaml::to_string(&settings).map_err(|error| {
        WorkerRuntimeError::new(
            WorkerRuntimeErrorKind::Internal,
            format!("cannot render dsh settings: {error}"),
        )
    })?;
    let path = dsh_home.join("settings.yaml");
    std::fs::write(&path, body).map_err(|error| {
        WorkerRuntimeError::new(
            WorkerRuntimeErrorKind::Internal,
            format!("cannot write {}: {error}", path.display()),
        )
    })
}

fn dsh_env(
    connection: &ResolvedWorkerConnection,
    request: &WorkerExecutionRequest,
    dsh_home: &std::path::Path,
) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    env.insert(
        "DSH_HOME".to_string(),
        dsh_home.to_string_lossy().into_owned(),
    );
    if let Some(api_key) =
        connection.api_key.as_deref().filter(|raw| !raw.trim().is_empty())
    {
        env.insert(DSH_KEY_ENV.to_string(), api_key.to_string());
    }
    // 驱动层白名单不继承 HOME（认证红线）——DSH_HOME 优先级更高不受影响，
    // 但补齐隔离 HOME 让 CLI 内部的 git 等子工具在 Unix 上正常工作，并与
    // claude/codex 的隔离语义一致。
    insert_isolated_home(&mut env, request, WorkerRuntimeType::DeepSeekHarness);
    env
}

// ---------------------------------------------------------------------------
// 版本解析
// ---------------------------------------------------------------------------

/// 解析 `name X.Y.Z …` / `X.Y.Z …` 形式的版本输出。
fn parse_semver_prefix(output: &str, binary_hint: &str) -> Option<String> {
    let pattern = regex::Regex::new(r"(\d+\.\d+\.\d+)").ok()?;
    let first_line = output.lines().next().unwrap_or_default();
    if first_line.to_ascii_lowercase().contains(binary_hint) || binary_hint.is_empty() {
        pattern
            .captures(first_line)
            .and_then(|captures| captures.get(1))
            .map(|match_| match_.as_str().to_string())
            .or_else(|| {
                // 某些 CLI 首行只有版本号本身。
                pattern
                    .captures(output)
                    .and_then(|captures| captures.get(1).map(|match_| match_.as_str().to_string()))
            })
    } else {
        None
    }
}

/// `codex-cli X.Y.Z` 的版本解析。
fn parse_codex_version(output: &str) -> Option<String> {
    let first_line = output
        .lines()
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !first_line.contains("codex") {
        return None;
    }
    let pattern = regex::Regex::new(r"(\d+\.\d+\.\d+)").ok()?;
    pattern
        .captures(&first_line)
        .and_then(|captures| captures.get(1))
        .map(|match_| match_.as_str().to_string())
}

/// DSH 版本解析（Developer Preview 容错）：`dsh` / `deepseek` 前缀提示之外，
/// 接受首行为裸 semver（含预发布号，如实装版 `0.1.1-rc.2`）的形态——仍锚定
/// 于真实探测输出，不猜测协议。
fn parse_dsh_version(output: &str) -> Option<String> {
    parse_semver_prefix(output, "dsh")
        .or_else(|| parse_semver_prefix(output, "deepseek"))
        .or_else(|| {
            let first_line = output.lines().next().unwrap_or_default().trim();
            let pattern = regex::Regex::new(r"^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$").ok()?;
            if pattern.is_match(first_line) {
                Some(first_line.to_string())
            } else {
                None
            }
        })
}

// ── Pi Coding Agent（earendil-works/pi；0.84.4 实机验证）────────────────

/// Pi 隔离 agent 目录（`PI_CODING_AGENT_DIR` 指向这里；models.json、
/// auth、sessions 全在该目录下，与真实 `~/.pi/agent` 互不污染）。
fn pi_agent_dir(config_dir: &std::path::Path) -> PathBuf {
    config_dir.join("pi-agent")
}

/// Pi 网关路由的密钥环境变量名（models.json 只写 `$ENV` 引用）。
const PI_KEY_ENV: &str = "LYNCEUS_GATEWAY_API_KEY";

/// 为 Pi 预写 `<agentDir>/models.json`（官方配置面，docs/models.md）。
///
/// 实测（0.84.4）：`--provider` 接受 models.json 里的自定义 provider id；
/// 模型条目只需 `id`（`contextWindow` 可选）；`apiKey` 支持 `$ENV` 引用；
/// `anthropic-messages` 在 **baseURL 后自拼 `/v1/messages`**——网关根地址
/// 直接可用，上游 /v1 后缀必须剥掉。
fn write_pi_models_json(
    agent_dir: &std::path::Path,
    base_url: &str,
    model: &str,
    key_env: &str,
    api: &str,
) -> Result<PathBuf, WorkerRuntimeError> {
    std::fs::create_dir_all(agent_dir).map_err(|error| {
        WorkerRuntimeError::new(
            WorkerRuntimeErrorKind::Internal,
            format!(
                "cannot create pi config dir {}: {error}",
                agent_dir.display()
            ),
        )
    })?;
    let path = agent_dir.join("models.json");
    let trimmed = base_url.trim_end_matches('/');
    let api_base = trimmed.strip_suffix("/v1").unwrap_or(trimmed);
    let models_json = serde_json::json!({
        "providers": {
            "gateway": {
                "baseUrl": api_base,
                "apiKey": format!("${key_env}"),
                "api": api,
                "models": [{ "id": model, "contextWindow": 200_000 }]
            }
        }
    });
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&models_json).unwrap_or_else(|_| "{}".to_string()),
    )
    .map_err(|error| {
        WorkerRuntimeError::new(
            WorkerRuntimeErrorKind::Internal,
            format!("cannot write {}: {error}", path.display()),
        )
    })?;
    Ok(path)
}

/// Pi 编排器 worker：one-shot `--mode json`（Developer Preview）。
pub struct PiWorker {
    context: AdapterContext,
}

impl PiWorker {
    /// 构造。
    #[must_use]
    pub fn new(resolver: Arc<dyn agents::worker::WorkerConnectionResolver>) -> Self {
        Self {
            context: AdapterContext {
                runtime_type: WorkerRuntimeType::Pi,
                binary_names: &["pi"],
                resolver,
                inflight: Arc::new(InflightRegistry::default()),
                session_sink: session_sink_arc(),
            },
        }
    }

    /// start/resume 共用命令构造：预写隔离 models.json + 环境注入，
    /// 多行指令走 stdin（Windows cmd shim 的 argv 截断红线）。
    fn pi_command(
        &self,
        connection: &ResolvedWorkerConnection,
        profile: &WorkerRuntimeProfile,
        request: &WorkerExecutionRequest,
        session: Option<&str>,
    ) -> Result<BuiltCommand, WorkerRuntimeError> {
        let Some(model) = profile.effective_model(connection.default_model.as_deref()) else {
            return Err(WorkerRuntimeError::new(
                WorkerRuntimeErrorKind::NotReady,
                "no model configured: set model_override on the profile or default_model \
                 on the connection",
            ));
        };
        let Some(base_url) =
            connection.base_url.as_deref().filter(|raw| !raw.trim().is_empty())
        else {
            return Err(WorkerRuntimeError::new(
                WorkerRuntimeErrorKind::NotReady,
                "pi requires a base_url connection (LiteLLM gateway or Anthropic-compatible endpoint)",
            ));
        };
        let config_dir = worker_config_dir(request, WorkerRuntimeType::Pi);
        let agent_dir = pi_agent_dir(&config_dir);
        write_pi_models_json(&agent_dir, base_url, model, PI_KEY_ENV, "anthropic-messages")?;
        let mut env = BTreeMap::new();
        env.insert(
            "PI_CODING_AGENT_DIR".to_string(),
            agent_dir.to_string_lossy().into_owned(),
        );
        if let Some(api_key) =
            connection.api_key.as_deref().filter(|raw| !raw.trim().is_empty())
        {
            env.insert(PI_KEY_ENV.to_string(), api_key.to_string());
        }
        // 驱动层白名单不继承 HOME（认证红线）——PI_CODING_AGENT_DIR 优先级
        // 更高不受影响，但补齐隔离 HOME 让 CLI 内部的 git 等子工具在 Unix
        // 上正常工作，并与 claude/codex 的隔离语义一致。
        insert_isolated_home(&mut env, request, WorkerRuntimeType::Pi);
        let mut args = vec![
            "--mode".to_string(),
            "json".to_string(),
            "-p".to_string(),
        ];
        if let Some(session_ref) = session {
            args.push("--session".to_string());
            args.push(session_ref.to_string());
        }
        args.push("--provider".to_string());
        args.push("gateway".to_string());
        args.push("--model".to_string());
        args.push(model.to_string());
        Ok(BuiltCommand {
            binary_names: self.context.binary_names,
            args,
            env,
            stdin: Some(request.instruction.clone()),
        })
    }
}

#[async_trait]
impl WorkerRuntime for PiWorker {
    fn runtime_type(&self) -> WorkerRuntimeType {
        WorkerRuntimeType::Pi
    }

    async fn probe(&self) -> WorkerProbe {
        probe(
            &self.context,
            &["--version"],
            |output| parse_semver_prefix(output, ""),
            vec!["json-mode".to_string(), "session-resume".to_string()],
        )
        .await
    }

    fn capabilities(&self) -> Vec<String> {
        vec!["json-mode".to_string(), "session-resume".to_string()]
    }

    async fn start(
        &self,
        request: WorkerExecutionRequest,
    ) -> Result<WorkerExecutionOutcome, WorkerRuntimeError> {
        execute(
            &self.context,
            request,
            WorkerInvocationPurpose::Start,
            |connection, profile, request| self.pi_command(connection, profile, request, None),
            Some(StreamFlavor::Pi),
        )
        .await
    }

    async fn resume(
        &self,
        request: WorkerExecutionRequest,
    ) -> Result<WorkerExecutionOutcome, WorkerRuntimeError> {
        let session_ref = request.session_ref.clone().ok_or_else(|| {
            WorkerRuntimeError::new(
                WorkerRuntimeErrorKind::NotReady,
                "resume requires a session reference",
            )
        })?;
        execute(
            &self.context,
            request,
            WorkerInvocationPurpose::Resume,
            |connection, profile, request| {
                self.pi_command(connection, profile, request, Some(&session_ref))
            },
            Some(StreamFlavor::Pi),
        )
        .await
    }

    async fn events(
        &self,
        _session_ref: &str,
    ) -> Result<Vec<models::worker::WorkerEvent>, WorkerRuntimeError> {
        Ok(Vec::new())
    }

    async fn cancel(&self, session_ref: &str) -> Result<bool, WorkerRuntimeError> {
        Ok(self.context.inflight.cancel(session_ref))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semver_parse_accepts_known_formats() {
        assert_eq!(
            parse_semver_prefix("2.1.235 (Claude Code)", "claude"),
            Some("2.1.235".to_string())
        );
        assert_eq!(
            parse_codex_version("codex-cli 0.150.1"),
            Some("0.150.1".to_string())
        );
        assert_eq!(parse_codex_version("npm 10.0.0"), None);
        assert_eq!(parse_semver_prefix("garbage output", "dsh"), None);
        // DSH 容错形态：带 deepseek 提示 / 裸版本号（含预发布号）。
        assert_eq!(
            parse_dsh_version("DeepSeek Harness 0.1.2"),
            Some("0.1.2".to_string())
        );
        assert_eq!(parse_dsh_version("0.1.2"), Some("0.1.2".to_string()));
        assert_eq!(
            parse_dsh_version("0.1.1-rc.2"),
            Some("0.1.1-rc.2".to_string())
        );
        assert_eq!(parse_dsh_version("garbage output"), None);
    }

    #[test]
    fn anthropic_env_mapping_injects_only_connection_values() {
        let connection = ResolvedWorkerConnection {
            connection_id: "prov".to_string(),
            protocol: ProviderType::Anthropic,
            base_url: Some("https://gateway.test".to_string()),
            api_key: Some("sk-test".to_string()),
            default_model: Some("m".to_string()),
        };
        let env = anthropic_env(&connection);
        assert_eq!(
            env.get("ANTHROPIC_BASE_URL").map(String::as_str),
            Some("https://gateway.test")
        );
        assert_eq!(
            env.get("ANTHROPIC_AUTH_TOKEN").map(String::as_str),
            Some("sk-test")
        );
        assert_eq!(
            env.get("ANTHROPIC_API_KEY").map(String::as_str),
            Some("sk-test")
        );
        // 登录态隔离：config dir 被强制重定向到干净目录。
        assert!(
            env.get("CLAUDE_CONFIG_DIR")
                .is_some_and(|value| value.contains(".lynceus-worker"))
        );
        assert_eq!(env.get("IS_SANDBOX").map(String::as_str), Some("1"));
        // 网关模型发现（未知 --model 名必须被 CLI 接受而非回退默认）。
        assert_eq!(
            env.get("CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY")
                .map(String::as_str),
            Some("1")
        );
        assert_eq!(
            env.get("CLAUDE_CODE_PROVIDER_MANAGED_BY_HOST")
                .map(String::as_str),
            Some("1")
        );
        // 非必要遥测关闭（打网关/中转站会被 403 污染事件流）。
        assert_eq!(
            env.get("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC")
                .map(String::as_str),
            Some("1")
        );
        assert_eq!(env.len(), 19);
        let missing = anthropic_env(&ResolvedWorkerConnection {
            api_key: None,
            base_url: None,
            ..connection
        });
        assert!(missing.get("ANTHROPIC_API_KEY").is_none());
    }

    #[test]
    fn claude_mcp_config_is_secret_free_and_reused_by_resume() {
        let directory = tempfile::tempdir().expect("temp dir");
        let request = WorkerExecutionRequest::start("prompt", 30)
            .with_config_dir(directory.path().join("worker-config"))
            .with_mcp("http://127.0.0.1:3000/mcp", "lyn_secret_token");
        let start = claude_args("claude-test", &request).expect("start args");
        let path = request
            .config_dir
            .as_ref()
            .expect("config dir")
            .join("lynceus-mcp.json");
        let contents = std::fs::read_to_string(path).expect("mcp config");
        assert!(!contents.contains("lyn_secret_token"));
        assert!(contents.contains("${LYNCEUS_MCP_TOKEN}"));

        let mut resume = request.clone();
        resume.session_ref = Some("session-1".to_string());
        let resumed = claude_args("claude-test", &resume).expect("resume args");
        assert!(start.contains(&"--mcp-config".to_string()));
        assert!(resumed.contains(&"--mcp-config".to_string()));
        assert!(resumed.contains(&"session-1".to_string()));
        // 执行能力：MCP + 原生工具统一显式放行（headless 默认静默拒绝）。
        let allowed_idx = start
            .iter()
            .position(|arg| arg == "--allowedTools")
            .expect("allowedTools flag");
        let allowed = start
            .get(allowed_idx + 1)
            .expect("allowedTools value");
        for tool in ["Bash", "Read", "Edit", "Write", "Grep", "Glob", "mcp__lynceus__*"] {
            assert!(allowed.contains(tool), "allowedTools missing {tool}");
        }
        assert_eq!(
            anthropic_env_for_request(
                &ResolvedWorkerConnection {
                    connection_id: "p".to_string(),
                    protocol: ProviderType::Anthropic,
                    base_url: None,
                    api_key: None,
                    default_model: None,
                },
                &request,
                None,
            )
            .get("LYNCEUS_MCP_TOKEN")
            .map(String::as_str),
            Some("lyn_secret_token")
        );
    }

    #[test]
    fn codex_start_and_resume_share_provider_and_mcp_overrides() {
        let request = WorkerExecutionRequest::resume("session-1", "prompt", 30)
            .with_config_dir(std::path::PathBuf::from("data/test-worker"))
            .with_mcp("http://127.0.0.1:3000/mcp", "lyn_secret_token");
        let connection = ResolvedWorkerConnection {
            connection_id: "p".to_string(),
            protocol: ProviderType::OpenaiCompatible,
            base_url: Some("http://127.0.0.1:4000/v1".to_string()),
            api_key: Some("sk-test".to_string()),
            default_model: Some("model".to_string()),
        };
        let resume = codex_args("model", &connection, &request, true).expect("resume args");
        let start_request = WorkerExecutionRequest::start("prompt", 30)
            .with_config_dir(std::path::PathBuf::from("data/test-worker"))
            .with_mcp("http://127.0.0.1:3000/mcp", "lyn_secret_token");
        let start = codex_args("model", &connection, &start_request, false).expect("start args");
        for common in [
            "model_provider=\"lynceus\"",
            "model_providers.lynceus.env_key=\"OPENAI_API_KEY\"",
            "mcp_servers.lynceus.bearer_token_env_var=\"LYNCEUS_MCP_TOKEN\"",
        ] {
            assert!(
                start.contains(&common.to_string()),
                "start missing {common}"
            );
            assert!(
                resume.contains(&common.to_string()),
                "resume missing {common}"
            );
        }
        // 执行能力：全放行（headless 下审批提示等不到回答，见 FULL_ACCESS_FLAG）。
        assert!(start.contains(&FULL_ACCESS_FLAG.to_string()));
        assert!(resume.contains(&FULL_ACCESS_FLAG.to_string()));
        // 不再请求 CLI 沙箱：写面/网络管控在 MCP 服务侧，不在 CLI 侧。
        assert!(!start.contains(&"--sandbox".to_string()));
        assert!(!start.contains(&"workspace-write".to_string()));
        assert!(!start.contains(&"read-only".to_string()));
        assert!(!start.contains(&"sandbox_workspace_write".to_string()));
        // Windows 沙箱 helper 不能提权：unelevated，否则 exec_command 以
        // Win32 1223 失败。
        let codex_home = std::path::PathBuf::from("data/test-worker");
        let rendered =
            std::fs::read_to_string(codex_home.join("config.toml")).expect("codex config.toml");
        assert!(rendered.contains("[windows]"));
        assert!(rendered.contains("sandbox = \"unelevated\""));
        assert!(
            !rendered.contains("sandbox = \"elevated\""),
            "不能把提权 helper 请回来：{rendered}"
        );
        assert!(!start.contains(&"lyn_secret_token".to_string()));
        assert!(!resume.contains(&"lyn_secret_token".to_string()));
    }


    /// 命令禁则译成 Claude Code 原生 deny 面（四个黑盒 CLI 里唯一能
    /// per-command 真拦的）；deny 胜过 allow——上面放行的 Bash 仍受
    /// 前缀约束。
    #[test]
    fn claude_args_render_disallowed_tools_from_command_policy() {
        let mut request = WorkerExecutionRequest::start("prompt", 30);
        request.denied_command_prefixes = vec!["rm -rf /".to_string(), "shutdown".to_string()];
        let connection = ResolvedWorkerConnection {
            connection_id: "p".to_string(),
            protocol: ProviderType::Anthropic,
            base_url: Some("http://127.0.0.1:4000".to_string()),
            api_key: Some("sk-test".to_string()),
            default_model: Some("model".to_string()),
        };
        let args = claude_args("model", &request).expect("claude args");
        let denied = args
            .iter()
            .skip_while(|arg| *arg != "--disallowedTools")
            .nth(1)
            .expect("disallowedTools value");
        assert_eq!(denied, "Bash(rm -rf / *),Bash(shutdown *)");
        // allowlist 仍在（deny 是叠加不是替换）。
        assert!(args.contains(&"--allowedTools".to_string()));

        // 无禁则 → 不出现该 flag（字节面不变）。
        let plain = WorkerExecutionRequest::start("prompt", 30);
        let args = claude_args("model", &plain).expect("claude args");
        assert!(!args.contains(&"--disallowedTools".to_string()));
    }    fn pi_test_connection() -> ResolvedWorkerConnection {
        ResolvedWorkerConnection {
            connection_id: "prov".to_string(),
            protocol: ProviderType::Anthropic,
            base_url: Some("http://127.0.0.1:4141".to_string()),
            api_key: Some("sk-gw-secret".to_string()),
            default_model: Some("pi-main".to_string()),
        }
    }

    fn pi_test_profile() -> WorkerRuntimeProfile {
        let mut profile = WorkerRuntimeProfile::new(
            WorkerRuntimeType::Pi,
            "prov",
            models::WorkerExecutionEnvironment::Local,
            1,
            900,
        )
        .expect("profile");
        profile.model_override = Some("pi-main".to_string());
        profile
    }

    /// 命令构造不触 resolver；测试注入永不解析的占位实现。
    struct NeverResolver;

    #[async_trait::async_trait]
    impl agents::worker::WorkerConnectionResolver for NeverResolver {
        async fn resolve(
            &self,
            _connection_id: &str,
        ) -> Result<ResolvedWorkerConnection, WorkerRuntimeError> {
            Err(WorkerRuntimeError::new(
                WorkerRuntimeErrorKind::NotInstalled,
                "test resolver never resolves",
            ))
        }

        async fn profile(
            &self,
            _runtime_type: WorkerRuntimeType,
        ) -> Result<Option<WorkerRuntimeProfile>, WorkerRuntimeError> {
            Ok(None)
        }

        async fn validate_connection(
            &self,
            _connection_id: &str,
        ) -> Result<ResolvedWorkerConnection, WorkerRuntimeError> {
            Err(WorkerRuntimeError::new(
                WorkerRuntimeErrorKind::NotInstalled,
                "test resolver never validates",
            ))
        }
    }

    #[test]
    fn pi_start_writes_isolated_models_json_and_agent_dir_env() {
        let directory = tempfile::tempdir().expect("temp dir");
        let request = WorkerExecutionRequest::start("prompt", 30)
            .with_config_dir(directory.path().to_path_buf());
        let worker = PiWorker::new(std::sync::Arc::new(NeverResolver));
        let built = worker
            .pi_command(&pi_test_connection(), &pi_test_profile(), &request, None)
            .expect("pi start command");
        // models.json 必须落在 PI_CODING_AGENT_DIR 指向的隔离 agent 目录。
        let agent_dir = directory.path().join("pi-agent");
        assert_eq!(
            built.env.get("PI_CODING_AGENT_DIR").map(String::as_str),
            Some(agent_dir.to_string_lossy().as_ref())
        );
        // HOME 隔离（Unix 上驱动层白名单不继承 HOME，必须由 adapter 指向
        // 隔离目录；agentDir 覆盖仍优先）。
        assert_eq!(
            built.env.get("HOME").map(String::as_str),
            Some(directory.path().to_string_lossy().as_ref())
        );
        let models = std::fs::read_to_string(agent_dir.join("models.json")).expect("models.json");
        assert!(models.contains("http://127.0.0.1:4141"));
        assert!(models.contains("\"anthropic-messages\""));
        // 密钥只经 env 注入，文件里只有 $ENV 引用。
        assert!(models.contains("$LYNCEUS_GATEWAY_API_KEY"));
        assert!(!models.contains("sk-gw-secret"));
        assert_eq!(
            built.env.get("LYNCEUS_GATEWAY_API_KEY").map(String::as_str),
            Some("sk-gw-secret")
        );
        assert_eq!(
            built.args,
            vec![
                "--mode".to_string(),
                "json".to_string(),
                "-p".to_string(),
                "--provider".to_string(),
                "gateway".to_string(),
                "--model".to_string(),
                "pi-main".to_string(),
            ]
        );
        assert_eq!(built.stdin.as_deref(), Some("prompt"));
    }

    #[test]
    fn pi_resume_carries_session_reference() {
        let directory = tempfile::tempdir().expect("temp dir");
        let request = WorkerExecutionRequest::resume("session-9", "prompt", 30)
            .with_config_dir(directory.path().to_path_buf());
        let worker = PiWorker::new(std::sync::Arc::new(NeverResolver));
        let built = worker
            .pi_command(&pi_test_connection(), &pi_test_profile(), &request, Some("session-9"))
            .expect("pi resume command");
        assert!(built.args.contains(&"--session".to_string()));
        assert!(built.args.contains(&"session-9".to_string()));
    }

    fn dsh_test_connection(protocol: ProviderType, base_url: &str) -> ResolvedWorkerConnection {
        ResolvedWorkerConnection {
            connection_id: "prov".to_string(),
            protocol,
            base_url: Some(base_url.to_string()),
            api_key: Some("sk-gw-secret".to_string()),
            default_model: Some("dsh-main".to_string()),
        }
    }

    fn dsh_test_profile() -> WorkerRuntimeProfile {
        let mut profile = WorkerRuntimeProfile::new(
            WorkerRuntimeType::DeepSeekHarness,
            "prov",
            models::WorkerExecutionEnvironment::Local,
            1,
            900,
        )
        .expect("profile");
        profile.model_override = Some("dsh-main".to_string());
        profile
    }

    #[test]
    fn dsh_start_writes_gateway_settings_without_model_flag() {
        let directory = tempfile::tempdir().expect("temp dir");
        let request = WorkerExecutionRequest::start("say hi", 30)
            .with_config_dir(directory.path().to_path_buf());
        let worker = DeepSeekHarnessWorker::new(std::sync::Arc::new(NeverResolver));
        // 网关根（无 /v1）：openai-completions 需补 /v1（pi-ai 拼
        // /chat/completions）。
        let built = worker
            .dsh_command(
                &dsh_test_connection(ProviderType::OpenaiCompatible, "http://127.0.0.1:4141"),
                &dsh_test_profile(),
                &request,
            )
            .expect("dsh start command");
        let dsh_home = directory.path().join("dsh-home");
        assert_eq!(
            built.env.get("DSH_HOME").map(String::as_str),
            Some(dsh_home.to_string_lossy().as_ref())
        );
        assert_eq!(
            built.env.get("HOME").map(String::as_str),
            Some(directory.path().to_string_lossy().as_ref())
        );
        assert_eq!(
            built.env.get("LYNCEUS_GATEWAY_API_KEY").map(String::as_str),
            Some("sk-gw-secret")
        );
        let settings = std::fs::read_to_string(dsh_home.join("settings.yaml")).expect("settings");
        assert!(settings.contains("api: openai-completions"));
        assert!(settings.contains("baseURL: http://127.0.0.1:4141/v1"));
        assert!(settings.contains("id: dsh-main"));
        assert!(settings.contains("provider: lynceus"));
        assert!(settings.contains("model: dsh-main"));
        assert!(settings.contains("apiKeyEnv: LYNCEUS_GATEWAY_API_KEY"));
        assert!(!settings.contains("sk-gw-secret"));
        // headless app 无 --model（0.1.1-rc.2 实测 unknown option）。
        assert_eq!(
            built.args,
            vec![
                "--profile".to_string(),
                "headless".to_string(),
                "say hi".to_string(),
            ]
        );
    }

    #[test]
    fn dsh_anthropic_protocol_strips_v1_and_switches_api() {
        let directory = tempfile::tempdir().expect("temp dir");
        let request = WorkerExecutionRequest::start("say hi", 30)
            .with_config_dir(directory.path().to_path_buf());
        let worker = DeepSeekHarnessWorker::new(std::sync::Arc::new(NeverResolver));
        worker
            .dsh_command(
                &dsh_test_connection(ProviderType::Anthropic, "https://upstream.test/v1"),
                &dsh_test_profile(),
                &request,
            )
            .expect("dsh anthropic command");
        let settings =
            std::fs::read_to_string(directory.path().join("dsh-home/settings.yaml"))
                .expect("settings");
        assert!(settings.contains("api: anthropic-messages"));
        assert!(settings.contains("baseURL: https://upstream.test"));
    }

    // ---------- 终稿捕获（settlement 契约） ----------
    //
    // 回归防线：外部 worker 跑完后，用户眼前必须有一句结论。这三条锁住
    // "终稿被采集 → 完整保留 → 不被空值/摘录覆盖"。

    #[test]
    fn codex_agent_message_becomes_final_message() {
        let mut collector = StreamCollector::default();
        absorb_codex_line(
            &mut collector,
            r#"{"type":"item.completed","item":{"type":"agent_message","text":"你好，这是结论"}}"#,
        );
        assert_eq!(collector.final_message.as_deref(), Some("你好，这是结论"));
        // 事件流仍留有界摘录（执行痕迹与终稿是两条道）。
        assert!(collector.events.iter().any(|(kind, _)| *kind == WorkerEventKind::Output));
    }

    /// 2026-09-23 codex-cli 0.150.1 实跑样本（litellm + step-5-preview）：
    /// 多步任务先发一条**空 text** 的 agent_message（narration 占位），
    /// 工具调用之后才是真正的终局 agent_message。空文本不得覆盖已有
    /// 终稿，后到的非空终稿覆盖先到的。
    #[test]
    fn codex_real_multistep_sequence_keeps_last_nonempty_message() {
        let mut collector = StreamCollector::default();
        // 1. 空叙述占位（先到）。
        absorb_codex_line(
            &mut collector,
            r#"{"type":"item.completed","item":{"id":"item_2","type":"agent_message","text":""}}"#,
        );
        assert!(collector.final_message.is_none(), "空叙述不得成为终稿");
        // 2. 工具调用痕迹不影响终稿。
        absorb_codex_line(
            &mut collector,
            r#"{"type":"item.completed","item":{"id":"item_3","type":"command_execution","command":"pwsh -Command 'dir'","exit_code":0}}"#,
        );
        assert!(collector.final_message.is_none());
        // 3. 真正的终局消息。
        absorb_codex_line(
            &mut collector,
            r#"{"type":"item.completed","item":{"id":"item_4","type":"agent_message","text":"当前目录共 552 个文件"}}"#,
        );
        assert_eq!(
            collector.final_message.as_deref(),
            Some("当前目录共 552 个文件")
        );
        // 4. 终局之后再来的空文本不得把终稿清掉。
        absorb_codex_line(
            &mut collector,
            r#"{"type":"item.completed","item":{"id":"item_5","type":"agent_message","text":"  "}}"#,
        );
        assert_eq!(
            collector.final_message.as_deref(),
            Some("当前目录共 552 个文件")
        );
    }

    /// `turn.failed`（实跑样本：缺环境变量时 codex 发
    /// `{"type":"error",...}` + `{"type":"turn.failed",...}`）必须置
    /// failed 并留下原因——否则退出码之外的失败只剩一段不可读的
    /// stderr。
    #[test]
    fn codex_turn_failed_marks_collector_failed_with_reason() {
        let mut collector = StreamCollector::default();
        absorb_codex_line(
            &mut collector,
            r#"{"type":"error","message":"Missing environment variable: `OPENAI_API_KEY`."}"#,
        );
        absorb_codex_line(
            &mut collector,
            r#"{"type":"turn.failed","error":{"message":"Missing environment variable: `OPENAI_API_KEY`."}}"#,
        );
        assert!(collector.failed, "turn.failed 必须置 failed");
        assert!(
            collector
                .events
                .iter()
                .any(|(kind, message)| *kind == WorkerEventKind::Error
                    && message.contains("OPENAI_API_KEY")),
            "失败原因必须进事件流: {:?}",
            collector.events
        );
    }

    #[test]
    fn claude_result_event_becomes_final_message() {
        let mut collector = StreamCollector::default();
        absorb_claude_line(
            &mut collector,
            r#"{"type":"result","subtype":"success","result":"扫描完成，未发现高危漏洞"}"#,
        );
        assert_eq!(
            collector.final_message.as_deref(),
            Some("扫描完成，未发现高危漏洞")
        );
    }

    /// pi 的 `message_end` 必须留住**全文**终稿。
    ///
    /// 回归防线：此前只 `record(Output, excerpt(text, 300))`——300 字符
    /// 截录进事件流、全文丢弃，于是 pi run 的 summary 只能退化成 stdout
    /// 粗提取（JSONL CLI 的 stdout 是一串协议噪声，不可读）。
    #[test]
    fn pi_message_end_becomes_final_message() {
        let mut collector = StreamCollector::default();
        absorb_pi_line(
            &mut collector,
            r#"{"type":"message_end","message":{"stopReason":"end_turn","content":[
                {"type":"text","text":"第一段结论。"},
                {"type":"text","text":"第二段结论。"}
            ]}}"#,
        );
        // 多 text block 必须全文拼接，不是摘录。
        assert_eq!(
            collector.final_message.as_deref(),
            Some("第一段结论。第二段结论。")
        );
        // 事件流仍留有界摘录（执行痕迹与终稿是两条道）。
        assert!(collector.events.iter().any(|(kind, _)| *kind == WorkerEventKind::Output));
    }

    /// pi 的 non-message_end 事件不得污染终稿（工具调用、session 头等）。
    #[test]
    fn pi_non_message_events_leave_final_message_empty() {
        let mut collector = StreamCollector::default();
        absorb_pi_line(&mut collector, r#"{"type":"session","id":"pi-session-1"}"#);
        absorb_pi_line(
            &mut collector,
            r#"{"type":"tool_execution_end","toolName":"bash","isError":false}"#,
        );
        absorb_pi_line(&mut collector, r#"{"type":"agent_end"}"#);
        assert!(collector.final_message.is_none());
        // 但 session 引用照旧解析（resume 靠它）。
        assert_eq!(collector.session_ref.as_deref(), Some("pi-session-1"));
    }

    /// pi 多轮发言：最后一个 `message_end` 赢（worker 可能边说边改）。
    #[test]
    fn pi_later_message_end_wins() {
        let mut collector = StreamCollector::default();
        absorb_pi_line(
            &mut collector,
            r#"{"type":"message_end","message":{"content":[{"type":"text","text":"先探一下"}]}}"#,
        );
        absorb_pi_line(
            &mut collector,
            r#"{"type":"message_end","message":{"content":[{"type":"text","text":"最终结论"}]}}"#,
        );
        assert_eq!(collector.final_message.as_deref(), Some("最终结论"));
    }

    #[test]
    fn later_final_message_wins_and_blanks_do_not_erase() {
        let mut collector = StreamCollector::default();
        // worker 分多轮发言：最后一句才是本轮结论。
        absorb_codex_line(
            &mut collector,
            r#"{"type":"item.completed","item":{"type":"agent_message","text":"第一轮"}}"#,
        );
        absorb_codex_line(
            &mut collector,
            r#"{"type":"item.completed","item":{"type":"agent_message","text":"第二轮，最终结论"}}"#,
        );
        assert_eq!(
            collector.final_message.as_deref(),
            Some("第二轮，最终结论")
        );
        // 空文本不得把已有终稿擦掉。
        collector.record_final_message("   ");
        assert_eq!(
            collector.final_message.as_deref(),
            Some("第二轮，最终结论")
        );
    }

    #[test]
    fn non_agent_message_lines_leave_final_message_empty() {
        let mut collector = StreamCollector::default();
        absorb_codex_line(
            &mut collector,
            r#"{"type":"item.completed","item":{"type":"command_execution","command":"ls","exit_code":0}}"#,
        );
        assert!(collector.final_message.is_none());
    }
}
