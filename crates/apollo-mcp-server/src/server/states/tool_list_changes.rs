//! Application catalog invalidation, independent of MCP notification delivery.

use std::{
    future::Future,
    sync::{Arc, OnceLock},
    time::Duration,
};

use rmcp::{Peer, RoleServer, ServiceError};
use tokio::sync::watch;
use tokio_util::{sync::CancellationToken, task::AbortOnDropHandle};
use tracing::error;

/// A coalescing signal to refetch the current tool catalog. No history is replayed.
#[derive(Clone)]
pub(super) struct ToolListChanges(watch::Sender<()>);

impl Default for ToolListChanges {
    fn default() -> Self {
        Self(watch::channel(()).0)
    }
}

impl ToolListChanges {
    /// Publish after committing the catalog and releasing its write locks.
    pub(super) fn publish(&self) {
        self.0.send_replace(());
    }

    pub(super) fn subscribe(&self) -> watch::Receiver<()> {
        self.0.subscribe()
    }

    #[cfg(test)]
    pub(super) fn receiver_count(&self) -> usize {
        self.0.receiver_count()
    }

    #[cfg(test)]
    pub(super) async fn closed(&self) {
        self.0.closed().await;
    }
}

/// Separates application state from the notification task owned by an rmcp service.
///
/// Service clones share one task owner. Dropping the last clone aborts delivery;
/// the task must never capture its owner. Stateless requests leave the cell empty.
/// Once delivery ends on a terminal error, it is not restarted for that service.
#[derive(Clone, Default)]
pub(super) enum LegacyToolNotifications {
    #[default]
    Application,
    Service(Arc<OnceLock<AbortOnDropHandle<()>>>),
}

impl LegacyToolNotifications {
    pub(super) fn for_service() -> Self {
        Self::Service(Arc::default())
    }

    pub(super) fn on_initialized(
        &self,
        peer: Peer<RoleServer>,
        changes: &ToolListChanges,
        shutdown: CancellationToken,
    ) {
        let Self::Service(task) = self else {
            return;
        };
        task.get_or_init(|| {
            // Subscribe synchronously so updates cannot race with task scheduling.
            let receiver = changes.subscribe();
            AbortOnDropHandle::new(tokio::spawn(async move {
                forward_changes(receiver, shutdown, || peer.notify_tool_list_changed()).await;
            }))
        });
    }
}

const NOTIFY_TIMEOUT: Duration = Duration::from_secs(5);

async fn forward_changes<F: Future<Output = Result<(), ServiceError>>>(
    mut changes: watch::Receiver<()>,
    shutdown: CancellationToken,
    mut notify: impl FnMut() -> F,
) {
    loop {
        tokio::select! {
            biased;
            _ = shutdown.cancelled() => return,
            changed = changes.changed() => {
                if changed.is_err() {
                    return;
                }
            }
        }

        let result = tokio::select! {
            biased;
            _ = shutdown.cancelled() => return,
            result = tokio::time::timeout(NOTIFY_TIMEOUT, notify()) => result,
        };
        match result {
            Ok(Ok(())) => {}
            Ok(Err(ServiceError::TransportSend(_) | ServiceError::TransportClosed)) => {
                error!("Failed to notify client of tool list change - stopping legacy delivery");
                return;
            }
            Ok(Err(error)) => {
                error!(?error, "Failed to notify client of tool list change");
            }
            Err(_) => {
                error!(
                    "Timed out notifying client of tool list change after 5s - stopping legacy delivery"
                );
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    proptest::proptest! {
        /// Each scheduled batch leaves at most one pending invalidation. A simple
        /// boolean model predicts sends without reproducing the watch implementation.
        #[test]
        fn forwarding_matches_pending_invalidation_model(
            batches in proptest::collection::vec(0usize..8, 1..32),
            stop_after in 0usize..32,
        ) {
            let runtime = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
            runtime.block_on(async {
                let changes = ToolListChanges::default();
                let shutdown = CancellationToken::new();
                let token = shutdown.clone();
                let receiver = changes.subscribe();
                let (sent, mut received) = tokio::sync::mpsc::unbounded_channel();
                let release = Arc::new(tokio::sync::Semaphore::new(0));
                let permits = release.clone();
                let task = tokio::spawn(async move {
                    let mut attempts = 0;
                    forward_changes(receiver, token, || {
                        attempts += 1;
                        sent.send(()).unwrap();
                        async {
                            permits.acquire().await.unwrap().forget();
                            Ok(())
                        }
                    }).await;
                    attempts
                });
                // Hold a first send in progress while generating further changes.
                changes.publish();
                received.recv().await.unwrap();
                let mut expected = 1;
                for count in batches.into_iter().take(stop_after) {
                    let mut pending = false;
                    for _ in 0..count {
                        changes.publish();
                        pending = true;
                    }
                    if pending {
                        expected += 1;
                        release.add_permits(1);
                        tokio::time::timeout(Duration::from_secs(2), received.recv()).await.unwrap().unwrap();
                    }
                    assert!(received.try_recv().is_err());
                }
                // Cancellation must end even a blocked send, discarding pending work.
                changes.publish();
                shutdown.cancel();
                let attempts = tokio::time::timeout(Duration::from_secs(2), task).await.unwrap().unwrap();
                assert_eq!(attempts, expected);
                assert!(received.try_recv().is_err());
                assert_eq!(changes.receiver_count(), 0);
            });
        }
    }

    #[tokio::test]
    async fn changes_are_coalesced_for_each_receiver_without_replay() {
        let changes = ToolListChanges::default();
        changes.publish(); // No subscribers is normal.
        let mut first = changes.subscribe();
        let mut second = changes.clone().subscribe();
        assert!(!first.has_changed().unwrap());
        changes.publish();
        changes.publish();
        first.changed().await.unwrap();
        second.changed().await.unwrap();
        assert!(!first.has_changed().unwrap());
        assert!(!second.has_changed().unwrap());
    }

    #[tokio::test]
    async fn shutdown_interrupts_a_blocked_send() {
        let changes = ToolListChanges::default();
        let shutdown = CancellationToken::new();
        let (started_tx, mut started_rx) = tokio::sync::mpsc::unbounded_channel();
        let receiver = changes.subscribe();
        let token = shutdown.clone();
        let task = tokio::spawn(async move {
            forward_changes(receiver, token, || {
                started_tx.send(()).unwrap();
                std::future::pending()
            })
            .await;
        });
        changes.publish();
        started_rx.recv().await.unwrap();
        shutdown.cancel();
        task.await.unwrap();
        changes.closed().await;
    }

    #[tokio::test]
    async fn recoverable_send_errors_do_not_stop_delivery() {
        let changes = ToolListChanges::default();
        let receiver = changes.subscribe();
        let (sent, mut received) = tokio::sync::mpsc::unbounded_channel();
        let task = tokio::spawn(async move {
            let mut attempts = 0;
            forward_changes(receiver, CancellationToken::new(), || {
                attempts += 1;
                sent.send(()).unwrap();
                std::future::ready(if attempts == 1 {
                    Err(ServiceError::UnexpectedResponse)
                } else {
                    Err(ServiceError::TransportClosed)
                })
            })
            .await;
            attempts
        });
        changes.publish();
        received.recv().await.unwrap();
        changes.publish();
        assert_eq!(task.await.unwrap(), 2);
        changes.closed().await;
    }
    #[tokio::test(start_paused = true)]
    async fn slow_client_times_out_without_delaying_another_client() {
        let changes = ToolListChanges::default();
        let slow = changes.subscribe();
        let fast = changes.subscribe();
        let shutdown = CancellationToken::new();
        let fast_shutdown = shutdown.clone();
        let (sent, mut received) = tokio::sync::mpsc::unbounded_channel();
        let slow_task = tokio::spawn(forward_changes(
            slow,
            CancellationToken::new(),
            std::future::pending,
        ));
        let fast_task = tokio::spawn(async move {
            forward_changes(fast, fast_shutdown, || {
                sent.send(()).unwrap();
                std::future::ready(Ok(()))
            })
            .await;
        });
        let start = tokio::time::Instant::now();
        changes.publish();
        received.recv().await.unwrap();
        // A second update can be delivered while the first client's send is blocked.
        changes.publish();
        received.recv().await.unwrap();
        assert_eq!(start.elapsed(), Duration::ZERO);
        shutdown.cancel();
        fast_task.await.unwrap();
        slow_task.await.unwrap();
        assert_eq!(start.elapsed(), NOTIFY_TIMEOUT);
        changes.closed().await;
    }

    #[tokio::test]
    async fn dropping_the_change_source_ends_delivery() {
        let changes = ToolListChanges::default();
        let receiver = changes.subscribe();
        drop(changes);
        forward_changes(receiver, CancellationToken::new(), || async {
            panic!("closed source must not send a notification")
        })
        .await;
    }

    #[tokio::test]
    async fn changes_during_a_send_are_delivered_after_it_finishes() {
        let changes = ToolListChanges::default();
        let receiver = changes.subscribe();
        let shutdown = CancellationToken::new();
        let task_shutdown = shutdown.clone();
        let release = Arc::new(tokio::sync::Semaphore::new(0));
        let permit = release.clone();
        let (sent, mut received) = tokio::sync::mpsc::unbounded_channel();
        let task = tokio::spawn(async move {
            forward_changes(receiver, task_shutdown, || async {
                sent.send(()).unwrap();
                permit.acquire().await.unwrap().forget();
                Ok(())
            })
            .await;
        });
        changes.publish();
        received.recv().await.unwrap();
        changes.publish();
        changes.publish();
        release.add_permits(1);
        received.recv().await.unwrap();
        shutdown.cancel();
        task.await.unwrap();
    }
}
