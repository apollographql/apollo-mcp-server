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
    model::{CallToolResult, CreateTaskResult, DetailedTask, InputResponses},
    task_manager::{TaskExit, TaskManager, TaskOptions},
};
use tokio_util::{
    sync::CancellationToken,
    task::{AbortOnDropHandle, TaskTracker},
};

use crate::{auth::Principal, tasks::TasksConfig};

pub(super) type Execution = BoxFuture<'static, Result<CallToolResult, ErrorData>>;

type Slots = Arc<Mutex<HashMap<String, Slot>>>;

/// An admitted task's owner and capacity reservation.
struct Slot {
    owner: Option<Principal>,
    // When the task stops counting against capacity: `None` while running,
    // then the end of rmcp's terminal-result retention.
    until: Option<Instant>,
}

#[derive(Clone)]
pub(super) struct TaskRuntime {
    inner: Arc<Inner>,
    _sweeper: Arc<AbortOnDropHandle<Option<()>>>,
}

struct Inner {
    config: TasksConfig,
    // Set when auth is enabled. A tokenless call stays synchronous: it could
    // not authenticate the `tasks/*` requests needed to read its result.
    require_owner: bool,
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
    pub(super) fn new(
        config: TasksConfig,
        shutdown: CancellationToken,
        require_owner: bool,
    ) -> Self {
        let stop = shutdown.child_token();
        let inner = Arc::new(Inner {
            config,
            require_owner,
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

    #[cfg(test)]
    pub(super) fn manager(&self) -> &TaskManager {
        &self.inner.manager
    }

    /// Return ownership unchanged when admission fails. The caller keeps waiting
    /// on the same execution, including any upstream side effects already begun.
    pub(super) fn detach(
        &self,
        owner: Option<Principal>,
        execution: Execution,
    ) -> Result<CreateTaskResult, Execution> {
        if self.inner.require_owner && owner.is_none() {
            return Err(execution);
        }
        let mut slots = self.inner.slots.lock();
        let now = Instant::now();
        slots.retain(|_, slot| slot.until.is_none_or(|until| until > now));
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
            let release = Release {
                slots: self.inner.slots.clone(),
                id: context.task_id().to_owned(),
                retention: Duration::from_millis(ttl_ms),
            };
            Box::pin(self.inner.workers.track_future(async move {
                let _release = release;
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
        slots.insert(task.task_id.clone(), Slot { owner, until: None });
        Ok(CreateTaskResult::new(task))
    }

    pub(super) fn get(
        &self,
        task_id: &str,
        owner: Option<&Principal>,
    ) -> Result<DetailedTask, ErrorData> {
        self.owned(task_id, owner, |manager| manager.get_task(task_id))
    }

    pub(super) fn cancel(&self, task_id: &str, owner: Option<&Principal>) -> Result<(), ErrorData> {
        self.owned(task_id, owner, |manager| manager.cancel_task(task_id))
    }

    pub(super) fn update(
        &self,
        task_id: &str,
        owner: Option<&Principal>,
        responses: InputResponses,
    ) -> Result<(), ErrorData> {
        self.owned(task_id, owner, |manager| {
            manager.update_task(task_id, responses)
        })
    }

    fn owned<T>(
        &self,
        task_id: &str,
        owner: Option<&Principal>,
        lookup: impl FnOnce(&TaskManager) -> Result<T, ErrorData>,
    ) -> Result<T, ErrorData> {
        let owned = self
            .inner
            .slots
            .lock()
            .get(task_id)
            .is_some_and(|slot| slot.owner.as_ref() == owner);
        if !owned {
            return Err(task_lookup_error());
        }
        lookup(&self.inner.manager).map_err(|_| task_lookup_error())
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

// Unknown, expired, and other principals' tasks share one error, so a lookup
// can't confirm that someone else's task exists. rmcp's own error includes the
// task ID and is logged at WARN; it must not reach logs or clients.
fn task_lookup_error() -> ErrorData {
    ErrorData::invalid_params("Task not found or expired", None)
}

/// Every worker exit (completion, cancellation, TTL abort) starts the same
/// retention window rmcp applies to its terminal result.
struct Release {
    slots: Slots,
    id: String,
    retention: Duration,
}

impl Drop for Release {
    fn drop(&mut self) {
        if let Some(slot) = self.slots.lock().get_mut(&self.id) {
            slot.until = Some(Instant::now() + self.retention);
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
            false,
        )
    }

    fn principal(issuer: &str, subject: &str) -> Principal {
        Principal {
            issuer: issuer.to_string(),
            subject: subject.to_string(),
        }
    }

    #[rstest::rstest]
    #[tokio::test]
    #[timeout(Duration::from_secs(10))]
    async fn concurrent_admission_counts_terminal_results_and_shutdown_releases_workers() {
        let runtime = runtime(1, 300_000);
        let dropped = CancellationToken::new();
        let guard = dropped.clone().drop_guard();
        let task = runtime
            .detach(
                None,
                Box::pin(async move {
                    let _guard = guard;
                    futures::future::pending().await
                }),
            )
            .ok()
            .unwrap();
        let callers = (0..10)
            .map(|_| {
                let runtime = runtime.clone();
                tokio::spawn(async move {
                    let execution = runtime
                        .detach(None, Box::pin(async { Ok(CallToolResult::default()) }))
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
                .detach(None, Box::pin(async { Ok(CallToolResult::default()) }))
                .is_err()
        );
        runtime.shutdown().await;
        assert!(runtime.manager().get_task(&task.task.task_id).is_err());
        assert!(
            runtime
                .detach(None, Box::pin(async { Ok(CallToolResult::default()) }))
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
            .detach(
                None,
                Box::pin(async move {
                    let _guard = guard;
                    ready.cancel();
                    futures::future::pending().await
                }),
            )
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
                .detach(None, Box::pin(async { Ok(CallToolResult::default()) }))
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
                .detach(
                    None,
                    Box::pin(async move {
                        let _guard = guard;
                        ready.cancel();
                        futures::future::pending().await
                    })
                )
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
            .detach(
                None,
                Box::pin(async move {
                    assert!(!panic, "test worker panic");
                    Err(ErrorData::invalid_params("bad input", None))
                }),
            )
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
                .detach(None, Box::pin(async { Ok(CallToolResult::default()) }))
                .is_err()
        );
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert!(
            runtime
                .detach(None, Box::pin(async { Ok(CallToolResult::default()) }))
                .is_ok()
        );
        runtime.shutdown().await;
    }

    #[rstest::rstest]
    #[tokio::test]
    #[timeout(Duration::from_secs(10))]
    async fn tasks_are_visible_only_to_their_owner() {
        let runtime = TaskRuntime::new(
            TasksConfig {
                enabled: true,
                ..Default::default()
            },
            CancellationToken::new(),
            true,
        );
        let execution = runtime
            .detach(None, Box::pin(async { Ok(CallToolResult::default()) }))
            .expect_err("an owner is required");
        assert!(execution.await.is_ok());

        let owner = principal("https://idp.example.com", "alice");
        let task = runtime
            .detach(Some(owner.clone()), Box::pin(futures::future::pending()))
            .ok()
            .unwrap();
        let id = task.task.task_id.as_str();
        let unknown = runtime.get("unknown", Some(&owner)).unwrap_err();
        for other in [
            None,
            Some(principal("https://idp.example.com", "mallory")),
            Some(principal("https://other.example.com", "alice")),
        ] {
            let other = other.as_ref();
            assert_eq!(runtime.get(id, other).unwrap_err(), unknown);
            assert_eq!(runtime.cancel(id, other).unwrap_err(), unknown);
            assert_eq!(
                runtime.update(id, other, Default::default()).unwrap_err(),
                unknown
            );
        }
        assert_eq!(
            runtime.get(id, Some(&owner)).unwrap().status(),
            TaskStatus::Working
        );
        runtime
            .update(id, Some(&owner), Default::default())
            .unwrap();
        runtime.cancel(id, Some(&owner)).unwrap();
        runtime.shutdown().await;
    }
}
