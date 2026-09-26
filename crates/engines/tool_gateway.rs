//! 外部工具安全网关（M6）。
//!
//! 工具只能以 argv 数组启动，永远不经过 shell；超时、非零退出、拒绝
//! 和输出工件都会生成同一条 `ToolInvocation`，因此“工具失败但任务显示
//! 成功”的状态丢失不会再发生。
//! 【统一工具系统核心·唯一执行通道】无 shell argv 子进程执行 + 审计。
//! `tool_execute`（lynceus-mcp）必须委托本网关——绝不允许第二执行路径。

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Instant;

use evidence::SealedArtifact;
use models::ids::{MissionId, ProjectId, RunId, TaskId};
use models::lifecycle::ToolStatus;
use models::tool_invocation::ToolInvocation;
use tokio::process::Command;
use tokio::time::{Duration, timeout};

/// 工具调用请求；`program` 与 `args` 分开，避免 shell 注入。
#[derive(Debug, Clone)]
pub struct ToolRequest {
    /// 工具逻辑名。
    pub tool_name: String,
    /// 可执行文件名/绝对路径。
    pub program: String,
    /// argv 参数（不含 program）。
    pub args: Vec<String>,
    /// 可选工作目录。
    pub current_dir: Option<PathBuf>,
    /// 超时秒数；缺省 120 秒。
    pub timeout_seconds: u64,
    /// 输出工件目录；为空则不落盘。
    pub artifact_dir: Option<PathBuf>,
    /// 追加到子进程环境的键值（叠加在继承环境之上；同名键覆盖继承值）。
    /// 目录配置（catalog settings）的 env 注入即经由此字段进入子进程。
    pub env: BTreeMap<String, String>,
    /// 发起本次调用的 Worker 身份（Tool Broker 从 `ExecutionScope.worker_id`
    /// 带下来；与 `module_id` 分工不同，见 `ToolInvocation::worker_id`）。
    pub worker_id: Option<String>,
}

impl ToolRequest {
    /// 构造一个默认 120 秒、无工件目录、无额外环境的请求。
    #[must_use]
    pub fn new(tool_name: impl Into<String>, program: impl Into<String>) -> Self {
        Self {
            tool_name: tool_name.into(),
            program: program.into(),
            args: Vec::new(),
            current_dir: None,
            timeout_seconds: 120,
            artifact_dir: None,
            env: BTreeMap::new(),
            worker_id: None,
        }
    }
}

/// 工具网关返回值。
#[derive(Debug, Clone)]
pub struct ToolResult {
    /// 审计调用记录。
    pub invocation: ToolInvocation,
    /// 原始 stdout 字节。
    pub stdout: Vec<u8>,
    /// 原始 stderr 字节。
    pub stderr: Vec<u8>,
}

/// 无 shell 的跨平台工具执行网关。
#[derive(Debug, Default, Clone, Copy)]
pub struct ToolGateway;

impl ToolGateway {
    /// 执行一次外部工具并生成完整审计记录。
    ///
    /// # Errors
    /// 进程启动失败、超时或工件落盘失败时返回对应 I/O 错误；工具本身
    /// 的非零退出仍以 `ToolResult.invocation.status = Error` 返回。
    pub async fn execute(
        &self,
        project_id: Option<ProjectId>,
        mission_id: Option<MissionId>,
        run_id: Option<RunId>,
        task_id: Option<TaskId>,
        request: ToolRequest,
    ) -> Result<ToolResult, std::io::Error> {
        let started = Instant::now();
        let mut invocation = ToolInvocation::new(request.tool_name.clone(), request.args.join(" "));
        invocation.project_id = project_id;
        invocation.mission_id = mission_id;
        invocation.run_id = run_id;
        invocation.task_id = task_id;
        invocation.worker_id = request.worker_id.clone();
        // 命令禁则（Lynceus 自有执行面的真拦截）：命中即拒绝 spawn，落一条
        // Error 审计记录——调用方看到的是"被平台管控拦截"，与参考实现的
        // hook block 语义一致（不伪造成功、不留半执行的进程）。
        if let Some(prefix) = crate::worker::CommandPolicy::seed().violation(&format!(
            "{} {}",
            request.program,
            request.args.join(" ")
        )) {
            invocation.status = ToolStatus::Error;
            invocation.error = Some(format!(
                "blocked by command policy (matched prefix: {prefix})"
            ));
            invocation.duration_ms = Some(elapsed_ms(started));
            invocation.finished_at = Some(models::utcnow());
            return Ok(ToolResult {
                invocation,
                stdout: Vec::new(),
                stderr: Vec::new(),
            });
        }
        let mut command = Command::new(&request.program);
        command.args(&request.args);
        command.envs(&request.env);
        // 工作目录必须显式给定，绝不让子进程继承 api 进程的 cwd：
        // subfinder / httpx / naabu 这类工具首次运行会在 cwd 里落默认
        // `config.yaml` / `provider-config.yaml`，继承仓库根就会把配置
        // 文件拉进项目根目录。优先级：显式 current_dir > artifact_dir
        // （产出本就写那里）> LYNCEUS_WORKSPACE_DIR > 系统临时目录。
        let working_dir = request
            .current_dir
            .clone()
            .or_else(|| request.artifact_dir.clone())
            .or_else(|| {
                std::env::var_os("LYNCEUS_WORKSPACE_DIR")
                    .filter(|value| !value.is_empty())
                    .map(PathBuf::from)
            })
            .unwrap_or_else(std::env::temp_dir);
        if std::fs::create_dir_all(&working_dir).is_ok() {
            command.current_dir(&working_dir);
        }
        let output = if let Ok(result) = timeout(
            Duration::from_secs(request.timeout_seconds.max(1)),
            command.output(),
        )
        .await
        {
            result?
        } else {
            invocation.status = ToolStatus::Timeout;
            invocation.error = Some(format!(
                "tool timed out after {} second(s)",
                request.timeout_seconds.max(1)
            ));
            invocation.duration_ms = Some(elapsed_ms(started));
            invocation.finished_at = Some(models::utcnow());
            return Ok(ToolResult {
                invocation,
                stdout: Vec::new(),
                stderr: Vec::new(),
            });
        };
        invocation.exit_code = output.status.code().map(i64::from);
        invocation.duration_ms = Some(elapsed_ms(started));
        invocation.finished_at = Some(models::utcnow());
        invocation.status = if output.status.success() {
            ToolStatus::Ok
        } else {
            ToolStatus::Error
        };
        invocation.output_summary = summarize(&output.stdout, &output.stderr);
        if !output.status.success() {
            invocation.error = Some(String::from_utf8_lossy(&output.stderr).trim().to_string());
        }

        if let Some(directory) = request.artifact_dir.as_ref() {
            std::fs::create_dir_all(directory)?;
            let filename = format!(
                "{}-{}.out",
                safe_component(&request.tool_name),
                invocation.id.as_str()
            );
            let path = directory.join(filename);
            let artifact = SealedArtifact::seal(output.stdout.clone());
            artifact
                .persist(&path)
                .map_err(|error| std::io::Error::other(error.to_string()))?;
            invocation
                .artifact_paths
                .push(path.to_string_lossy().into_owned());
            invocation.metadata.insert(
                "sha256".to_string(),
                serde_json::Value::String(artifact.sha256().as_hex().to_string()),
            );
        }
        Ok(ToolResult {
            invocation,
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }
}

fn elapsed_ms(started: Instant) -> i64 {
    i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX)
}

fn safe_component(value: &str) -> String {
    let mut out: String = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
                ch
            } else {
                '_'
            }
        })
        .collect();
    if out.is_empty() {
        out.push_str("tool");
    }
    out
}

fn summarize(stdout: &[u8], stderr: &[u8]) -> String {
    let source = if stdout.is_empty() { stderr } else { stdout };
    let text = String::from_utf8_lossy(source);
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    compact.chars().take(2000).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn gateway_uses_argv_and_records_success() {
        let gateway = ToolGateway;
        let mut request = if cfg!(windows) {
            ToolRequest::new("echo", "cmd")
        } else {
            ToolRequest::new("echo", "printf")
        };
        if cfg!(windows) {
            request.args = vec!["/C".to_string(), "echo gateway".to_string()];
        } else {
            request.args = vec!["gateway".to_string()];
        }
        let result = gateway
            .execute(None, None, None, None, request)
            .await
            .expect("portable echo should execute");
        assert_eq!(result.invocation.status, ToolStatus::Ok);
    }

    /// env 注入直测：请求携带的键值必须出现在子进程环境中
    /// （catalog settings.env 的注入即经由 ToolRequest.env 生效）。
    #[tokio::test]
    async fn gateway_injects_request_env_into_subprocess() {
        let mut request = if cfg!(windows) {
            ToolRequest::new("env-echo", "cmd")
        } else {
            ToolRequest::new("env-echo", "sh")
        };
        if cfg!(windows) {
            request.args = vec!["/C".to_string(), "echo %LYNCEUS_TEST_TOKEN%".to_string()];
        } else {
            request.args = vec!["-c".to_string(), "echo $LYNCEUS_TEST_TOKEN".to_string()];
        }
        request
            .env
            .insert("LYNCEUS_TEST_TOKEN".to_string(), "tok_abc123".to_string());
        let result = ToolGateway
            .execute(None, None, None, None, request)
            .await
            .expect("env echo runner should execute");
        assert_eq!(result.invocation.status, ToolStatus::Ok);
        let stdout = String::from_utf8_lossy(&result.stdout);
        assert!(
            stdout.contains("tok_abc123"),
            "subprocess environment missing injected token: {stdout}"
        );
    }

    #[tokio::test]
    async fn gateway_blocks_denied_command_prefix_without_spawning() {
        let gateway = ToolGateway;
        // 命中默认禁则前缀：不得 spawn 任何进程，落 Error 审计记录。
        let mut request = ToolRequest::new("blocked", "rm");
        request.args = vec!["-rf".to_string(), "/".to_string()];
        let result = gateway
            .execute(None, None, None, None, request)
            .await
            .expect("denylist hit is an audited Error, not an io failure");
        assert_eq!(result.invocation.status, ToolStatus::Error);
        assert!(
            result
                .invocation
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("blocked by command policy"),
            "error must name the policy: {:?}",
            result.invocation.error
        );
        assert!(result.stdout.is_empty(), "被禁命令不得有输出");
    }

    #[tokio::test]
    async fn gateway_allows_normal_audit_command() {
        let gateway = ToolGateway;
        let mut request = if cfg!(windows) {
            ToolRequest::new("echo", "cmd")
        } else {
            ToolRequest::new("echo", "printf")
        };
        if cfg!(windows) {
            request.args = vec!["/C".to_string(), "echo formatted".to_string()];
        } else {
            request.args = vec!["formatted".to_string()];
        }
        let result = gateway
            .execute(None, None, None, None, request)
            .await
            .expect("normal command executes");
        assert_eq!(result.invocation.status, ToolStatus::Ok);
    }
}
