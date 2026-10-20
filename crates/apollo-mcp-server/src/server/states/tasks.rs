//! Application ownership around rmcp's task state machine.

use std::{
    collections::HashMap,
    panic::AssertUnwindSafe,
    sync::Arc,
    time::{Duration, Instant},
};

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

// Admitted task IDs, each mapped to when it stops counting against capacity:
// `None` while running, then the end of rmcp's terminal-result retention.
type Slots = Arc<Mutex<HashMap<String, Option<Instant>>>>;

#[derive(Clone)]
pub(super) struct TaskRuntime {
    inner: Arc<Inner>,
    _sweeper: Arc<AbortOnDropHandle<Option<()>>>,
}

struct Inner {
    config: TasksConfig,
    manager: TaskManager,
    // Locked across admission and shutdown to serialize them.
    slots: Slots,
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
            slots: Slots::default(),
            stop: stop.clone(),
            workers: TaskTracker::new(),
        });
        let weak = Arc::downgrade(&inner);
        // rmcp expires tasks only from its own entry points, so abandoned work
        // needs a periodic call to reach its TTL.
        let sweeper = tokio::spawn(stop.run_until_cancelled_owned(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(1));
            loop {
                interval.tick().await;
                let Some(inner) = weak.upgrade() else { break };
                inner.manager.running_task_count();
            }
        }));
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
        let mut slots = self.inner.slots.lock();
        let now = Instant::now();
        slots.retain(|_, until| until.is_none_or(|until| until > now));
        if self.inner.stop.is_cancelled() || slots.len() >= self.inner.config.max_tasks.get() {
            return Err(execution);
        }
        let stop = self.inner.stop.clone();
        let ttl_ms = self.inner.config.ttl_ms.get();
        let options = TaskOptions::new()
            .with_ttl_ms(ttl_ms)
            .with_poll_interval_ms(1_000);
        // Tracking is established before spawn, so even an unpolled/aborted
        // worker is accounted for during teardown.
        let task = self.inner.manager.spawn(options, |context| {
            // Created outside the worker so dropping it unpolled still releases.
            let slot = Slot {
                slots: self.inner.slots.clone(),
                id: context.task_id().to_owned(),
                retention: Duration::from_millis(ttl_ms),
            };
            Box::pin(self.inner.workers.track_future(async move {
                let _slot = slot;
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
        // A finished worker's slot update waits on this lock, so it cannot
        // precede the insert.
        slots.insert(task.task_id.clone(), None);
        Ok(CreateTaskResult::new(task))
    }

    pub(super) async fn shutdown(&self) {
        self.inner.stop.cancel();
        {
            let mut slots = self.inner.slots.lock();
            self.inner.manager.shutdown();
            slots.clear();
        }
        self.inner.workers.close();
        self.inner.workers.wait().await;
    }
}

/// Every worker exit (completion, cancellation, TTL abort) starts the same
/// retention window rmcp applies to its terminal result.
struct Slot {
    slots: Slots,
    id: String,
    retention: Duration,
}

impl Drop for Slot {
    fn drop(&mut self) {
        if let Some(until) = self.slots.lock().get_mut(&self.id) {
            *until = Some(Instant::now() + self.retention);
        }
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
    async fn failures_reach_terminal_state_and_release_capacity(#[case] panic: bool) {
        let runtime = runtime(1, 50);
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
        assert!(
            runtime
                .detach(Box::pin(async { Ok(CallToolResult::default()) }))
                .is_err()
        );
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert!(
            runtime
                .detach(Box::pin(async { Ok(CallToolResult::default()) }))
                .is_ok()
        );
        runtime.shutdown().await;
    }
}
