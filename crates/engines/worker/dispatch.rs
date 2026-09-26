//! 编排 → 外部 Worker 的派发层：指令构造、执行、transcript 密封与
//! `SolverResult` 映射。
//!
//! 红线：
//! - 外部 worker 的 stdout/stderr 只是**受控 observation**（有界、脱敏、
//!   密封 transcript），绝不直接变成 Finding/Evidence；
//! - 指令构造是确定性的（目标 / intent / 预算 / 输出契约），不含密钥；
//! - 未绑定有效 Connection → NotReady 显式失败；无可用 runtime →
//!   unavailable 显式失败。**绝不回退内部执行**。

use std::sync::Arc;

use agents::solver::SolverContext;
use agents::solver::SolverError;
use agents::solver::SolverExecutionError;
use agents::solver::SolverResult;
use agents::worker::WorkerExecutionRequest;
use agents::worker::WorkerRuntimeErrorKind;
use agents::worker::WorkerRuntimeSelector;
use agents::worker::worker_run_failed;
use evidence::SealedArtifact;
use models::agent_preset::{
    AgentPreset, render_template, builtin_templates::WORKER_INSTRUCTION_V1,
};
use models::fact::Fact;
use models::worker::WorkerRunStatus;
use models::worker::WorkerRuntimeType;

use super::adapters::DEFAULT_TIMEOUT_SECONDS;
use super::command_policy::CommandPolicy;

/// run config 键：显式偏好某个 worker runtime（wire 值，如
/// `"claude_code"`）。缺省时按注册表优先级选第一个可用项。
pub const CONFIG_WORKER_RUNTIME: &str = "worker_runtime";

/// run config 键：单次外部执行的秒级超时覆盖。
pub const CONFIG_WORKER_TIMEOUT_SECONDS: &str = "worker_timeout_seconds";

/// run config 键：mission 级 Agent 预设指定（WP4 解析优先级第一层）。
pub const CONFIG_AGENT_PRESET: &str = "agent_preset";

/// 把运营者的中断注入消息接在原始指令后面。
///
/// 分隔线是给 worker 的明确信号：前面是原任务全文（fresh start 时它是
/// 唯一上下文来源），后面是**新到达的指令**，并要求不要重复已完成的工作。
fn enrich_with_interrupt(instruction: &str, message: &str) -> String {
    format!(
        "{instruction}\n\n=== operator interrupt (new instruction from the user) ===\n{message}\n=== continue the task with the new instruction; do not repeat work already done ==="
    )
}

/// 把一次分支任务派发给外部 Worker Runtime。
///
/// # Errors
/// - 无可用 runtime / 配置缺失：显式 `unavailable` / `configuration
///   required` 消息（不伪造产出）；
/// - 外部执行失败 / 超时 / 取消：`SolverExecutionError`，携带完整
///   worker 审计记录（manager 仍可原子落库）。
pub async fn run_domain(
    selector: Arc<dyn WorkerRuntimeSelector>,
    solver_name: &str,
    context: &SolverContext,
) -> Result<SolverResult, SolverError> {
    let preferred = context
        .config
        .get(CONFIG_WORKER_RUNTIME)
        .and_then(serde_json::Value::as_str);
    let runtime = selector.select(preferred).await.map_err(|error| {
        let label = match error.kind {
            WorkerRuntimeErrorKind::NotInstalled => "not installed",
            WorkerRuntimeErrorKind::NotReady => "not ready (configuration required)",
            WorkerRuntimeErrorKind::Unavailable => "unavailable",
            WorkerRuntimeErrorKind::Unsupported => "unsupported",
            WorkerRuntimeErrorKind::Timeout => "timeout",
            WorkerRuntimeErrorKind::Cancelled => "cancelled",
            WorkerRuntimeErrorKind::Internal => "internal error",
        };
        SolverError::Other(format!(
            "external worker runtime unavailable ({label}): {}",
            error.message
        ))
    })?;
    let runtime_type = runtime.runtime_type();
    let timeout_seconds = context
        .config
        .get(CONFIG_WORKER_TIMEOUT_SECONDS)
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(DEFAULT_TIMEOUT_SECONDS);

    let instruction_bundle =
        build_instruction(solver_name, context, runtime_type, selector.as_ref()).await;
    // 派发前先落一条"正在跑"的 WorkerRun 骨架：runtime 是 select 里才定下
    // 的（无偏好时按注册表游标轮转），不先落库的话，worker 正在跑的这几
    // 分钟里前端没有任何来源能回答"这次是谁在干"——会话面板只能显示通用
    // 的 "Worker · 未关联意图 #xxx"。adapter 随后用同一 id upsert 覆盖
    // 终态。落库失败只 warn，不影响执行。
    let attribution = agents::worker::DispatchAttribution {
        project_id: context.project_id.clone(),
        mission_id: context.mission_id.clone(),
        branch_id: context.branch_id.clone(),
        run_id: context.run_id.clone(),
        task_id: context.task_id.clone(),
        instruction: instruction_bundle.instruction.clone(),
        runtime_type,
        // MCP grant 已按这个 id 发了 scope；骨架复用同一 id，broker 的
        // insert_session 才能反查 agent_preset_id 收紧授权。
        preferred_id: context
            .worker_mcp
            .as_ref()
            .map(|binding| binding.worker_run_id.clone()),
        agent_preset_id: instruction_bundle.preset_id.clone(),
    };
    let preallocated_worker_run_id = selector.begin_dispatch(&attribution).await;
    let mut request =
        WorkerExecutionRequest::start(instruction_bundle.instruction, timeout_seconds);
    // 命令禁则前缀随请求下传到 adapter：Claude Code 译成
    // `--disallowedTools` 真拦；其余 runtime 无 per-command 原生面，
    // 仅作审计记录（禁示已由指令尾部的提示词块承载）。
    request.denied_command_prefixes = CommandPolicy::from_config(&context.config).denied_prefixes;
    request.workdir = Some(crate::domains::artifact_dir_for(context));
    if let Some(binding) = context.worker_mcp.as_ref() {
        request.config_dir = Some(binding.config_dir.clone());
        request.mcp_url = Some(binding.endpoint.clone());
        request.mcp_bearer_token = Some(binding.bearer_token.clone());
    }
    if let Some(worker_run_id) = preallocated_worker_run_id {
        // 用预分配的 id（MCP 场景下就是 grant scope 那个 id），保证骨架行被
        // 终态覆盖，而不是留下第二条记录。
        request.worker_run_id = Some(worker_run_id);
    }

    let dispatched = runtime.start(request.clone()).await.map_err(map_runtime_error)?;
    let mut outcome = dispatched;
    // 中断注入（终端 Ctrl+C 的编排版）：operator 中断了正在跑的 worker
    // 并留下消息时，**停止的只是上一次执行，不是任务本身**——用 resume
    // （保留 worker 会话记忆）或 fresh start（全量指令重发，已含注入
    // 消息）把任务继续跑下去，不走失败路径。
    let worker_run_id = outcome.run.id.clone();
    if outcome.run.status == WorkerRunStatus::Cancelled
        && let Some(message) = selector.take_interrupt_message(&worker_run_id).await
    {
        let mut retry = request.clone();
        retry.instruction = enrich_with_interrupt(&retry.instruction, &message);
        retry.worker_run_id = Some(worker_run_id.clone());
        // resume 需要运行中上报过的会话引用；拿不到（thread.started 之前
        // 就被打断 / 该 runtime 不支持 resume）就 fresh start。
        let known_session = selector.worker_session_ref(&worker_run_id).await;
        tracing::info!(
            worker_run_id = %worker_run_id,
            session_ref = ?known_session,
            "operator interrupt consumed; choosing continuation mode"
        );
        let resumed = match known_session {
            Some(session_ref) => {
                retry.session_ref = Some(session_ref);
                runtime.resume(retry).await
            }
            None => runtime.start(retry).await,
        };
        match resumed {
            Ok(retry_outcome) => {
                tracing::info!(
                    worker_run_id = %worker_run_id,
                    "operator interrupt injected; worker continued with the new instruction"
                );
                outcome = retry_outcome;
            }
            Err(error) => {
                // 续跑失败不掩盖原取消事实：保留 cancelled 结局，上层照常
                // 按失败落账（任务会被标记失败，运营者能看到原因）。
                tracing::warn!(
                    worker_run_id = %worker_run_id,
                    error = %error.message,
                    "interrupt continuation failed; cancelled outcome stands"
                );
            }
        }
    }
    // 终结簿记清理：会话引用与中断登记随终态一起销（任何出口都走到）。
    selector.forget_worker(&worker_run_id).await;
    // transcript 密封：无论成败，外部输出都成为 SHA-256 绑定的工件。
    seal_transcript(context, &mut outcome);

    apply_provenance(context, &mut outcome);
    // 预设审计：本次派发实际使用的预设 key（内置默认为 None）。
    outcome.run.agent_preset_id = instruction_bundle.preset_id;
    let mut result = SolverResult::default();
    result.worker_runs.push(outcome.run.clone());
    result.worker_invocations.push(outcome.invocation.clone());

    if worker_run_failed(&outcome.run) {
        let message = match outcome.run.status {
            WorkerRunStatus::Timeout => format!(
                "external worker runtime '{}' timed out after {timeout_seconds}s",
                runtime_type.as_str()
            ),
            WorkerRunStatus::Cancelled => "external worker run was cancelled".to_string(),
            _ => format!(
                "external worker runtime '{}' failed: {}",
                runtime_type.as_str(),
                outcome.run.error.as_deref().unwrap_or("no detail")
            ),
        };
        return Err(SolverError::Execution(
            SolverExecutionError::new(message)
                .with_worker_runs(result.worker_runs)
                .with_worker_invocations(result.worker_invocations),
        ));
    }

    // 终稿优先：worker 最后一句面向用户的纯文本是本次派发要回给用户的
    // 内容（settlement 契约）。adapter 已把它有界化进
    // `WorkerInvocation.summary`；拿不到才退化成状态行。
    let worker_summary = outcome
        .invocation
        .summary
        .as_deref()
        .or(outcome.run.summary.as_deref())
        .map(str::trim)
        .filter(|text| !text.is_empty());
    result.notes = Some(match worker_summary {
        Some(text) => {
            format!("{}\n\n[worker '{}' status={}]", text, runtime_type.as_str(), outcome.run.status.as_str())
        }
        None => format!(
            "external worker '{}' status={}",
            runtime_type.as_str(),
            outcome.run.status.as_str()
        ),
    });

    // Fact 的 statement 是用户和下游 agent 唯一能看见的一行。此前它只写
    // 状态，worker 真正做了什么（终稿）被埋在 `WorkerRun.summary` 里，
    // 前端 fact 列表完全看不到——用户看到的就是一句
    // "completed a run (status succeeded)"，等于没有输出。
    //
    // 现在把终稿并进 statement。安全性有两点保证：终稿已由 adapter 有界化
    // （`WORKER_FINAL_MESSAGE_CHARS` = 2000）；且 `context.rs` 的
    // `include_if_budget` 按 token 预算挑 fact，超长的只会更早被丢弃，
    // 不会把指令撑爆。
    //
    // 终稿打头、harness/状态收尾成小标签（与 `result.notes` 同款）。早先
    // 是 "external worker 'x' completed a run (status y): <终稿>"，把用户
    // 唯一想看的产出压在一句套话后面——fact 列表先读到的永远是前缀。
    // 状态没有单独进 `fact.data`，所以保留这个尾标签，信息不丢。
    let statement = match worker_summary {
        Some(text) => format!(
            "{} [worker '{}' status={}]",
            text,
            runtime_type.as_str(),
            outcome.run.status.as_str()
        ),
        None => format!(
            "external worker '{}' completed a run (status {})",
            runtime_type.as_str(),
            outcome.run.status.as_str()
        ),
    };
    let mut fact = Fact::new(
        context.project_id.clone(),
        "worker.run.completed".to_string(),
        statement,
    );
    fact.data.insert(
        "worker_run_id".to_string(),
        serde_json::Value::String(outcome.run.id.clone()),
    );
    fact.data.insert(
        "runtime".to_string(),
        serde_json::Value::String(runtime_type.as_str().to_string()),
    );
    // 终稿单独存一份：statement 是给人看的拼接行，这里留干净的原文，
    // 下游程序化消费不必再解析前缀。
    if let Some(text) = worker_summary {
        fact.data.insert(
            "summary".to_string(),
            serde_json::Value::String(text.to_string()),
        );
    }
    if let Some(model) = outcome.run.model.clone() {
        fact.data
            .insert("model".to_string(), serde_json::Value::String(model));
    }
    result.new_facts.push(fact);
    Ok(result)
}

fn map_runtime_error(error: agents::worker::WorkerRuntimeError) -> SolverError {
    let label = match error.kind {
        WorkerRuntimeErrorKind::NotInstalled => "not installed",
        WorkerRuntimeErrorKind::NotReady => "not ready (configuration required)",
        WorkerRuntimeErrorKind::Unavailable => "unavailable",
        WorkerRuntimeErrorKind::Unsupported => "unsupported",
        WorkerRuntimeErrorKind::Timeout => "timeout",
        WorkerRuntimeErrorKind::Cancelled => "cancelled",
        WorkerRuntimeErrorKind::Internal => "internal error",
    };
    SolverError::Other(format!(
        "external worker runtime {label}: {}",
        error.message
    ))
}

/// 指令构造产物：渲染后的指令 + 实际使用的预设 key（审计）。
struct InstructionBundle {
    instruction: String,
    /// 内置默认为 `None`；mission/profile 指定且命中时为预设 key。
    preset_id: Option<String>,
}

/// Agent 预设解析优先级（WP4）：mission 配置指定 > worker runtime
/// profile 绑定 > 内置默认。「自定义留空 = 使用内置默认」——指定的
/// key 缺失或停用时继续回落下一层。
async fn resolve_worker_preset(
    context: &SolverContext,
    selector: &dyn WorkerRuntimeSelector,
    runtime_type: WorkerRuntimeType,
) -> Option<AgentPreset> {
    let source = context.agent_preset_source.as_ref()?;
    let mission_key = context
        .config
        .get(CONFIG_AGENT_PRESET)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let profile_key = selector.profile_agent_preset_key(runtime_type).await;
    let mut keys: Vec<&str> = Vec::new();
    if let Some(key) = mission_key {
        keys.push(key);
    }
    if let Some(key) = profile_key.as_deref() {
        keys.push(key);
    }
    for key in keys {
        if let Some(preset) = source.enabled_preset(key) {
            return Some(preset);
        }
    }
    None
}

/// 确定性任务指令（WP4 起经 Agent 预设模板渲染）：目标 / intent /
/// 目标描述符 / 预算 / 输出契约。不含密钥与无关上下文。未指定预设时
/// 使用内置 v1 模板——渲染结果与改造前硬编码逐字节一致（parity 测试
/// 守护）。
///
/// 命令禁则块以**代码所有的尾部**追加（预设模板与内置模板等同对待）：
/// 黑盒 CLI 的原生 deny 面只覆盖部分 runtime，提示词层是唯一的全局
/// 兜底，必须不能被预设编辑掉（与参考实现的 code-owned tail 同构）。
async fn build_instruction(
    solver_name: &str,
    context: &SolverContext,
    runtime_type: WorkerRuntimeType,
    selector: &dyn WorkerRuntimeSelector,
) -> InstructionBundle {
    let mut vars = instruction_vars(solver_name, context, runtime_type);
    let policy = CommandPolicy::from_config(&context.config);
    match resolve_worker_preset(context, selector, runtime_type).await {
        Some(preset) => {
            // 预设的步数预算覆盖（配置面：预设编辑器的「步数预算」）。
            if let Some(max_turns) = preset.max_turns {
                vars.insert(
                    "budget_steps".to_string(),
                    serde_json::Value::String(max_turns.to_string()),
                );
            }
            InstructionBundle {
                instruction: with_traffic_block(with_command_policy(
                    render_template(&preset.instruction_template, &vars),
                    &policy,
                )),
                preset_id: Some(preset.key),
            }
        }
        None => InstructionBundle {
            instruction: with_traffic_block(with_command_policy(
                render_template(WORKER_INSTRUCTION_V1, &vars),
                &policy,
            )),
            preset_id: None,
        },
    }
}

/// 把命令禁则块追加到指令尾部（空策略原样返回）。
fn with_command_policy(instruction: String, policy: &CommandPolicy) -> String {
    let block = policy.prompt_block();
    if block.is_empty() {
        instruction
    } else {
        format!("{instruction}\n\n{block}")
    }
}

/// 流量录制启用时追加流量工具指引（代码所有，预设编辑绕不过）。
///
/// 与参考实现的 `workerTrafficBlock` 同构：让 worker 先查已录制的流量、
/// 不要重复 curl 同一 URL——录制代理在跑才有意义。
fn with_traffic_block(instruction: String) -> String {
    if crate::traffic::global_traffic().is_none() {
        return instruction;
    }
    format!(
        "{instruction}\n\n【流量工具】\n- traffic_search（host 必填）：回看本机已录制的 HTTP 交换索引（id/method/url/status），\
支持 body_contains 在请求/响应正文里做子串检索（至少 3 字符）。\n\
- traffic_get(id)：取某条交换的完整请求/响应原文。\n\
先查流量、不要重复 curl 同一 URL；要更多结果显式调大 limit。"
    )
}

/// 模板变量集（对齐 `SolverContext`：mission_goal / intent / targets /
/// step budget / 输出契约）。`has_*` 布尔变量承载旧逻辑的存在性分支。
pub(crate) fn instruction_vars(
    solver_name: &str,
    context: &SolverContext,
    runtime_type: WorkerRuntimeType,
) -> serde_json::Map<String, serde_json::Value> {
    use serde_json::Value;
    let mut vars = serde_json::Map::new();
    let mut insert = |key: &str, value: Value| {
        vars.insert(key.to_string(), value);
    };
    insert("solver_name", Value::String(solver_name.to_string()));
    insert(
        "runtime_display_name",
        Value::String(runtime_type.display_name().to_string()),
    );
    // intent 存在性是独立分支（与 title 非空无关，保持旧字节语义）。
    insert(
        "has_intent",
        Value::String(if context.intent.is_some() { "1" } else { "" }.to_string()),
    );
    if let Some(intent) = context.intent.as_ref() {
        insert("intent_title", Value::String(intent.title.clone()));
        insert(
            "has_intent_description",
            Value::String(
                if intent
                    .description
                    .as_deref()
                    .filter(|value| !value.is_empty())
                    .is_some()
                {
                    "1".to_string()
                } else {
                    String::new()
                },
            ),
        );
        insert(
            "intent_description",
            Value::String(intent.description.clone().unwrap_or_default()),
        );
    } else {
        insert("intent_title", Value::String(String::new()));
        insert("has_intent_description", Value::String(String::new()));
        insert("intent_description", Value::String(String::new()));
    }
    let targets = if context.target.is_empty() {
        String::new()
    } else {
        context
            .target
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    insert("has_targets", Value::String(if targets.is_empty() { String::new() } else { "1".to_string() }));
    insert("targets", Value::String(targets));
    let mission_goal = context
        .config
        .get("mission_goal")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    insert(
        "has_mission_goal",
        Value::String(if mission_goal.is_some() { "1".to_string() } else { String::new() }),
    );
    insert(
        "mission_goal",
        Value::String(mission_goal.unwrap_or_default()),
    );
    // 操作约束（OPSEC 红线）：mission.constraints 经 config 到达这里。
    // 空约束 = 不渲染约束块，保持无约束任务的指令字节不变。
    let constraints = context
        .config
        .get("constraints")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    insert(
        "has_constraints",
        Value::String(if constraints.is_empty() {
            String::new()
        } else {
            "1".to_string()
        }),
    );
    insert(
        "constraints",
        Value::String(
            constraints
                .iter()
                .map(|item| format!("- {item}"))
                .collect::<Vec<_>>()
                .join("\n"),
        ),
    );
    // 预热知识：bootstrap 检索注入的知识卡摘要（config.knowledge_hints）。有则
    // 渲染"相关知识"块，无则不占字节。这是知识到达 worker 的**预热**通道（此前
    // 注入后无人消费）；worker 仍可 knowledge_search 深拉完整卡片。
    let knowledge_hints = context
        .config
        .get(models::CONFIG_KNOWLEDGE_HINTS)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    insert(
        "has_knowledge",
        Value::String(if knowledge_hints.is_empty() {
            String::new()
        } else {
            "1".to_string()
        }),
    );
    insert(
        "knowledge",
        Value::String(
            knowledge_hints
                .iter()
                .map(|item| format!("- {item}"))
                .collect::<Vec<_>>()
                .join("\n"),
        ),
    );
    insert("budget_steps", Value::String(context.budget_steps.to_string()));
    vars
}



/// 把 stdout/stderr（原始有界捕获）密封为工件并回填路径。
fn seal_transcript(context: &SolverContext, outcome: &mut agents::worker::WorkerExecutionOutcome) {
    let bytes = std::mem::take(&mut outcome.transcript);
    if bytes.is_empty() {
        return;
    }
    // transcript 密封：SHA-256 绑定的 SealedArtifact，落 mission 工件目录。
    let artifact_dir = crate::domains::artifact_dir_for(context).join("worker");
    if std::fs::create_dir_all(&artifact_dir).is_err() {
        return;
    }
    let path = artifact_dir.join(format!("{}.transcript", uuid::Uuid::new_v4().simple()));
    let sealed = SealedArtifact::seal(bytes);
    if sealed.persist(&path).is_ok() {
        outcome.run.transcript_path = Some(path.to_string_lossy().into_owned());
    }
}

/// 回填 mission/run/branch/task provenance（独立执行时字段保持 None）。
fn apply_provenance(context: &SolverContext, outcome: &mut agents::worker::WorkerExecutionOutcome) {
    outcome.run.project_id = context.project_id.clone();
    outcome.run.mission_id = context.mission_id.clone();
    outcome.run.run_id = Some(context.run_id.clone());
    outcome.run.branch_id = context.branch_id.clone();
    outcome.run.task_id = Some(context.task_id.clone());
    outcome.invocation.project_id = Some(context.project_id.clone());
    outcome.invocation.worker_run_id = Some(outcome.run.id.clone());
}

#[cfg(test)]
mod parity_tests {
    #![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
    use super::*;
    use async_trait::async_trait;

    /// 构造指定形态的 SolverContext（字段与 branch_runtime 装配同构）。
    fn context(
        intent_title: Option<&str>,
        intent_description: Option<&str>,
        targets: &[(&str, &str)],
        mission_goal: Option<&str>,
    ) -> SolverContext {
        use models::intent::Intent;
        use models::{ProjectId, RunId, TaskId};
        let mut context = SolverContext::new(
            ProjectId::new("proj_p".to_string()),
            RunId::new("run_p".to_string()),
            TaskId::new("task_p".to_string()),
        );
        if let Some(title) = intent_title {
            let mut intent = Intent::new(ProjectId::new("proj_p".to_string()), title.to_string());
            intent.description = intent_description.map(str::to_string);
            context.intent = Some(intent);
        }
        for (key, value) in targets {
            context
                .target
                .insert((*key).to_string(), (*value).to_string());
        }
        if let Some(goal) = mission_goal {
            context
                .config
                .insert("mission_goal".to_string(), serde_json::json!(goal));
        }
        context.budget_steps = 40;
        context
    }

    /// Bootstrap 预热知识进指令：config.knowledge_hints 非空时渲染"相关知识"
    /// 块（worker 拿到预热线索），为空时不占字节。这是知识到达 worker 的预热
    /// 通道（此前注入后无人消费）。
    #[tokio::test]
    async fn knowledge_hints_render_into_instruction_when_present() {
        let mut with_knowledge =
            context(Some("Enumerate surface"), None, &[("url", "https://t.example")], Some("Root it"));
        with_knowledge.config.insert(
            models::CONFIG_KNOWLEDGE_HINTS.to_string(),
            serde_json::json!(["Nuclei: template-based scanning", "httpx: fast HTTP probing"]),
        );
        let rendered =
            build_instruction("web_recon", &with_knowledge, WorkerRuntimeType::Codex, &noop_selector())
                .await
                .instruction;
        assert!(
            rendered.contains("相关知识"),
            "预热知识块必须渲染：{rendered}"
        );
        assert!(
            rendered.contains("- Nuclei: template-based scanning"),
            "知识条目必须逐条渲染：{rendered}"
        );

        // 无预热知识 → 不渲染该块（保持无知识任务的指令字节不变）。
        let bare =
            context(Some("Enumerate surface"), None, &[("url", "https://t.example")], Some("Root it"));
        let rendered_bare =
            build_instruction("web_recon", &bare, WorkerRuntimeType::Codex, &noop_selector())
                .await
                .instruction;
        assert!(
            !rendered_bare.contains("相关知识"),
            "无预热知识时不得渲染该块：{rendered_bare}"
        );
    }

    /// 内置模板的渲染契约（P1 出洞纪律对齐后）：任务目标 / 约束块 /
    /// 意图 / 目标描述符 / 预算 / 纪律条款按存在性渲染，跨 runtime 一致。
    #[tokio::test]
    async fn builtin_template_renders_mission_goal_constraints_and_disciplines() {
        let cases: [(&str, Vec<(&str, &str)>, Option<&str>, Option<&str>, Option<&str>); 6] = [
            ("full", vec![("url", "https://t.example")], Some("Find issues"), Some("Scan the login surface"), Some("Root the target")),
            ("no_intent", vec![("url", "https://t.example")], None, None, Some("Root the target")),
            ("no_intent_no_targets", vec![], None, None, Some("Root the target")),
            ("no_intent_no_targets_no_goal", vec![], None, None, None),
            ("full_no_goal", vec![("host", "h.example"), ("ip", "10.0.0.1")], Some("Enumerate"), Some("All subdomains"), None),
            ("intent_no_desc", vec![("url", "https://t.example")], Some("Bare intent"), None, None),
        ];
        for (name, targets, intent, desc, goal) in cases {
            let mut context = context(intent, desc, &targets, goal);
            context.config.insert(
                "constraints".to_string(),
                serde_json::json!(["only test t.example", "no port scanning"]),
            );
            for runtime in WorkerRuntimeType::all() {
                let rendered = build_instruction("web_recon", &context, runtime, &noop_selector())
                    .await
                    .instruction;
                // 任务目标与约束块按存在性渲染。
                if let Some(goal) = goal {
                    assert!(rendered.contains(&format!("任务目标：{goal}")), "case={name}");
                } else {
                    assert!(!rendered.contains("任务目标："), "case={name}");
                }
                assert!(
                    rendered.contains("【操作约束（最高优先级"),
                    "约束块必须渲染：case={name}"
                );
                assert!(rendered.contains("- only test t.example"), "case={name}");
                // 意图与目标描述符。
                if let Some(intent) = intent {
                    assert!(rendered.contains(intent), "case={name}");
                } else {
                    assert!(
                        !rendered.contains("你领到的意图（本次唯一任务"),
                        "case={name}"
                    );
                }
                if let Some((key, value)) = targets.first() {
                    assert!(
                        rendered.contains(&format!("{key}={value}")),
                        "目标描述符必须渲染：case={name}"
                    );
                } else {
                    assert!(!rendered.contains("项目目标描述符"), "case={name}");
                }
                // 纪律与预算恒在。
                assert!(rendered.contains("对抗式自检"), "case={name}");
                assert!(rendered.contains("一条路要走透再下结论"), "case={name}");
                assert!(rendered.contains("步骤预算：40"), "case={name}");
                // 未指定预设 → 审计为内置默认（None）。
                assert!(
                    build_instruction("web_recon", &context, runtime, &noop_selector())
                        .await
                        .preset_id
                        .is_none()
                );
            }
        }
    }

    /// 无约束任务的指令不出现约束块（字节面保持干净）。
    #[tokio::test]
    async fn template_without_constraints_omits_constraint_block() {
        let context = context(Some("I"), None, &[("url", "https://t.example")], Some("goal"));
        let rendered = build_instruction("web_recon", &context, WorkerRuntimeType::Codex, &noop_selector())
            .await
            .instruction;
        assert!(!rendered.contains("操作约束"));
        assert!(!rendered.contains("- only test"));
    }

    /// 命令禁则块是**代码所有的尾部**：默认种子策略下内置模板与自定义
    /// 预设都带上它（预设编辑绕不过）；显式关闭才消失。
    #[tokio::test]
    async fn command_policy_block_is_code_owned_tail() {
        // 默认（config 无策略键）→ 种子策略生效。
        let default_ctx = context(Some("I"), None, &[("url", "https://t.example")], Some("goal"));
        let rendered = build_instruction("web_recon", &default_ctx, WorkerRuntimeType::Codex, &noop_selector())
            .await
            .instruction;
        assert!(rendered.contains("【命令禁则"), "默认必须带禁则块");
        assert!(rendered.contains("- rm -rf /"), "默认前缀必须列出");
        assert!(rendered.contains("DROP DATABASE"), "提示词层禁则必须全量");

        // 显式关闭 → 不渲染。
        let mut disabled = context(Some("I"), None, &[("url", "https://t.example")], None);
        disabled.config.insert(
            crate::worker::CONFIG_COMMAND_POLICY.to_string(),
            serde_json::json!({"enabled": false}),
        );
        let off = build_instruction("web_recon", &disabled, WorkerRuntimeType::Codex, &noop_selector())
            .await
            .instruction;
        assert!(!off.contains("命令禁则"), "显式关闭后不渲染");
    }

    /// 解析优先级：mission 配置 > profile 绑定 > 内置默认；停用/缺失回落。
    #[tokio::test]
    async fn preset_resolution_follows_mission_then_profile_priority() {
        use models::agent_preset::AgentPresetSource;
        use std::collections::HashMap;
        use std::sync::Mutex;

        struct StaticSource(Mutex<HashMap<String, AgentPreset>>);
        impl AgentPresetSource for StaticSource {
            fn enabled_preset(&self, key: &str) -> Option<AgentPreset> {
                let guard = self.0.lock().expect("lock");
                let preset = guard.get(key)?;
                preset.enabled.then(|| preset.clone())
            }
        }

        let now = models::common::utcnow();
        let mut custom =
            AgentPreset::new_v1("team_worker", "Team worker", None, "CUSTOM {{solver_name}} {{budget_steps}}", now);
        custom.builtin = false;
        let mut disabled = AgentPreset::new_v1("off", "Off", None, "OFF", now);
        disabled.enabled = false;

        let source: Arc<dyn models::agent_preset::AgentPresetSource> = Arc::new(StaticSource(
            Mutex::new(HashMap::from([
                ("team_worker".to_string(), custom),
                ("off".to_string(), disabled),
            ])),
        ));

        let runtime = WorkerRuntimeType::ClaudeCode;
        let selector = preset_selector(Some("team_worker"));

        // mission 指定优先。
        let mut mission_pinned = context(Some("I"), None, &[("url", "https://t.example")], None);
        mission_pinned.config.insert(
            CONFIG_AGENT_PRESET.to_string(),
            serde_json::json!("team_worker"),
        );
        mission_pinned.agent_preset_source = Some(Arc::clone(&source));
        let bundle = build_instruction("web_recon", &mission_pinned, runtime, &selector).await;
        assert_eq!(bundle.preset_id, Some("team_worker".to_string()));
        assert!(bundle.instruction.starts_with("CUSTOM web_recon"));

        // 无 mission 指定 → profile 绑定。
        let mut profile_bound = context(Some("I"), None, &[("url", "https://t.example")], None);
        profile_bound.agent_preset_source = Some(Arc::clone(&source));
        assert_eq!(
            build_instruction("web_recon", &profile_bound, runtime, &selector)
                .await
                .preset_id,
            Some("team_worker".to_string())
        );

        // mission 指定但预设停用 → 回落 profile 绑定。
        let mut fallback = context(Some("I"), None, &[("url", "https://t.example")], None);
        fallback
            .config
            .insert(CONFIG_AGENT_PRESET.to_string(), serde_json::json!("off"));
        fallback.agent_preset_source = Some(Arc::clone(&source));
        assert_eq!(
            build_instruction("web_recon", &fallback, runtime, &selector)
                .await
                .preset_id,
            Some("team_worker".to_string())
        );

        // mission + profile 都未指定 → 内置默认（审计 None）。
        let mut plain = context(Some("I"), None, &[("url", "https://t.example")], None);
        plain.agent_preset_source = Some(Arc::clone(&source));
        let plain_bundle = build_instruction("web_recon", &plain, runtime, &noop_selector()).await;
        assert!(plain_bundle.preset_id.is_none());
        assert!(plain_bundle.instruction.contains("执行纪律"));
    }


    // —— 测试替身：无 profile 绑定 / 带 profile 绑定的 selector ——

    fn noop_selector() -> NoopSelector {
        NoopSelector
    }

    fn preset_selector(key: Option<&str>) -> PresetSelector {
        PresetSelector {
            key: key.map(str::to_string),
        }
    }

    struct NoopSelector;

    #[async_trait]
    impl WorkerRuntimeSelector for NoopSelector {
        async fn select(
            &self,
            _preferred: Option<&str>,
        ) -> Result<Arc<dyn agents::worker::WorkerRuntime>, agents::worker::WorkerRuntimeError>
        {
            unreachable!("parity tests never dispatch");
        }
        async fn probes(&self) -> Vec<models::worker::WorkerProbe> {
            Vec::new()
        }
        fn runtime(
            &self,
            _runtime_type: WorkerRuntimeType,
        ) -> Option<Arc<dyn agents::worker::WorkerRuntime>> {
            None
        }
    }

    struct PresetSelector {
        key: Option<String>,
    }

    #[async_trait]
    impl WorkerRuntimeSelector for PresetSelector {
        async fn select(
            &self,
            _preferred: Option<&str>,
        ) -> Result<Arc<dyn agents::worker::WorkerRuntime>, agents::worker::WorkerRuntimeError>
        {
            unreachable!("parity tests never dispatch");
        }
        async fn probes(&self) -> Vec<models::worker::WorkerProbe> {
            Vec::new()
        }
        fn runtime(
            &self,
            _runtime_type: WorkerRuntimeType,
        ) -> Option<Arc<dyn agents::worker::WorkerRuntime>> {
            None
        }
        async fn profile_agent_preset_key(
            &self,
            _runtime_type: WorkerRuntimeType,
        ) -> Option<String> {
            self.key.clone()
        }
    }
}

#[cfg(test)]
mod interrupt_tests {
    use super::enrich_with_interrupt;

    #[test]
    fn enrich_appends_operator_message_with_clear_delimiters() {
        let enriched = enrich_with_interrupt("审计 https://x.test 的登录接口", "先看 sort 参数的注入");
        assert!(enriched.starts_with("审计 https://x.test 的登录接口"), "原指令必须完整在前");
        assert!(enriched.contains("=== operator interrupt (new instruction from the user) ==="));
        assert!(enriched.contains("先看 sort 参数的注入"));
        assert!(enriched.contains("continue the task with the new instruction"), "必须要求带着新指令继续");
        assert!(enriched.ends_with("do not repeat work already done ==="), "必须要求不重复已完成工作");
    }

    #[test]
    fn enrich_keeps_multiline_instruction_intact() {
        let instruction = "第一行\n第二行\n第三行";
        let enriched = enrich_with_interrupt(instruction, "新指令");
        assert!(enriched.contains(instruction), "多行原指令不得被改写");
    }
}
