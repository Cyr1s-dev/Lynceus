//! 持久、有界的执行控制面。
//!
//! worker 当前仍在进程内，但每次状态迁移先写 SQLite。公开的本地后端是
//! fail-closed 安全适配器：没有真正绑定 allow-listed 工具时只会返回
//! `denied`，绝不把“请求到达占位层”伪装成任务成功。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use models::{
    ExecutionBackendType, ExecutionJob, ExecutionRequest, ExecutionResult, ExecutionStatus,
    ToolInvocation, ToolStatus, utcnow,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use storage::{Repository, StorageError, redact_text, redact_value};
use tokio::sync::{Mutex, Notify};
use tokio::task::JoinHandle;

/// 后端执行失败；错误只保存脱敏摘要。
#[derive(Debug, Clone, thiserror::Error)]
#[error("{0}")]
pub struct ExecutionBackendError(pub String);

/// 执行控制面错误。
#[derive(Debug, thiserror::Error)]
pub enum ExecutionControlError {
    /// 存储失败。
    #[error(transparent)]
    Storage(#[from] StorageError),
    /// 请求参数不合法。
    #[error("{0}")]
    Invalid(String),
    /// 执行不存在。
    #[error("execution not found: {0}")]
    NotFound(String),
    /// 幂等键或稳定 ID 与既有请求冲突。
    #[error("{0}")]
    Conflict(String),
    /// 同一后端家族被重复注册。
    #[error("execution backend already registered: {0}")]
    DuplicateBackend(&'static str),
}

/// 可插拔的有界执行后端。
#[async_trait]
pub trait ExecutionBackend: Send + Sync {
    /// 后端家族。
    fn backend_type(&self) -> ExecutionBackendType;
    /// 执行一个结构化请求。
    async fn execute(
        &self,
        request: &ExecutionRequest,
    ) -> Result<ExecutionResult, ExecutionBackendError>;
    /// 尽力取消；`false` 表示后端没有可取消的实时作业。
    async fn cancel(&self, request_id: &str) -> Result<bool, ExecutionBackendError>;
}

/// 安全本地后端。
///
/// 它不运行 shell，也不假装运行了结构化工具。真正的本地工具必须通过
/// `ExecutionBackend` 注册受限适配器后才能产生 `succeeded`。
#[derive(Debug, Default)]
pub struct SafeLocalExecutionBackend;

#[async_trait]
impl ExecutionBackend for SafeLocalExecutionBackend {
    fn backend_type(&self) -> ExecutionBackendType {
        ExecutionBackendType::Local
    }

    async fn execute(
        &self,
        request: &ExecutionRequest,
    ) -> Result<ExecutionResult, ExecutionBackendError> {
        let now = utcnow();
        let reason = if request.command.is_empty() {
            format!(
                "no allow-listed local adapter is registered for tool: {}",
                request.tool_name
            )
        } else {
            "raw command execution is disabled".to_string()
        };
        Ok(ExecutionResult {
            id: models::ExecutionResultId::new(models::new_id("execres")),
            request_id: request.id.clone(),
            project_id: request.project_id.clone(),
            run_id: request.run_id.clone(),
            task_id: request.task_id.clone(),
            session_id: request.session_id.clone(),
            tool_name: request.tool_name.clone(),
            backend_type: ExecutionBackendType::Local,
            status: ExecutionStatus::Denied,
            stdout_summary: String::new(),
            stderr_summary: reason.clone(),
            exit_code: None,
            error: Some(reason),
            artifacts: Vec::new(),
            started_at: now,
            completed_at: Some(now),
        })
    }

    async fn cancel(&self, _request_id: &str) -> Result<bool, ExecutionBackendError> {
        Ok(false)
    }
}

/// 提交参数。
#[derive(Debug, Clone)]
pub struct SubmitExecutionOptions {
    /// 失败后是否安全重试。
    pub safe_to_retry: bool,
    /// 最大尝试次数。
    pub max_attempts: u8,
    /// 可选幂等键。
    pub idempotency_key: Option<String>,
}

impl Default for SubmitExecutionOptions {
    fn default() -> Self {
        Self {
            safe_to_retry: false,
            max_attempts: 1,
            idempotency_key: None,
        }
    }
}

/// bounded wait 的结果；超时不改变底层执行状态。
#[derive(Debug, Clone, PartialEq)]
pub struct ExecutionWaitOutcome {
    /// 当前 durable job。
    pub job: ExecutionJob,
    /// 仅表示调用者等待超时。
    pub timed_out: bool,
}

/// 持久执行控制器。
pub struct ExecutionControlPlane {
    repository: Arc<dyn Repository>,
    backends: HashMap<ExecutionBackendType, Arc<dyn ExecutionBackend>>,
    tasks: Mutex<HashMap<String, JoinHandle<()>>>,
    events: Mutex<HashMap<String, Arc<Notify>>>,
    mutation_lock: Mutex<()>,
}

impl ExecutionControlPlane {
    /// 以一组互斥后端构造控制器。
    ///
    /// # Errors
    /// 同一后端家族被注册两次。
    pub fn new(
        repository: Arc<dyn Repository>,
        backends: impl IntoIterator<Item = Arc<dyn ExecutionBackend>>,
    ) -> Result<Self, ExecutionControlError> {
        let mut registered = HashMap::new();
        for backend in backends {
            let backend_type = backend.backend_type();
            if registered.insert(backend_type, backend).is_some() {
                return Err(ExecutionControlError::DuplicateBackend(
                    backend_type.as_str(),
                ));
            }
        }
        Ok(Self {
            repository,
            backends: registered,
            tasks: Mutex::new(HashMap::new()),
            events: Mutex::new(HashMap::new()),
            mutation_lock: Mutex::new(()),
        })
    }

    /// 构造只公开 fail-closed 本地后端的控制器。
    ///
    /// # Errors
    /// 固定后端集合不应冲突；保留 `Result` 让启动路径不依赖 panic。
    pub fn safe_local(repository: Arc<dyn Repository>) -> Result<Self, ExecutionControlError> {
        Self::new(repository, [Arc::new(SafeLocalExecutionBackend) as Arc<_>])
    }

    /// 已注册后端类型。
    #[must_use]
    pub fn backend_types(&self) -> Vec<ExecutionBackendType> {
        let mut values = self.backends.keys().copied().collect::<Vec<_>>();
        values.sort_unstable_by_key(|item| item.as_str());
        values
    }

    /// 持久化 queued job，并将实际执行从 HTTP 等待中分离。
    ///
    /// # Errors
    /// 请求非法、后端未注册、幂等键冲突或存储失败。
    pub async fn submit(
        self: &Arc<Self>,
        request: ExecutionRequest,
        options: SubmitExecutionOptions,
    ) -> Result<ExecutionJob, ExecutionControlError> {
        validate_request(&request, &options)?;
        if !self.backends.contains_key(&request.backend_type) {
            return Err(ExecutionControlError::Invalid(format!(
                "execution backend is not registered: {}",
                request.backend_type.as_str()
            )));
        }
        let digest = request_digest(&request)?;
        let normalized_key = options
            .idempotency_key
            .as_deref()
            .map(str::trim)
            .filter(|key| !key.is_empty())
            .map(ToOwned::to_owned);

        let guard = self.mutation_lock.lock().await;
        if let Some(key) = normalized_key.as_deref() {
            for existing in self.repository.list_execution_jobs(
                request.project_id.as_ref().map(models::ProjectId::as_str),
                None,
                None,
            )? {
                if existing.idempotency_key.as_deref() == Some(key)
                    && existing.owner_id == request.owner_id
                {
                    if existing.request_digest != digest {
                        return Err(ExecutionControlError::Conflict(
                            "idempotency key already belongs to a different request".to_string(),
                        ));
                    }
                    return Ok(existing);
                }
            }
        }
        if let Some(existing) = self.repository.get_execution_job(request.id.as_str())? {
            if existing.request_digest == digest {
                return Ok(existing);
            }
            return Err(ExecutionControlError::Conflict(format!(
                "execution id already exists with different request: {}",
                request.id
            )));
        }

        let mut job = ExecutionJob::queued(redacted_request(&request)?, digest);
        job.safe_to_retry = options.safe_to_retry;
        job.max_attempts = options.max_attempts;
        job.idempotency_key = normalized_key;
        job.metadata.insert(
            "control_plane".to_string(),
            Value::String("in_process_persisted_rust_v1".to_string()),
        );
        job.metadata
            .insert("exact_request_persisted".to_string(), Value::Bool(false));
        self.repository.create_execution_job(&job)?;
        drop(guard);

        self.event_for(job.id.as_str()).await;
        let execution_id = job.id.as_str().to_string();
        let control = Arc::clone(self);
        let handle = tokio::spawn(async move {
            if let Err(error) = control.run(request).await {
                tracing::error!(execution_id, %error, "execution worker failed to persist outcome");
            }
            control.signal(&execution_id).await;
        });
        let mut tasks = self.tasks.lock().await;
        tasks.retain(|_, task| !task.is_finished());
        tasks.insert(job.id.as_str().to_string(), handle);
        Ok(job)
    }

    /// 按 ID 取 job。
    ///
    /// # Errors
    /// 存储读取失败。
    pub fn get(&self, execution_id: &str) -> Result<Option<ExecutionJob>, ExecutionControlError> {
        Ok(self.repository.get_execution_job(execution_id)?)
    }

    /// 列 job。
    ///
    /// # Errors
    /// 存储读取失败。
    pub fn list_jobs(
        &self,
        project_id: Option<&str>,
        run_id: Option<&str>,
        status: Option<ExecutionStatus>,
    ) -> Result<Vec<ExecutionJob>, ExecutionControlError> {
        Ok(self
            .repository
            .list_execution_jobs(project_id, run_id, status)?)
    }

    /// 有界等待 job 进入终态；等待超时不取消实际执行。
    ///
    /// # Errors
    /// job 不存在或存储失败。
    pub async fn wait(
        &self,
        execution_id: &str,
        timeout: Duration,
    ) -> Result<ExecutionWaitOutcome, ExecutionControlError> {
        let started = tokio::time::Instant::now();
        let event = self.event_for(execution_id).await;
        loop {
            let job = self.require(execution_id)?;
            if job.status.is_terminal() {
                return Ok(ExecutionWaitOutcome {
                    job,
                    timed_out: false,
                });
            }
            let Some(remaining) = timeout.checked_sub(started.elapsed()) else {
                return Ok(ExecutionWaitOutcome {
                    job,
                    timed_out: true,
                });
            };
            if remaining.is_zero() {
                return Ok(ExecutionWaitOutcome {
                    job,
                    timed_out: true,
                });
            }
            let poll = remaining.min(Duration::from_millis(100));
            let _ = tokio::time::timeout(poll, event.notified()).await;
        }
    }

    /// 请求取消，并在返回前持久化 `cancelled` 终态。
    ///
    /// # Errors
    /// job 不存在或持久化失败。
    pub async fn cancel(
        &self,
        execution_id: &str,
        note: &str,
    ) -> Result<ExecutionJob, ExecutionControlError> {
        let mut job = {
            let _guard = self.mutation_lock.lock().await;
            let mut job = self.require(execution_id)?;
            if job.status.is_terminal() {
                return Ok(job);
            }
            job.cancel_requested = true;
            let clean_note = redact_text(note);
            job.cancel_note =
                (!clean_note.is_empty()).then(|| clean_note.chars().take(2000).collect());
            job.status = ExecutionStatus::Cancelled;
            job.error = Some(
                job.cancel_note
                    .clone()
                    .unwrap_or_else(|| "execution was cancelled".to_string()),
            );
            let now = utcnow();
            job.completed_at = Some(now);
            job.heartbeat_at = Some(now);
            job.result = Some(terminal_result(
                &job.request,
                ExecutionStatus::Cancelled,
                job.error.clone(),
                job.started_at.unwrap_or(now),
                now,
            ));
            self.update_job(&mut job)?;
            self.ensure_tool_invocation(&mut job)?;
            job
        };

        if let Some(backend) = self.backends.get(&job.request.backend_type)
            && let Err(error) = backend.cancel(execution_id).await
        {
            tracing::warn!(execution_id, %error, "execution backend cancellation failed");
        }
        if let Some(handle) = self.tasks.lock().await.remove(execution_id) {
            handle.abort();
            let _ = handle.await;
        }
        self.signal(execution_id).await;
        job = self.require(execution_id)?;
        Ok(job)
    }

    /// 标记上一个进程遗留的非终态 job 为 orphaned。
    ///
    /// # Errors
    /// 存储读写失败。
    pub fn recover_orphaned(&self) -> Result<Vec<ExecutionJob>, ExecutionControlError> {
        let mut recovered = Vec::new();
        for mut job in self.repository.list_execution_jobs(None, None, None)? {
            if job.status.is_terminal() {
                continue;
            }
            let now = utcnow();
            job.status = ExecutionStatus::Orphaned;
            job.error = Some(
                "execution owner process exited before a terminal result was persisted".to_string(),
            );
            job.completed_at = Some(now);
            job.heartbeat_at = Some(now);
            job.result = Some(terminal_result(
                &job.request,
                ExecutionStatus::Orphaned,
                job.error.clone(),
                job.started_at.unwrap_or(job.submitted_at),
                now,
            ));
            self.update_job(&mut job)?;
            self.ensure_tool_invocation(&mut job)?;
            recovered.push(job);
        }
        Ok(recovered)
    }

    // One loop iteration is the auditable state-transition boundary; keeping
    // its branches together makes retry/cancel precedence reviewable.
    #[allow(clippy::too_many_lines)]
    async fn run(self: &Arc<Self>, request: ExecutionRequest) -> Result<(), ExecutionControlError> {
        loop {
            {
                let _guard = self.mutation_lock.lock().await;
                let mut job = self.require(request.id.as_str())?;
                if job.status.is_terminal() || job.cancel_requested {
                    return Ok(());
                }
                let now = utcnow();
                job.status = ExecutionStatus::Running;
                job.attempts = job.attempts.saturating_add(1);
                job.started_at.get_or_insert(now);
                job.heartbeat_at = Some(now);
                self.update_job(&mut job)?;
            }
            self.signal(request.id.as_str()).await;

            let backend = self
                .backends
                .get(&request.backend_type)
                .cloned()
                .ok_or_else(|| {
                    ExecutionControlError::Invalid(format!(
                        "execution backend is unavailable: {}",
                        request.backend_type.as_str()
                    ))
                })?;
            let outcome = tokio::time::timeout(
                Duration::from_secs(request.timeout_seconds),
                backend.execute(&request),
            )
            .await;

            if outcome.is_err() {
                let _ = backend.cancel(request.id.as_str()).await;
            }

            let guard = self.mutation_lock.lock().await;
            let mut job = self.require(request.id.as_str())?;
            if job.status.is_terminal() || job.cancel_requested {
                return Ok(());
            }
            let now = utcnow();
            match outcome {
                Err(_) => {
                    job.status = ExecutionStatus::HardTimeout;
                    job.error = Some(format!(
                        "execution exceeded hard timeout of {} seconds",
                        request.timeout_seconds
                    ));
                    job.result = Some(terminal_result(
                        &request,
                        job.status,
                        job.error.clone(),
                        job.started_at.unwrap_or(now),
                        now,
                    ));
                }
                Ok(Err(error)) => {
                    job.status = ExecutionStatus::Failed;
                    job.error = Some(redact_text(&format!("ExecutionBackendError: {error}")));
                    job.result = Some(terminal_result(
                        &request,
                        job.status,
                        job.error.clone(),
                        job.started_at.unwrap_or(now),
                        now,
                    ));
                }
                Ok(Ok(result)) => {
                    if result.request_id != request.id {
                        job.status = ExecutionStatus::Failed;
                        job.error = Some(format!(
                            "backend returned mismatched request id: {}",
                            result.request_id
                        ));
                        job.result = Some(terminal_result(
                            &request,
                            job.status,
                            job.error.clone(),
                            job.started_at.unwrap_or(now),
                            now,
                        ));
                    } else if !result.status.is_terminal() {
                        job.status = ExecutionStatus::Failed;
                        job.error = Some(format!(
                            "backend returned non-terminal status: {}",
                            result.status.as_str()
                        ));
                        job.result = Some(terminal_result(
                            &request,
                            job.status,
                            job.error.clone(),
                            job.started_at.unwrap_or(now),
                            now,
                        ));
                    } else {
                        let result = redacted_result(result);
                        job.status = result.status;
                        job.error.clone_from(&result.error);
                        job.completed_at = Some(result.completed_at.unwrap_or(now));
                        job.result = Some(result);
                    }
                }
            }
            job.completed_at.get_or_insert(now);
            job.heartbeat_at = Some(now);

            if should_retry(&job) {
                append_retry_history(&mut job);
                job.status = ExecutionStatus::Queued;
                job.result = None;
                job.completed_at = None;
                self.update_job(&mut job)?;
                drop(guard);
                self.signal(request.id.as_str()).await;
                tokio::time::sleep(Duration::from_millis(
                    u64::from(job.attempts).saturating_mul(50).min(200),
                ))
                .await;
                continue;
            }

            self.update_job(&mut job)?;
            self.ensure_tool_invocation(&mut job)?;
            drop(guard);
            self.signal(request.id.as_str()).await;
            return Ok(());
        }
    }

    fn require(&self, execution_id: &str) -> Result<ExecutionJob, ExecutionControlError> {
        self.repository
            .get_execution_job(execution_id)?
            .ok_or_else(|| ExecutionControlError::NotFound(execution_id.to_string()))
    }

    fn update_job(&self, job: &mut ExecutionJob) -> Result<(), ExecutionControlError> {
        job.version = job.version.saturating_add(1);
        self.repository.update_execution_job(job)?;
        Ok(())
    }

    fn ensure_tool_invocation(&self, job: &mut ExecutionJob) -> Result<(), ExecutionControlError> {
        if job.tool_invocation_id.is_some() {
            return Ok(());
        }
        for existing in self.repository.list_tool_invocations(
            job.request
                .project_id
                .as_ref()
                .map(models::ProjectId::as_str),
        )? {
            if existing
                .metadata
                .get("execution_control")
                .and_then(Value::as_object)
                .and_then(|value| value.get("execution_id"))
                .and_then(Value::as_str)
                == Some(job.id.as_str())
            {
                job.tool_invocation_id = Some(existing.id);
                self.update_job(job)?;
                return Ok(());
            }
        }

        let request = &job.request;
        let mut summaries = vec![job.partial_output_summary.clone()];
        let mut artifact_paths = request.artifact_paths.clone();
        if let Some(result) = &job.result {
            summaries.push(result.stdout_summary.clone());
            summaries.push(result.stderr_summary.clone());
            artifact_paths.extend(result.artifacts.iter().map(|item| item.path.clone()));
        }
        artifact_paths.sort_unstable();
        artifact_paths.dedup();
        let mut argument_names = request.args.keys().map(String::as_str).collect::<Vec<_>>();
        argument_names.sort_unstable();
        let mut invocation = ToolInvocation::new(
            request.tool_name.clone(),
            format!(
                "{}(backend={}, args=[{}])",
                request.tool_name,
                request.backend_type.as_str(),
                argument_names.join(", ")
            ),
        );
        invocation.project_id.clone_from(&request.project_id);
        invocation.mission_id.clone_from(&request.mission_id);
        invocation.branch_id.clone_from(&request.branch_id);
        invocation.run_id.clone_from(&request.run_id);
        invocation.task_id.clone_from(&request.task_id);
        invocation.output_summary = summaries
            .into_iter()
            .filter(|part| !part.trim().is_empty())
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(2000)
            .collect();
        invocation.status = tool_status(job.status);
        invocation.exit_code = job.result.as_ref().and_then(|result| result.exit_code);
        invocation.duration_ms = job
            .completed_at
            .zip(job.started_at)
            .map(|(completed, started)| completed.elapsed_milliseconds_since(&started));
        invocation.artifact_paths = artifact_paths;
        invocation.error = job.error.as_deref().map(redact_text);
        invocation.metadata.insert(
            "execution_control".to_string(),
            json!({
                "execution_id": job.id.as_str(),
                "status": job.status.as_str(),
                "request_digest": job.request_digest,
                "attempts": job.attempts,
                "safe_to_retry": job.safe_to_retry,
            }),
        );
        invocation.started_at = job.started_at.unwrap_or(job.submitted_at);
        invocation.finished_at = job.completed_at;
        let saved = self.repository.add_tool_invocation(&invocation)?;
        job.tool_invocation_id = Some(saved.id);
        self.update_job(job)?;
        Ok(())
    }

    async fn event_for(&self, execution_id: &str) -> Arc<Notify> {
        let mut events = self.events.lock().await;
        Arc::clone(
            events
                .entry(execution_id.to_string())
                .or_insert_with(|| Arc::new(Notify::new())),
        )
    }

    async fn signal(&self, execution_id: &str) {
        self.event_for(execution_id).await.notify_waiters();
    }
}

fn validate_request(
    request: &ExecutionRequest,
    options: &SubmitExecutionOptions,
) -> Result<(), ExecutionControlError> {
    if request.tool_name.trim().is_empty() {
        return Err(ExecutionControlError::Invalid(
            "tool_name must not be blank".to_string(),
        ));
    }
    if request.timeout_seconds == 0 {
        return Err(ExecutionControlError::Invalid(
            "timeout_seconds must be at least 1".to_string(),
        ));
    }
    if !(1..=10).contains(&options.max_attempts) {
        return Err(ExecutionControlError::Invalid(
            "max_attempts must be between 1 and 10".to_string(),
        ));
    }
    if options
        .idempotency_key
        .as_ref()
        .is_some_and(|key| key.chars().count() > 256)
    {
        return Err(ExecutionControlError::Invalid(
            "idempotency_key must not exceed 256 characters".to_string(),
        ));
    }
    if let Some(limits) = &request.resource_limits
        && (limits
            .cpu_cores
            .is_some_and(|value| value <= 0.0 || !value.is_finite())
            || limits.memory_mb.is_some_and(|value| value <= 0)
            || limits.disk_mb.is_some_and(|value| value <= 0))
    {
        return Err(ExecutionControlError::Invalid(
            "resource limits must be finite positive values".to_string(),
        ));
    }
    Ok(())
}

fn request_digest(request: &ExecutionRequest) -> Result<String, ExecutionControlError> {
    let value = serde_json::to_value(request).map_err(|error| {
        ExecutionControlError::Invalid(format!("execution request cannot be serialized: {error}"))
    })?;
    let canonical = canonicalize(value);
    let bytes = serde_json::to_vec(&canonical).map_err(|error| {
        ExecutionControlError::Invalid(format!("execution request cannot be serialized: {error}"))
    })?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn canonicalize(value: Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.into_iter().map(canonicalize).collect()),
        Value::Object(map) => {
            let mut items = map.into_iter().collect::<Vec<_>>();
            items.sort_unstable_by(|left, right| left.0.cmp(&right.0));
            Value::Object(
                items
                    .into_iter()
                    .map(|(key, value)| (key, canonicalize(value)))
                    .collect(),
            )
        }
        other => other,
    }
}

fn redacted_request(request: &ExecutionRequest) -> Result<ExecutionRequest, ExecutionControlError> {
    let mut value = serde_json::to_value(request).map_err(|error| {
        ExecutionControlError::Invalid(format!("execution request cannot be serialized: {error}"))
    })?;
    let Some(payload) = value.as_object_mut() else {
        return Err(ExecutionControlError::Invalid(
            "execution request must serialize as an object".to_string(),
        ));
    };
    if let Some(args) = payload.get("args") {
        payload.insert("args".to_string(), redact_value(args));
    }
    if let Some(metadata) = payload.get("metadata") {
        payload.insert("metadata".to_string(), redact_value(metadata));
    }
    if let Some(command) = request.command.first() {
        payload.insert(
            "command".to_string(),
            Value::Array(vec![
                Value::String(command.clone()),
                Value::String("<arguments redacted>".to_string()),
            ]),
        );
    }
    serde_json::from_value(value).map_err(|error| {
        ExecutionControlError::Invalid(format!("redacted execution request is invalid: {error}"))
    })
}

fn redacted_result(mut result: ExecutionResult) -> ExecutionResult {
    result.stdout_summary = redact_text(&result.stdout_summary);
    result.stderr_summary = redact_text(&result.stderr_summary);
    result.error = result.error.as_deref().map(redact_text);
    for artifact in &mut result.artifacts {
        artifact.summary = redact_text(&artifact.summary);
        artifact.path = redact_text(&artifact.path);
    }
    result
}

fn terminal_result(
    request: &ExecutionRequest,
    status: ExecutionStatus,
    error: Option<String>,
    started_at: models::Timestamp,
    completed_at: models::Timestamp,
) -> ExecutionResult {
    ExecutionResult {
        id: models::ExecutionResultId::new(models::new_id("execres")),
        request_id: request.id.clone(),
        project_id: request.project_id.clone(),
        run_id: request.run_id.clone(),
        task_id: request.task_id.clone(),
        session_id: request.session_id.clone(),
        tool_name: request.tool_name.clone(),
        backend_type: request.backend_type,
        status,
        stdout_summary: String::new(),
        stderr_summary: error.clone().unwrap_or_default(),
        exit_code: None,
        error,
        artifacts: Vec::new(),
        started_at,
        completed_at: Some(completed_at),
    }
}

fn should_retry(job: &ExecutionJob) -> bool {
    job.safe_to_retry
        && !job.cancel_requested
        && job.attempts < job.max_attempts
        && matches!(
            job.status,
            ExecutionStatus::Failed | ExecutionStatus::Timeout | ExecutionStatus::HardTimeout
        )
}

fn append_retry_history(job: &mut ExecutionJob) {
    let mut history = job
        .metadata
        .get("retry_history")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    history.push(json!({
        "attempt": job.attempts,
        "status": job.status.as_str(),
        "error": job.error.as_deref().map(redact_text),
        "completed_at": job.completed_at.map(|value| value.to_wire_string()),
    }));
    if history.len() > 9 {
        history.drain(..history.len() - 9);
    }
    job.metadata
        .insert("retry_history".to_string(), Value::Array(history));
}

const fn tool_status(status: ExecutionStatus) -> ToolStatus {
    match status {
        ExecutionStatus::Succeeded => ToolStatus::Ok,
        ExecutionStatus::Denied => ToolStatus::Denied,
        ExecutionStatus::Timeout | ExecutionStatus::HardTimeout => ToolStatus::Timeout,
        ExecutionStatus::Queued
        | ExecutionStatus::Pending
        | ExecutionStatus::Running
        | ExecutionStatus::Failed
        | ExecutionStatus::Cancelled
        | ExecutionStatus::Orphaned => ToolStatus::Error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use models::{ExecutionId, ExecutionResultId};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use storage::SqliteRepository;

    struct SuccessfulBackend;

    #[async_trait]
    impl ExecutionBackend for SuccessfulBackend {
        fn backend_type(&self) -> ExecutionBackendType {
            ExecutionBackendType::Local
        }

        async fn execute(
            &self,
            request: &ExecutionRequest,
        ) -> Result<ExecutionResult, ExecutionBackendError> {
            let now = utcnow();
            Ok(ExecutionResult {
                id: ExecutionResultId::new("execres_test".to_string()),
                request_id: request.id.clone(),
                project_id: request.project_id.clone(),
                run_id: request.run_id.clone(),
                task_id: request.task_id.clone(),
                session_id: request.session_id.clone(),
                tool_name: request.tool_name.clone(),
                backend_type: ExecutionBackendType::Local,
                status: ExecutionStatus::Succeeded,
                stdout_summary: "done".to_string(),
                stderr_summary: String::new(),
                exit_code: Some(0),
                error: None,
                artifacts: Vec::new(),
                started_at: now,
                completed_at: Some(now),
            })
        }

        async fn cancel(&self, _request_id: &str) -> Result<bool, ExecutionBackendError> {
            Ok(false)
        }
    }

    struct RetryBackend(AtomicUsize);

    #[async_trait]
    impl ExecutionBackend for RetryBackend {
        fn backend_type(&self) -> ExecutionBackendType {
            ExecutionBackendType::Local
        }

        async fn execute(
            &self,
            request: &ExecutionRequest,
        ) -> Result<ExecutionResult, ExecutionBackendError> {
            if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
                return Err(ExecutionBackendError("transient secret=abc".to_string()));
            }
            SuccessfulBackend.execute(request).await
        }

        async fn cancel(&self, _request_id: &str) -> Result<bool, ExecutionBackendError> {
            Ok(false)
        }
    }

    struct NeverFinishesBackend;

    #[async_trait]
    impl ExecutionBackend for NeverFinishesBackend {
        fn backend_type(&self) -> ExecutionBackendType {
            ExecutionBackendType::Local
        }

        async fn execute(
            &self,
            _request: &ExecutionRequest,
        ) -> Result<ExecutionResult, ExecutionBackendError> {
            std::future::pending().await
        }

        async fn cancel(&self, _request_id: &str) -> Result<bool, ExecutionBackendError> {
            Ok(true)
        }
    }

    struct NonTerminalBackend;

    #[async_trait]
    impl ExecutionBackend for NonTerminalBackend {
        fn backend_type(&self) -> ExecutionBackendType {
            ExecutionBackendType::Local
        }

        async fn execute(
            &self,
            request: &ExecutionRequest,
        ) -> Result<ExecutionResult, ExecutionBackendError> {
            let mut result = SuccessfulBackend.execute(request).await?;
            result.status = ExecutionStatus::Running;
            Ok(result)
        }

        async fn cancel(&self, _request_id: &str) -> Result<bool, ExecutionBackendError> {
            Ok(false)
        }
    }

    fn repository() -> Arc<dyn Repository> {
        Arc::new(
            SqliteRepository::open(":memory:")
                .unwrap_or_else(|error| panic!("内存仓储应可创建: {error}")),
        )
    }

    #[tokio::test]
    async fn safe_local_backend_never_claims_placeholder_success() {
        let control = Arc::new(
            ExecutionControlPlane::safe_local(repository())
                .unwrap_or_else(|error| panic!("固定后端应可注册: {error}")),
        );
        let queued = control
            .submit(
                ExecutionRequest::new("scanner".to_string()),
                SubmitExecutionOptions {
                    max_attempts: 1,
                    ..SubmitExecutionOptions::default()
                },
            )
            .await
            .unwrap_or_else(|error| panic!("提交应成功: {error}"));
        let outcome = control
            .wait(queued.id.as_str(), Duration::from_secs(1))
            .await
            .unwrap_or_else(|error| panic!("等待应成功: {error}"));
        assert_eq!(outcome.job.status, ExecutionStatus::Denied);
        assert!(outcome.job.tool_invocation_id.is_some());
    }

    #[tokio::test]
    async fn registered_backend_can_complete_and_is_audited() {
        let control = Arc::new(
            ExecutionControlPlane::new(repository(), [Arc::new(SuccessfulBackend) as Arc<_>])
                .unwrap_or_else(|error| panic!("后端应可注册: {error}")),
        );
        let queued = control
            .submit(
                ExecutionRequest::new("scanner".to_string()),
                SubmitExecutionOptions {
                    max_attempts: 1,
                    ..SubmitExecutionOptions::default()
                },
            )
            .await
            .unwrap_or_else(|error| panic!("提交应成功: {error}"));
        let outcome = control
            .wait(queued.id.as_str(), Duration::from_secs(1))
            .await
            .unwrap_or_else(|error| panic!("等待应成功: {error}"));
        assert_eq!(outcome.job.status, ExecutionStatus::Succeeded);
        assert_eq!(outcome.job.attempts, 1);
        assert!(outcome.job.tool_invocation_id.is_some());
    }

    #[tokio::test]
    async fn retry_history_is_durable_and_redacted() {
        let control = Arc::new(
            ExecutionControlPlane::new(
                repository(),
                [Arc::new(RetryBackend(AtomicUsize::new(0))) as Arc<_>],
            )
            .unwrap_or_else(|error| panic!("后端应可注册: {error}")),
        );
        let queued = control
            .submit(
                ExecutionRequest::new("scanner".to_string()),
                SubmitExecutionOptions {
                    safe_to_retry: true,
                    max_attempts: 2,
                    idempotency_key: None,
                },
            )
            .await
            .unwrap_or_else(|error| panic!("提交应成功: {error}"));
        let outcome = control
            .wait(queued.id.as_str(), Duration::from_secs(2))
            .await
            .unwrap_or_else(|error| panic!("等待应成功: {error}"));
        assert_eq!(outcome.job.status, ExecutionStatus::Succeeded);
        assert_eq!(outcome.job.attempts, 2);
        let history = outcome.job.metadata["retry_history"]
            .as_array()
            .unwrap_or_else(|| panic!("应存在重试历史"));
        assert_eq!(history.len(), 1);
        assert!(!history[0].to_string().contains("abc"));
    }

    #[tokio::test]
    async fn bounded_wait_does_not_cancel_and_explicit_cancel_is_terminal() {
        let control = Arc::new(
            ExecutionControlPlane::new(repository(), [Arc::new(NeverFinishesBackend) as Arc<_>])
                .unwrap_or_else(|error| panic!("后端应可注册: {error}")),
        );
        let queued = control
            .submit(
                ExecutionRequest::new("scanner".to_string()),
                SubmitExecutionOptions::default(),
            )
            .await
            .unwrap_or_else(|error| panic!("提交应成功: {error}"));
        let timed_out = control
            .wait(queued.id.as_str(), Duration::ZERO)
            .await
            .unwrap_or_else(|error| panic!("有限等待应成功: {error}"));
        assert!(timed_out.timed_out);
        assert!(!timed_out.job.status.is_terminal());

        let cancelled = control
            .cancel(queued.id.as_str(), "user token=secret")
            .await
            .unwrap_or_else(|error| panic!("取消应成功: {error}"));
        assert_eq!(cancelled.status, ExecutionStatus::Cancelled);
        assert_eq!(
            cancelled.cancel_note.as_deref(),
            Some("user token=********")
        );
        assert!(cancelled.tool_invocation_id.is_some());
    }

    #[tokio::test]
    async fn idempotency_key_reuses_only_the_same_request() {
        let control = Arc::new(
            ExecutionControlPlane::new(repository(), [Arc::new(SuccessfulBackend) as Arc<_>])
                .unwrap_or_else(|error| panic!("后端应可注册: {error}")),
        );
        let request = ExecutionRequest::new("scanner".to_string());
        let options = SubmitExecutionOptions {
            idempotency_key: Some("same-key".to_string()),
            ..SubmitExecutionOptions::default()
        };
        let first = control
            .submit(request.clone(), options.clone())
            .await
            .unwrap_or_else(|error| panic!("首次提交应成功: {error}"));
        let repeated = control
            .submit(request.clone(), options.clone())
            .await
            .unwrap_or_else(|error| panic!("重复提交应复用: {error}"));
        assert_eq!(repeated.id, first.id);

        let mut changed = request;
        changed
            .args
            .insert("target".to_string(), Value::String("other".to_string()));
        let error = control
            .submit(changed, options)
            .await
            .expect_err("同一幂等键不得接受不同请求");
        assert!(matches!(error, ExecutionControlError::Conflict(_)));
    }

    #[tokio::test]
    async fn backend_non_terminal_result_is_persisted_as_failure() {
        let control = Arc::new(
            ExecutionControlPlane::new(repository(), [Arc::new(NonTerminalBackend) as Arc<_>])
                .unwrap_or_else(|error| panic!("后端应可注册: {error}")),
        );
        let queued = control
            .submit(
                ExecutionRequest::new("scanner".to_string()),
                SubmitExecutionOptions::default(),
            )
            .await
            .unwrap_or_else(|error| panic!("提交应成功: {error}"));
        let outcome = control
            .wait(queued.id.as_str(), Duration::from_secs(1))
            .await
            .unwrap_or_else(|error| panic!("等待应成功: {error}"));
        assert_eq!(outcome.job.status, ExecutionStatus::Failed);
        assert!(
            outcome
                .job
                .error
                .as_deref()
                .is_some_and(|error| error.contains("non-terminal"))
        );
    }

    #[test]
    fn recover_marks_non_terminal_jobs_orphaned() {
        let repository = repository();
        let request = ExecutionRequest::new("scanner".to_string());
        let mut job = ExecutionJob::queued(request, "digest".to_string());
        job.id = ExecutionId::new("exec_orphan".to_string());
        job.request.id = job.id.clone();
        job.status = ExecutionStatus::Running;
        repository
            .create_execution_job(&job)
            .unwrap_or_else(|error| panic!("fixture 应写入: {error}"));
        let control = ExecutionControlPlane::safe_local(Arc::clone(&repository))
            .unwrap_or_else(|error| panic!("固定后端应可注册: {error}"));
        let recovered = control
            .recover_orphaned()
            .unwrap_or_else(|error| panic!("恢复应成功: {error}"));
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].status, ExecutionStatus::Orphaned);
        assert!(recovered[0].tool_invocation_id.is_some());
    }
}
