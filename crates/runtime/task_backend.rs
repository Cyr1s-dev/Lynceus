//! TaskBackend：执行调度抽象 —— `server/core/storage/task_backend.py` 的移植。
//!
//! manager 提交产出 future 的闭包；backend 决定它**如何**运行（inline、
//! 线程池、Redis worker、NATS JetStream、Temporal）。MVP 提供在当前
//! tokio 运行时上直接 spawn 的内存实现，但所有调用点只依赖本接口，
//! 后端可替换而核心逻辑不动。
//!
//! 与 Python 的两处映射差异（记录于 `docs/REFACTOR_PROGRESS.md`）：
//! 1. `TaskFn` 是 `FnOnce() -> BoxFuture`：Python 侧闭包产出 coroutine，
//!    此处闭包产出 boxed future——未被 poll 的 future 与未被 await 的
//!    coroutine 一样不会执行；
//! 2. `asyncio` 任务名（`loop.create_task(coro, name=task_id)`）在 tokio
//!    无对应物，改为 `tracing` 日志记录 task_id。

#![allow(clippy::doc_markdown)]
//!
//! `asyncio.Task` 允许多方持有同一任务（backend 的跟踪 set 与句柄共享
//! 引用，`result()` 可多次 await）；tokio 的 `JoinHandle` 是单消费者。移植
//! 用**监督任务**解耦：监督任务独占 `JoinHandle` 并把结局发到 `watch` 频道，
//! 句柄拿 `AbortHandle`（cancel）+ `watch::Receiver`（done/result），backend
//! 跟踪监督任务的句柄用于 `shutdown` 排空。

use std::future::Future;
use std::pin::Pin;

use tokio::sync::watch;
use tokio::task::AbortHandle;
use tokio::task::JoinHandle;

/// 一个已装箱的单位工作 future（Python `Coroutine[Any, Any, Any]`）。
pub type TaskFuture = Pin<Box<dyn Future<Output = ()> + Send>>;
/// 单位工作：产出 future 的闭包（Python `TaskFn`）。
pub type TaskFn = Box<dyn FnOnce() -> TaskFuture + Send>;

/// 被监督任务的结局（监督任务发布到 watch 频道的终值）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Completion {
    /// 尚未结束。
    Pending,
    /// 正常完成。
    Succeeded,
    /// 被取消（Python `asyncio.CancelledError`）。
    Cancelled,
    /// panic。
    Panicked,
}

/// 等待已提交任务时的失败模式（Python 侧以异常类型表达）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TaskJoinError {
    /// 任务被取消（`asyncio.CancelledError`）。
    #[error("task was cancelled")]
    Cancelled,
    /// 任务 panic。
    #[error("task panicked")]
    Panicked,
}

/// 已提交工作的句柄（Python `TaskHandle`）：`result()` 等待完成。
///
/// `done()` / `cancel()` 可从任意共享方调用；`result()` 可多次 await
/// （`watch` 频道保留终值）。
#[derive(Debug)]
pub struct TaskHandle {
    task_id: String,
    abort: AbortHandle,
    completion: watch::Receiver<Completion>,
}

impl TaskHandle {
    /// 包装一个已 spawn 的任务：启动监督任务发布其结局。
    ///
    /// 返回（句柄， 监督任务句柄）——调用方（backend）持有监督任务用于
    /// `shutdown` 排空。
    fn supervise(task_id: String, join: JoinHandle<()>) -> (Self, JoinHandle<()>) {
        let abort = join.abort_handle();
        let (completion_tx, completion) = watch::channel(Completion::Pending);
        let supervisor = tokio::spawn(async move {
            let outcome = match join.await {
                Ok(()) => Completion::Succeeded,
                Err(err) if err.is_cancelled() => Completion::Cancelled,
                Err(_) => Completion::Panicked,
            };
            let _ = completion_tx.send(outcome);
        });
        (
            Self {
                task_id,
                abort,
                completion,
            },
            supervisor,
        )
    }

    /// 任务 id（Python `task_id`）。
    #[must_use]
    pub fn task_id(&self) -> &str {
        &self.task_id
    }

    /// 任务是否已结束（Python `done()`）。
    #[must_use]
    pub fn is_finished(&self) -> bool {
        *self.completion.borrow() != Completion::Pending
    }

    /// 请求取消底层工作（Python `cancel()`）：任务已结束返回 `false`。
    ///
    /// 注意与 Python 的语义差异：`asyncio` 取消会在协程内抛
    /// `CancelledError`，让运行时自行落盘"中断"状态；tokio `abort` 直接
    /// drop future，被中断的代码不会执行——"中断 → PAUSED"的落盘由取消
    /// 的发起方（`pause_mission`）负责，见 manager 文档。
    #[must_use]
    pub fn cancel(&self) -> bool {
        if *self.completion.borrow() != Completion::Pending {
            return false;
        }
        self.abort.abort();
        true
    }

    /// 等待并返回任务结果（Python `result()`，异常重抛）。
    ///
    /// # Errors
    /// 任务被取消（`asyncio.CancelledError` 对应物）或 panic。
    pub async fn result(&mut self) -> Result<(), TaskJoinError> {
        while *self.completion.borrow() == Completion::Pending {
            if self.completion.changed().await.is_err() {
                // 发送端在发布终值前消失（监督任务被外部 abort，正常路径
                // 不发生）；按取消处理。
                return Err(TaskJoinError::Cancelled);
            }
        }
        match *self.completion.borrow() {
            Completion::Succeeded => Ok(()),
            Completion::Cancelled | Completion::Pending => Err(TaskJoinError::Cancelled),
            Completion::Panicked => Err(TaskJoinError::Panicked),
        }
    }
}

/// 异步工作的提交-跟踪接口（Python `TaskBackend` Protocol）。
///
/// 实现必须立即返回 [`TaskHandle`] 且不阻塞调用方；`shutdown` 等待
/// 在飞任务排空。
#[async_trait::async_trait]
pub trait TaskBackend: Send + Sync {
    /// 提交一个任务并立即返回句柄。
    async fn submit(&self, task_id: &str, task_fn: TaskFn) -> TaskHandle;

    /// 等待在飞任务排空（Python `shutdown`）。
    async fn shutdown(&self);
}

/// 在当前 tokio 运行时上逐个 spawn 的内存后端（Python
/// `InMemoryTaskBackend`）。
///
/// 适用 MVP 与测试；跟踪在飞任务使 `shutdown` 能等待它们。满足
/// [`TaskBackend`]。
#[derive(Debug, Default)]
pub struct InMemoryTaskBackend {
    tasks: tokio::sync::Mutex<Vec<JoinHandle<()>>>,
}

#[async_trait::async_trait]
impl TaskBackend for InMemoryTaskBackend {
    async fn submit(&self, task_id: &str, task_fn: TaskFn) -> TaskHandle {
        let join = tokio::spawn(task_fn());
        let (handle, supervisor) = TaskHandle::supervise(task_id.to_string(), join);
        tracing::info!(task_id, "task submitted");
        let mut tasks = self.tasks.lock().await;
        // add_done_callback(discard) 的对应物：先清理已结束的句柄。
        tasks.retain(|handle| !handle.is_finished());
        tasks.push(supervisor);
        handle
    }

    async fn shutdown(&self) {
        let mut tasks = self.tasks.lock().await;
        for handle in tasks.drain(..) {
            // gather(..., return_exceptions=True)：逐个等待，异常不中断排空。
            let _ = handle.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn in_memory_backend_runs_submitted_work_and_reports_result() {
        let backend = InMemoryTaskBackend::default();
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let task_fn: TaskFn = Box::new(move || {
            Box::pin(async move {
                let _ = rx.await;
            })
        });
        let mut handle = backend.submit("probe", task_fn).await;
        assert!(!handle.is_finished());
        tx.send(()).expect("接收端存活");
        handle.result().await.expect("任务应成功");
        assert!(handle.is_finished());

        backend.shutdown().await;
    }

    #[tokio::test]
    async fn result_is_awaitable_multiple_times() {
        let backend = InMemoryTaskBackend::default();
        let task_fn: TaskFn = Box::new(|| Box::pin(async {}));
        let mut handle = backend.submit("once", task_fn).await;
        handle.result().await.expect("任务应成功");
        // asyncio.Task 允许多次 await result()；watch 终值保证同语义。
        handle.result().await.expect("重复 await 应复用终值");
        backend.shutdown().await;
    }

    #[tokio::test]
    async fn cancel_on_finished_task_returns_false() {
        let backend = InMemoryTaskBackend::default();
        let task_fn: TaskFn = Box::new(|| Box::pin(async {}));
        let mut handle = backend.submit("quick", task_fn).await;
        handle.result().await.expect("任务应成功");
        assert!(!handle.cancel());
        backend.shutdown().await;
    }

    #[tokio::test]
    async fn cancelled_task_reports_cancelled_result() {
        let backend = InMemoryTaskBackend::default();
        let task_fn: TaskFn = Box::new(|| {
            Box::pin(async {
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            })
        });
        let mut handle = backend.submit("blocked", task_fn).await;
        assert!(handle.cancel());
        assert_eq!(handle.result().await.err(), Some(TaskJoinError::Cancelled));
        backend.shutdown().await;
    }

    #[tokio::test]
    async fn shutdown_awaits_inflight_tasks() {
        let backend = InMemoryTaskBackend::default();
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let task_fn: TaskFn = Box::new(move || {
            Box::pin(async move {
                let _ = rx.await;
            })
        });
        let handle = backend.submit("blocked", task_fn).await;
        let backend = std::sync::Arc::new(backend);
        let shutdown = {
            let backend = backend.clone();
            tokio::spawn(async move {
                backend.shutdown().await;
            })
        };
        tokio::task::yield_now().await;
        assert!(!handle.is_finished());
        tx.send(()).expect("接收端存活");
        shutdown.await.expect("shutdown 任务应成功");
    }
}
