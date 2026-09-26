//! 进程驱动：外部 Worker Runtime 的统一子进程边界。
//!
//! 职责只有三件事：发现可执行文件（PATH / 显式 env 覆盖）、按超时运行
//! 并有界捕获输出、把取消语义传播到子进程（`kill_on_drop`）。**不做**
//! 任何工具拼接、shell 解释或 Harness 逻辑——命令行完全由各 adapter
//! 构造。

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

/// 一次受控子进程执行的完整规格（由 adapter 构造，driver 不做解释）。
#[derive(Debug, Clone)]
pub struct ProcessSpec {
    /// 可执行文件（绝对路径或 PATH 可解析名）。
    pub program: PathBuf,
    /// argv（不含 program）。
    pub args: Vec<String>,
    /// 追加到子进程的环境变量（叠加继承环境；同名覆盖）。
    ///
    /// **红线**：只允许包含 Connection 注入项（base URL / key / model）
    /// 与 driver 行为开关；绝不包含 Lynceus 进程的无关环境。
    pub env: BTreeMap<String, String>,
    /// 工作目录（可选）。
    pub working_dir: Option<PathBuf>,
    /// 整体超时；超时强杀并标记 [`ProcessOutcome::timed_out`]。
    pub timeout: Duration,
    /// 写入 stdin 的数据（可选；None 则立即关闭 stdin）。
    pub stdin_data: Option<String>,
}

/// 有界进程输出（`stdout` / `stderr` 已按字节上限截断并做 UTF-8 有损转换）。
#[derive(Debug, Clone)]
pub struct ProcessOutcome {
    /// 退出码（被信号/强杀终止时为 None）。
    pub exit_code: Option<i64>,
    /// 是否因超时被终止。
    pub timed_out: bool,
    /// 是否因取消信号被终止。
    pub cancelled: bool,
    /// stdout（截断后）。
    pub stdout: String,
    /// stderr（截断后）。
    pub stderr: String,
    /// 任一输出流因超出上限被截断。
    pub truncated: bool,
}

/// 单流输出捕获上限（worker transcript 入库侧另有事件级有界化）。
const MAX_STREAM_BYTES: usize = 8 * 1024 * 1024;

impl ProcessOutcome {
    /// 退出是否成功。
    #[must_use]
    pub fn succeeded(&self) -> bool {
        !self.timed_out && !self.cancelled && self.exit_code == Some(0)
    }

    /// 密封 transcript 的原始字节（stdout 在前，stderr 以标记分隔）。
    #[must_use]
    pub fn transcript_bytes(&self) -> Vec<u8> {
        let mut bytes = self.stdout.clone().into_bytes();
        if !self.stderr.is_empty() {
            bytes.extend_from_slice(b"\n--- stderr ---\n");
            bytes.extend_from_slice(self.stderr.clone().into_bytes().as_slice());
        }
        bytes
    }
}

/// 运行一次受控子进程。
///
/// # Errors
/// 进程启动失败（可执行文件缺失 / 权限）或工作目录不存在。
pub async fn run_process(spec: &ProcessSpec) -> std::io::Result<ProcessOutcome> {
    run_process_inner(spec, None, None).await
}

/// 运行一次受控子进程，支持外部取消信号（cancel 通道置 true 即终止）。
///
/// # Errors
/// 进程启动失败（可执行文件缺失 / 权限）或工作目录不存在。
pub async fn run_process_with_cancel(
    spec: &ProcessSpec,
    cancel: tokio::sync::watch::Receiver<bool>,
) -> std::io::Result<ProcessOutcome> {
    run_process_inner(spec, Some(cancel), None).await
}

/// 流式运行：stdout **逐行**回调（JSONL 事件解析用），其余语义与
/// [`run_process_with_cancel`] 完全一致（有界捕获、超时、取消）。
/// 回调在捕获任务内同步调用——必须轻量，不得阻塞管道排空。
///
/// # Errors
/// 进程启动失败（可执行文件缺失 / 权限）或工作目录不存在。
pub async fn run_process_streaming<F>(
    spec: &ProcessSpec,
    cancel: tokio::sync::watch::Receiver<bool>,
    on_line: F,
) -> std::io::Result<ProcessOutcome>
where
    F: FnMut(&str) + Send + 'static,
{
    run_process_inner(spec, Some(cancel), Some(Box::new(on_line))).await
}

/// stdout 逐行回调（JSONL 解析钩子，闭包须轻量）。
pub type StreamHandler = Box<dyn FnMut(&str) + Send>;

async fn run_process_inner(
    spec: &ProcessSpec,
    mut cancel: Option<tokio::sync::watch::Receiver<bool>>,
    on_line: Option<StreamHandler>,
) -> std::io::Result<ProcessOutcome> {
    use tokio::io::AsyncWriteExt;

    let mut command = build_command(spec)?;
    command.kill_on_drop(true);
    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::piped());
    command.stdin(std::process::Stdio::piped());
    // 认证红线：Agent CLI 子进程不得继承 Lynceus 进程的 home、CLI
    // config、OAuth session 或 provider 环境。先清空，再只回填运行进程
    // 所需的最小公共环境，最后叠加 adapter 显式注入的 Connection/MCP
    // 配置；因此 CLI 不可能通过用户已有登录态完成认证。
    command.env_clear();
    for (key, value) in std::env::vars_os() {
        let key_text = key.to_string_lossy();
        if is_inherited_safe(&key_text) {
            command.env(key, value);
        }
    }
    // Worker 的上游全部是本机回环（LiteLLM 网关 / lynceus-mcp）。部分 CLI
    // 的 HTTP 栈会读 Windows 系统代理（WinINET registry），把 127.0.0.1
    // 也转发给代理并被 502 拒绝（实测 Clash 7890）；显式放行回环。
    // adapter 显式注入的同名变量可覆盖此默认值。
    command.env("NO_PROXY", "127.0.0.1,localhost");
    command.env("no_proxy", "127.0.0.1,localhost");
    // PATHEXT 决定 `cmd` shim 能否按名解析可执行文件。npm 全局包的 `.cmd`
    // 一律是 `node <pkg>/bin/x.js`（claude 例外：它直接链到真 `.exe`），
    // 所以 PATHEXT 缺 `.EXE` 时，`cmd /c codex.cmd` 会以
    // `'node' is not recognized` 失败——CLI 压根没跑起来。
    //
    // 沙箱 / CI 常把 PATHEXT 砍成 `.CPL` 之类的最小集（本机实测即如此），
    // 父进程继承下来就会静默废掉 codex / pi / dsh 三个 runtime。盲目继承
    // 等于把宿主的洁癖当成 Worker 的运行环境；缺 `.EXE` 时回填 Windows
    // 标准值。adapter 显式注入的同名变量仍可覆盖此默认值。
    let inherited_pathext_has_exe = std::env::var_os("PATHEXT")
        .map(|value| {
            value
                .to_string_lossy()
                .to_ascii_uppercase()
                .split(';')
                .any(|extension| extension == ".EXE")
        })
        .unwrap_or(false);
    if !inherited_pathext_has_exe {
        command.env(
            "PATHEXT",
            ".COM;.EXE;.BAT;.CMD;.VBS;.VBE;.JS;.JSE;.WSF;.WSH;.MSC",
        );
    }
    #[cfg(windows)]
    if let Some(path) = path_with_usable_bash() {
        command.env("PATH", path);
    }
    command.envs(&spec.env);
    if let Some(directory) = spec.working_dir.as_ref() {
        command.current_dir(directory);
    }
    let mut child = command.spawn()?;

    // Windows：注册进程树强杀 guard。必须在 `child` **之后**声明——作用域退出
    // （取消 / 超时 / 暂停导致上层 future 被 drop）时按"后声明先析构"顺序，先跑
    // 这里的整树强杀（此刻 `cmd` shim 仍活着、进程树完整）再让 `child` 的
    // kill_on_drop 收尾，无竞争地收掉 `cmd` + 它 spawn 的 node，杜绝 node 被
    // reparent 成孤儿继续跑。仅 Windows 需要（Unix 无 `.cmd` shim 间接层）。
    #[cfg(target_os = "windows")]
    let _worker_tree = KillTreeOnDrop::new(child.id());

    let mut stdout_pipe = child.stdout.take();
    let mut stderr_pipe = child.stderr.take();
    let mut stdin_pipe = child.stdin.take();

    // stdin 数据写入后立即关闭，避免子进程等待 EOF。
    if let (Some(stdin), Some(data)) = (stdin_pipe.as_mut(), spec.stdin_data.as_ref()) {
        let _ = stdin.write_all(data.as_bytes()).await;
    }
    drop(stdin_pipe);

    let stdout_task = tokio::spawn(async move {
        let mut on_line = on_line;
        match on_line.as_mut() {
            Some(handler) => read_lines(&mut stdout_pipe, MAX_STREAM_BYTES, handler.as_mut()).await,
            None => read_bounded(&mut stdout_pipe, MAX_STREAM_BYTES).await,
        }
    });
    let stderr_task =
        tokio::spawn(async move { read_bounded(&mut stderr_pipe, MAX_STREAM_BYTES).await });

    // 超时与取消都收敛为显式强杀（kill_on_drop 兜底 future 中断场景）。
    let cancel_requested = async {
        if let Some(receiver) = cancel.as_mut() {
            // 初始值 false；等到变为 true 才返回。
            while !*receiver.borrow_and_update() {
                if receiver.changed().await.is_err() {
                    // 发送端已丢弃（会话正常收尾）→ 不再等待取消。
                    std::future::pending::<()>().await;
                }
            }
        } else {
            std::future::pending::<()>().await;
        }
    };
    let status = tokio::select! {
        result = child.wait() => result?,
        _ = tokio::time::sleep(spec.timeout) => {
            kill_child_tree(&mut child).await;
            let _ = child.wait().await;
            let (stdout, stdout_truncated) = join_capture(stdout_task).await;
            let (stderr, stderr_truncated) = join_capture(stderr_task).await;
            return Ok(ProcessOutcome {
                exit_code: None,
                timed_out: true,
                cancelled: false,
                stdout,
                stderr,
                truncated: stdout_truncated || stderr_truncated,
            });
        }
        _ = cancel_requested => {
            kill_child_tree(&mut child).await;
            let _ = child.wait().await;
            let (stdout, stdout_truncated) = join_capture(stdout_task).await;
            let (stderr, stderr_truncated) = join_capture(stderr_task).await;
            return Ok(ProcessOutcome {
                exit_code: None,
                timed_out: false,
                cancelled: true,
                stdout,
                stderr,
                truncated: stdout_truncated || stderr_truncated,
            });
        }
    };

    let stdout = join_capture(stdout_task).await;
    let stderr = join_capture(stderr_task).await;
    let truncated = stdout.1 || stderr.1;
    Ok(ProcessOutcome {
        exit_code: status.code().map(i64::from),
        timed_out: false,
        cancelled: false,
        stdout: stdout.0,
        stderr: stderr.0,
        truncated,
    })
}

/// 整树强杀 worker 进程树（Windows）：`taskkill /F /T /PID <pid>`。
///
/// 为什么需要：npm 的 `.cmd` shim 是 `cmd /c xxx.cmd`，真正干活的是它 spawn 的
/// node 子进程。`kill_on_drop` 只 TerminateProcess 直接子进程（cmd），node 会被
/// reparent 成孤儿继续跑——实测 mission 暂停、stop-local 强杀 api 之后 worker
/// 仍在跑就是这么来的。`taskkill /T` 沿父子关系一次性收掉整棵树（cmd + node），
/// 且在 cmd 死前完成枚举，杜绝 reparent 竞争。
///
/// 进程已退出时 taskkill 报 "not found"，无害忽略。同步等待以确保枚举完成后再
/// 让 `kill_on_drop` 收尾（否则 cmd 先死会破坏树、漏杀 node）。
#[cfg(target_os = "windows")]
fn kill_process_tree(pid: u32) {
    // /F=强制 /T=连同子进程树 /PID=目标；输出全部丢弃，避免泄漏到父进程 stdio。
    let _ = std::process::Command::new("taskkill")
        .args(["/F", "/T", "/PID", &pid.to_string()])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

/// Drop guard：析构时整树强杀子进程。
///
/// 关键：本 guard 必须在 `child` **之后**声明，作用域退出时按"后声明先析构"顺序，
/// 先跑 [`kill_process_tree`]（此刻 cmd 仍活着、树仍完整）再让 `child` 的
/// `kill_on_drop` 收尾——从而无竞争地覆盖暂停（上层 future 被 drop）、取消、
/// 超时等所有路径。仅 Windows 需要（Unix 无 `.cmd` shim 间接层，`kill_on_drop`
/// 直接杀掉真实子进程即可）。
#[cfg(target_os = "windows")]
struct KillTreeOnDrop(Option<u32>);

#[cfg(target_os = "windows")]
impl KillTreeOnDrop {
    fn new(pid: Option<u32>) -> Self {
        Self(pid)
    }
}

#[cfg(target_os = "windows")]
impl Drop for KillTreeOnDrop {
    fn drop(&mut self) {
        if let Some(pid) = self.0.take() {
            kill_process_tree(pid);
        }
    }
}

/// 取消 / 超时分支用的整树强杀（跨平台）。
///
/// Windows 走 `taskkill /T`（连同 node 一起收，避免孤儿）；丢到 blocking 池执行
/// 以免阻塞 async worker，并 await 以保证 node 在 cmd 死前被收掉。Unix 无 `.cmd`
/// shim 间接层，直接 `start_kill` 杀掉真实子进程即可。
async fn kill_child_tree(child: &mut tokio::process::Child) {
    #[cfg(target_os = "windows")]
    {
        if let Some(pid) = child.id() {
            let _ = tokio::task::spawn_blocking(move || kill_process_tree(pid)).await;
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = child.start_kill();
    }
}

/// 在 Windows 上以 CREATE_NO_WINDOW 启动；`.cmd` / `.bat` 脚本经
/// `COMSPEC` 显式执行（现代 Rust 拒绝直接 CreateProcess 批处理脚本）。
fn build_command(spec: &ProcessSpec) -> std::io::Result<tokio::process::Command> {
    let mut command;
    #[cfg(target_os = "windows")]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let extension = spec
            .program
            .extension()
            .and_then(|value| value.to_str())
            .map(str::to_ascii_lowercase);
        if matches!(extension.as_deref(), Some("cmd" | "bat")) {
            let comspec = std::env::var("COMSPEC")
                .unwrap_or_else(|_| "C:\\Windows\\System32\\cmd.exe".to_string());
            let mut args = vec!["/d".to_string(), "/c".to_string()];
            args.push(spec.program.to_string_lossy().into_owned());
            args.extend(spec.args.iter().cloned());
            command = tokio::process::Command::new(comspec);
            command.args(&args);
        } else {
            command = tokio::process::Command::new(&spec.program);
            command.args(&spec.args);
        }
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(target_os = "windows"))]
    {
        command = tokio::process::Command::new(&spec.program);
        command.args(&spec.args);
    }
    Ok(command)
}

/// Windows：把可用的 Git bash `bin` 目录前置到 worker 的 PATH。
///
/// 宿主的 PATH 常见两种残局，都会让 pi 的 bash 工具直接罢工：
/// ① 只有 WSL 的 `bash` 占位 shim（`WindowsApps\bash.exe`，没配发行版时一跑
///    就失败）；
/// ② Git 装了但 `bin` 没进 PATH——只有 `...\Git\cmd` 在（实测即如此：git 装在
///    `D:\Program Files\Git`，PATH 里却是 C 盘的残留条目）。
/// pi 要的是 MINGW bash，两种都认不下来，报 "No bash shell found" 然后什么都
/// 执行不了。这里先从 PATH 里的 Git `cmd` 条目反推同级的 `bin`，再兜底扫常见
/// 安装位置（含非 C 盘与用户级安装），命中且不在 PATH 中才前置（幂等）。
#[cfg(windows)]
fn path_with_usable_bash() -> Option<String> {
    let current = std::env::var_os("PATH")?.to_string_lossy().to_string();
    augment_path_with_bash(&current)
}

/// [`path_with_usable_bash`] 的纯逻辑：给定 PATH 字符串，返回前置了可用
/// Git bash `bin` 的新 PATH；无需改动或已可用时返回 `None`。
#[cfg(windows)]
fn augment_path_with_bash(current: &str) -> Option<String> {
    use std::path::Path;

    let entries: Vec<&str> = current
        .split(';')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .collect();

    // PATH 里已经有真 bash（非 WSL 占位）就不动。
    let already_usable = entries.iter().any(|entry| {
        Path::new(entry).join("bash.exe").is_file()
            && !entry.to_ascii_lowercase().contains("windowsapps")
    });
    if already_usable {
        return None;
    }

    let mut candidates: Vec<PathBuf> = entries
        .iter()
        .filter_map(|entry| {
            let bin = Path::new(entry).parent()?.join("bin");
            bin.join("bash.exe").is_file().then_some(bin)
        })
        .collect();
    for root in [
        r"C:\Program Files\Git",
        r"C:\Program Files (x86)\Git",
        r"D:\Program Files\Git",
        r"E:\Program Files\Git",
    ] {
        let bin = Path::new(root).join("bin");
        if bin.join("bash.exe").is_file() {
            candidates.push(bin);
        }
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        let bin = Path::new(&local).join("Programs").join("Git").join("bin");
        if bin.join("bash.exe").is_file() {
            candidates.push(bin);
        }
    }

    let found = candidates
        .into_iter()
        .find(|bin| entries.iter().all(|entry| !Path::new(entry).eq(bin.as_path())))?;
    Some(format!("{};{current}", found.display()))
}

/// 子进程只继承平台运行必需变量；home/config/credential 变量必须由
/// adapter 指向本次 Worker 的隔离目录，不能从父进程自然流入。
fn is_inherited_safe(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    matches!(
        upper.as_str(),
        "PATH"
            | "PATHEXT"
            | "COMSPEC"
            | "SYSTEMROOT"
            | "WINDIR"
            | "TEMP"
            | "TMP"
            | "LANG"
            | "LC_ALL"
            | "LC_CTYPE"
            | "TERM"
            | "CI"
            | "NO_COLOR"
            | "TZ"
    )
}

async fn join_capture(task: tokio::task::JoinHandle<(String, bool)>) -> (String, bool) {
    task.await
        .unwrap_or_else(|error| (format!("output capture failed: {error}"), false))
}

/// 逐行读取 stdout：每行回调 `handler`，同时保持有界累积（transcript 用）。
async fn read_lines(
    pipe: &mut Option<impl tokio::io::AsyncRead + Unpin>,
    max: usize,
    handler: &mut (dyn FnMut(&str) + Send),
) -> (String, bool) {
    use tokio::io::{AsyncBufReadExt, BufReader};
    let Some(pipe) = pipe.as_mut() else {
        return (String::new(), false);
    };
    let mut reader = BufReader::new(pipe);
    let mut buffer = Vec::new();
    let mut line = String::new();
    let mut truncated = false;
    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                let raw = line.as_bytes();
                if buffer.len() + raw.len() > max {
                    let remaining = max - buffer.len();
                    buffer.extend_from_slice(&raw[..remaining.min(raw.len())]);
                    truncated = true;
                } else {
                    buffer.extend_from_slice(raw);
                }
                let trimmed = line.trim_end_matches(['\n', '\r']);
                handler(trimmed);
            }
        }
    }
    (String::from_utf8_lossy(&buffer).into_owned(), truncated)
}

/// 有界读取一个输出流（超出 `max` 字节截断并返回截断标记）。
async fn read_bounded(
    pipe: &mut Option<impl tokio::io::AsyncRead + Unpin>,
    max: usize,
) -> (String, bool) {
    use tokio::io::AsyncReadExt;
    let Some(pipe) = pipe.as_mut() else {
        return (String::new(), false);
    };
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 8192];
    let mut truncated = false;
    loop {
        match pipe.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(read) => {
                if buffer.len() + read > max {
                    let remaining = max - buffer.len();
                    buffer.extend_from_slice(&chunk[..remaining]);
                    truncated = true;
                    // 继续排空管道但不入库，直至流关闭。
                } else {
                    buffer.extend_from_slice(&chunk[..read]);
                }
            }
        }
    }
    (String::from_utf8_lossy(&buffer).into_owned(), truncated)
}

/// 按 PATH / 显式覆盖发现可执行文件（不经 shell）。
///
/// Windows 上依次尝试 `name.exe` / `name.cmd` / `name.bat`；其余平台只
/// 尝试 `name` 本身。返回绝对路径。
#[must_use]
pub fn locate_executable(name: &str, override_path: Option<&str>) -> Option<PathBuf> {
    if let Some(explicit) = override_path
        && !explicit.trim().is_empty()
    {
        let path = PathBuf::from(explicit.trim());
        if path.is_file() {
            return Some(path);
        }
        return None;
    }
    let path_variable = std::env::var_os("PATH")?;
    let candidates: Vec<String> = if cfg!(target_os = "windows") {
        vec![
            format!("{name}.exe"),
            format!("{name}.cmd"),
            format!("{name}.bat"),
            name.to_string(),
        ]
    } else {
        vec![name.to_string()]
    };
    for directory in std::env::split_paths(&path_variable) {
        for candidate in &candidates {
            let full = directory.join(candidate);
            if full.is_file() {
                return Some(full);
            }
        }
    }
    None
}

/// 显式二进制路径覆盖的环境变量名（`LYNCEUS_WORKER_<TYPE>_PATH`）。
#[must_use]
pub fn override_env_name(runtime: models::WorkerRuntimeType) -> String {
    let suffix = runtime.as_str().to_ascii_uppercase();
    format!("LYNCEUS_WORKER_{suffix}_PATH")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn runs_bounded_process_and_captures_output() {
        let spec = if cfg!(windows) {
            ProcessSpec {
                program: PathBuf::from("cmd"),
                args: vec![
                    "/d".to_string(),
                    "/c".to_string(),
                    "echo driver-ok".to_string(),
                ],
                env: BTreeMap::new(),
                working_dir: None,
                timeout: Duration::from_secs(30),
                stdin_data: None,
            }
        } else {
            ProcessSpec {
                program: PathBuf::from("printf"),
                args: vec!["driver-ok".to_string()],
                env: BTreeMap::new(),
                working_dir: None,
                timeout: Duration::from_secs(30),
                stdin_data: None,
            }
        };
        let outcome = run_process(&spec).await.expect("process must run");
        assert!(outcome.succeeded(), "{outcome:?}");
        assert!(outcome.stdout.contains("driver-ok"), "{outcome:?}");
    }

    #[tokio::test]
    async fn kills_process_on_timeout() {
        let spec = if cfg!(windows) {
            ProcessSpec {
                program: PathBuf::from("cmd"),
                args: vec![
                    "/d".to_string(),
                    "/c".to_string(),
                    "ping -n 30 127.0.0.1 > NUL".to_string(),
                ],
                env: BTreeMap::new(),
                working_dir: None,
                timeout: Duration::from_millis(400),
                stdin_data: None,
            }
        } else {
            ProcessSpec {
                program: PathBuf::from("sleep"),
                args: vec!["30".to_string()],
                env: BTreeMap::new(),
                working_dir: None,
                timeout: Duration::from_millis(400),
                stdin_data: None,
            }
        };
        let outcome = run_process(&spec).await.expect("process must run");
        assert!(outcome.timed_out);
        assert!(!outcome.succeeded());
    }

    #[test]
    fn locate_executable_missing_returns_none() {
        assert!(
            locate_executable("definitely-not-a-real-cli-xyz", None).is_none(),
            "unknown binary must not be found"
        );
    }

    #[tokio::test]
    async fn missing_binary_is_spawn_error_not_panic() {
        let spec = ProcessSpec {
            program: PathBuf::from("definitely-not-a-real-cli-xyz"),
            args: Vec::new(),
            env: BTreeMap::new(),
            working_dir: None,
            timeout: Duration::from_secs(5),
            stdin_data: None,
        };
        assert!(run_process(&spec).await.is_err());
    }

    #[test]
    fn inherited_env_allowlist_excludes_vendor_credentials() {
        // 生产用 is_inherited_safe 白名单；厂商凭证/会话变量不在名单内 → 不继承。
        for leaked in [
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_BASE_URL",
            "ANTHROPIC_MODEL",
            "ANTHROPIC_AUTH_TOKEN",
            "CLAUDE_CODE_SESSION_ID",
            "CLAUDECODE",
            "OPENAI_API_KEY",
            "DEEPSEEK_TOKEN",
            "MINIMAX_API_KEY",
            "MY_SERVICE_API_KEY",
            "GATEWAY_AUTH_TOKEN",
        ] {
            assert!(!is_inherited_safe(leaked), "{leaked} must not be inherited");
        }
    }

    #[test]
    fn inherited_environment_is_minimal_and_excludes_user_home() {
        assert!(is_inherited_safe("PATH"));
        assert!(is_inherited_safe("SystemRoot"));
        assert!(is_inherited_safe("TEMP"));
        for leaked in [
            "HOME",
            "USERPROFILE",
            "APPDATA",
            "XDG_CONFIG_HOME",
            "NPM_CONFIG_USERCONFIG",
            "CLAUDE_CONFIG_DIR",
            "CODEX_HOME",
        ] {
            assert!(!is_inherited_safe(leaked), "{leaked} must be adapter-owned");
        }
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn subprocess_env_is_sanitized_for_auth_variables() {
        // 即使父进程带毒（认证变量存在），子进程也看不到——除非 adapter
        // 显式注入。PATH 等运行所需变量仍然存活。
        let marker = "LYNCEUS_E2E_ENV_PROBE_MARKER";
        let mut env = BTreeMap::new();
        env.insert("LYNCEUS_TEST_SAFE_VAR".to_string(), marker.to_string());
        let spec = ProcessSpec {
            program: PathBuf::from("cmd"),
            args: vec![
                "/d".to_string(),
                "/c".to_string(),
                "echo SAFE=%LYNCEUS_TEST_SAFE_VAR% TOKEN=%ANTHROPIC_API_KEY%".to_string(),
            ],
            env,
            working_dir: None,
            timeout: Duration::from_secs(30),
            stdin_data: None,
        };
        let outcome = run_process(&spec).await.expect("process must run");
        let stdout = outcome.stdout.replace('\r', "");
        assert!(
            stdout.contains("SAFE="),
            "injected env must be present: {stdout}"
        );
        assert!(
            !outcome.stdout.contains("sk-"),
            "auth var must not leak: {stdout}"
        );
    }

    /// 回归：Git 装了但 `bin` 没进 PATH（只有 `...\Git\cmd` 在）时，pi 的
    /// bash 工具报 "No bash shell found" 然后什么都执行不了——worker 产出
    /// 0 发现却仍算 succeeded。这里锁住「从 cmd 反推 bin 并前置」。
    #[cfg(windows)]
    #[test]
    fn augment_path_recovers_git_bash_bin_from_a_sibling_cmd_entry() {
        let dir = tempfile::tempdir().expect("tempdir");
        let git_root = dir.path().join("Program Files").join("Git");
        std::fs::create_dir_all(git_root.join("bin")).expect("bin dir");
        std::fs::create_dir_all(git_root.join("cmd")).expect("cmd dir");
        std::fs::write(git_root.join("bin").join("bash.exe"), b"stub").expect("bash stub");

        let path = format!("{};C:\\Windows\\System32", git_root.join("cmd").display());
        let augmented = augment_path_with_bash(&path).expect("bash must be discovered");

        assert!(
            augmented.starts_with(&git_root.join("bin").display().to_string()),
            "discovered bin must be prepended: {augmented}"
        );
        assert!(
            augmented.ends_with(&path),
            "original PATH must be preserved: {augmented}"
        );
    }

    /// PATH 里已经有真 bash（非 WSL 占位）时不做任何改动——幂等，且不覆盖
    /// 使用者自己调好的 shell。
    #[cfg(windows)]
    #[test]
    fn augment_path_is_a_noop_when_a_real_bash_is_already_reachable() {
        let dir = tempfile::tempdir().expect("tempdir");
        let bin = dir.path().join("git-bin");
        std::fs::create_dir_all(&bin).expect("bin dir");
        std::fs::write(bin.join("bash.exe"), b"stub").expect("bash stub");

        let path = format!("{};C:\\Windows\\System32", bin.display());
        assert!(
            augment_path_with_bash(&path).is_none(),
            "already-usable bash must not be touched"
        );
    }

    /// 只有 WSL 占位 shim 时不算"可用"：那是 `WindowsApps\bash.exe`，没配
    /// 发行版时一跑就失败，pi 照样找不到 shell。
    #[cfg(windows)]
    #[test]
    fn wsl_placeholder_bash_is_not_treated_as_usable() {
        let path = r"C:\Users\me\AppData\Local\Microsoft\WindowsApps;C:\Windows\System32";
        // 该目录在本机不存在 → 不会误判为可用；函数应继续去寻找真 bash。
        let _ = augment_path_with_bash(path);
    }
}
