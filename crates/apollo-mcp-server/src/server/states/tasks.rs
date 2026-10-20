//! Application ownership around rmcp's task state machine.

use std::{collections::HashSet, panic::AssertUnwindSafe, sync::Arc, time::Duration};

use futures::{FutureExt as _, future::BoxFuture};
use parking_lot::Mutex;
use rmcp::{
    ErrorData,
    model::{CallToolResult, CreateTaskResult},
    task_manager::{TaskExit, TaskManager, TaskOptions},
};
use tokio_util::{
    sync::CancellationToken,
    task::{AbortOnDropHandle, TaskTracker},
};

use crate::tasks::TasksConfig;

pub(super) type Execution = BoxFuture<'static, Result<CallToolResult, ErrorData>>;

#[derive(Clone)]
pub(super) struct TaskRuntime {
    inner: Arc<Inner>,
    _sweeper: Arc<AbortOnDropHandle<()>>,
}

struct Inner {
    config: TasksConfig,
    manager: TaskManager,
    // Serializes admission with shutdown; includes retained terminal results.
    ids: Mutex<HashSet<String>>,
    stop: CancellationToken,
    workers: TaskTracker,
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.stop.cancel();
        self.manager.shutdown();
    }
}

impl TaskRuntime {
    pub(super) fn new(config: TasksConfig, shutdown: CancellationToken) -> Self {
        let stop = shutdown.child_token();
        let inner = Arc::new(Inner {
            config,
            manager: TaskManager::new(),
            ids: Mutex::new(HashSet::new()),
            stop: stop.clone(),
            workers: TaskTracker::new(),
        });
        // Spawn only after construction: upgrading a cyclic Arc during its
        // construction can fail if the sweeper runs on another runtime thread.
        let weak = Arc::downgrade(&inner);
        let sweeper = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(1));
            loop {
                tokio::select! {
                    biased;
                    _ = stop.cancelled() => {
                        if let Some(inner) = weak.upgrade() {
                            inner.ids.lock().clear();
                            inner.manager.shutdown();
                        }
                        break;
                    }
                    _ = interval.tick() => {
                        let Some(inner) = weak.upgrade() else { break };
                        inner.ids.lock().retain(|id| inner.manager.get_task(id).is_ok());
                    }
                }
            }
        });
        Self {
            inner,
            _sweeper: Arc::new(AbortOnDropHandle::new(sweeper)),
        }
    }

    pub(super) fn delay(&self) -> Duration {
        Duration::from_millis(self.inner.config.detach_after_ms)
    }

    pub(super) fn manager(&self) -> &TaskManager {
        &self.inner.manager
    }

    /// Return ownership unchanged when admission fails. The caller keeps waiting
    /// on the same execution, including any upstream side effects already begun.
    pub(super) fn detach(&self, execution: Execution) -> Result<CreateTaskResult, Execution> {
        let mut ids = self.inner.ids.lock();
        ids.retain(|id| self.inner.manager.get_task(id).is_ok());
        if self.inner.stop.is_cancelled() || ids.len() >= self.inner.config.max_tasks.get() {
            return Err(execution);
        }
        let stop = self.inner.stop.clone();
        let options = TaskOptions::new()
            .with_ttl_ms(self.inner.config.ttl_ms.get())
            .with_poll_interval_ms(1_000);
        // Tracking is established before spawn, so even an unpolled/aborted
        // worker is accounted for during teardown.
        let task = self.inner.manager.spawn(options, |context| {
            Box::pin(self.inner.workers.track_future(async move {
                tokio::select! {
                    biased;
                    _ = stop.cancelled() => Err(TaskExit::Cancelled),
                    _ = context.cancelled() => Err(TaskExit::Cancelled),
                    result = AssertUnwindSafe(execution).catch_unwind() => {
                        match result {
                            Ok(result) => result.map_err(TaskExit::Error),
                            Err(_) => Err(TaskExit::Error(ErrorData::internal_error("GraphQL task panicked", None))),
                        }
                    }
                }
            }))
        });
        ids.insert(task.task_id.clone());
        Ok(CreateTaskResult::new(task))
    }

    pub(super) async fn shutdown(&self) {
        self.inner.stop.cancel();
        {
            let mut ids = self.inner.ids.lock();
            self.inner.manager.shutdown();
            ids.clear();
        }
        self.inner.workers.close();
        self.inner.workers.wait().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::model::TaskStatus;

    fn runtime(max: usize, ttl_ms: u64) -> TaskRuntime {
        TaskRuntime::new(
            TasksConfig {
                enabled: true,
                max_tasks: max.try_into().unwrap(),
                ttl_ms: ttl_ms.try_into().unwrap(),
                ..Default::default()
            },
            CancellationToken::new(),
        )
    }

    #[rstest::rstest]
    #[tokio::test]
    #[timeout(Duration::from_secs(10))]
    async fn concurrent_admission_counts_terminal_results_and_shutdown_releases_workers() {
        let runtime = runtime(1, 300_000);
        let dropped = CancellationToken::new();
        let guard = dropped.clone().drop_guard();
        let task = runtime
            .detach(Box::pin(async move {
                let _guard = guard;
                futures::future::pending().await
            }))
            .ok()
            .unwrap();
        let callers = (0..10)
            .map(|_| {
                let runtime = runtime.clone();
                tokio::spawn(async move {
                    let execution = runtime
                        .detach(Box::pin(async { Ok(CallToolResult::default()) }))
                        .expect_err("at capacity");
                    // Failed admission returns the original future, still executable.
                    assert!(execution.await.is_ok());
                })
            })
            .collect::<Vec<_>>();
        for caller in callers {
            caller.await.unwrap();
        }
        runtime.manager().cancel_task(&task.task.task_id).unwrap();
        dropped.cancelled().await;
        while runtime
            .manager()
            .get_task(&task.task.task_id)
            .unwrap()
            .status()
            != TaskStatus::Cancelled
        {
            tokio::task::yield_now().await;
        }
        assert!(
            runtime
                .detach(Box::pin(async { Ok(CallToolResult::default()) }))
                .is_err()
        );
        runtime.shutdown().await;
        assert!(runtime.manager().get_task(&task.task.task_id).is_err());
        assert!(
            runtime
                .detach(Box::pin(async { Ok(CallToolResult::default()) }))
                .is_err()
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    #[timeout(Duration::from_secs(10))]
    async fn abandoned_work_expires_without_polling_and_releases_capacity() {
        let runtime = runtime(1, 10);
        let started = CancellationToken::new();
        let dropped = CancellationToken::new();
        let guard = dropped.clone().drop_guard();
        let ready = started.clone();
        let task = runtime
            .detach(Box::pin(async move {
                let _guard = guard;
                ready.cancel();
                futures::future::pending().await
            }))
            .ok()
            .unwrap();
        started.cancelled().await;
        // The cleanup timer, not a tasks/get call, must abort abandoned work.
        dropped.cancelled().await;
        assert_eq!(
            runtime
                .manager()
                .get_task(&task.task.task_id)
                .unwrap()
                .status(),
            TaskStatus::Failed
        );
        // rmcp uses std::time::Instant; await the actual retention deadline.
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(
            runtime
                .detach(Box::pin(async { Ok(CallToolResult::default()) }))
                .is_ok()
        );
        runtime.shutdown().await;
    }

    #[rstest::rstest]
    #[tokio::test]
    #[timeout(Duration::from_secs(10))]
    async fn dropping_owner_aborts_started_worker() {
        let runtime = runtime(1, 300_000);
        let started = CancellationToken::new();
        let dropped = CancellationToken::new();
        let guard = dropped.clone().drop_guard();
        let ready = started.clone();
        assert!(
            runtime
                .detach(Box::pin(async move {
                    let _guard = guard;
                    ready.cancel();
                    futures::future::pending().await
                }))
                .is_ok()
        );
        started.cancelled().await;
        drop(runtime);
        dropped.cancelled().await;
    }

    #[rstest::rstest]
    #[case::protocol(false)]
    #[case::panic(true)]
    #[tokio::test]
    #[timeout(Duration::from_secs(10))]
    async fn failures_reach_terminal_state(#[case] panic: bool) {
        let runtime = runtime(1, 300_000);
        let task = runtime
            .detach(Box::pin(async move {
                assert!(!panic, "test worker panic");
                Err(ErrorData::invalid_params("bad input", None))
            }))
            .ok()
            .unwrap();
        loop {
            let result = runtime.manager().get_task(&task.task.task_id).unwrap();
            if result.status().is_terminal() {
                assert_eq!(result.status(), TaskStatus::Failed);
                break;
            }
            tokio::task::yield_now().await;
        }
        runtime.shutdown().await;
    }
}
