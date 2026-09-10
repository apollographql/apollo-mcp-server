//! Application catalog invalidation, independent of MCP notification delivery.

use std::{future::Future, sync::Arc};

use rmcp::{Peer, RoleServer, ServiceError};
use tokio::sync::watch;
use tokio_util::{sync::CancellationToken, task::AbortOnDropHandle};
use tracing::{debug, error};

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

/// One legacy lifecycle shared by clones of an rmcp service. The task never
/// captures this owner, so final-owner drop releases pending or active delivery.
#[derive(Clone, Default)]
pub(super) struct LegacyToolNotifications(Arc<parking_lot::Mutex<Lifecycle>>);

#[derive(Default)]
enum Lifecycle {
    #[default]
    AwaitingInitialize,
    AwaitingInitialized(watch::Receiver<()>),
    // Records that delivery was started, even after a terminal send error. Such
    // failures do not implicitly restart delivery on repeated notifications.
    Started {
        _task: AbortOnDropHandle<()>,
    },
}

impl LegacyToolNotifications {
    pub(super) fn initialize(&self, changes: &ToolListChanges) {
        let mut state = self.0.lock();
        if matches!(*state, Lifecycle::AwaitingInitialize) {
            *state = Lifecycle::AwaitingInitialized(changes.subscribe());
        }
    }

    pub(super) fn on_initialized(&self, peer: Peer<RoleServer>, shutdown: CancellationToken) {
        let mut state = self.0.lock();
        *state = match std::mem::take(&mut *state) {
            Lifecycle::AwaitingInitialized(receiver) => Lifecycle::Started {
                _task: AbortOnDropHandle::new(tokio::spawn(async move {
                    forward_changes(receiver, shutdown, || peer.notify_tool_list_changed()).await;
                })),
            },
            unchanged => unchanged,
        };
    }
}

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
            result = notify() => result,
        };
        match result {
            Ok(()) => {}
            // Preserve the previous best-effort delivery policy for transport
            // failures; TransportSend does not universally imply permanent closure.
            Err(ServiceError::TransportSend(error)) => {
                error!(
                    ?error,
                    "Failed to notify client of tool list change - stopping legacy delivery"
                );
                return;
            }
            Err(ServiceError::TransportClosed) => {
                debug!("Client transport closed - stopping legacy tool notifications");
                return;
            }
            Err(error) => {
                error!(?error, "Failed to notify client of tool list change");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[derive(Clone, Copy, Debug)]
    enum Action {
        Publish,
        Succeed,
        RecoverableFailure,
        TransportClosed,
        TransportSend,
        Cancel,
    }

    fn transport_send_error() -> ServiceError {
        ServiceError::TransportSend(rmcp::transport::DynamicTransportError::from_parts(
            "test transport",
            std::any::TypeId::of::<()>(),
            Box::new(std::io::Error::from(std::io::ErrorKind::BrokenPipe)),
        ))
    }

    #[derive(Clone, Copy)]
    enum Model {
        Idle,
        Sending { pending: bool },
        Stopped,
    }

    proptest::proptest! {
        #[test]
        fn forwarding_matches_pending_invalidation_model(
            actions in proptest::collection::vec(proptest::prop_oneof![
                proptest::strategy::Just(Action::Publish),
                proptest::strategy::Just(Action::Succeed),
                proptest::strategy::Just(Action::RecoverableFailure),
                proptest::strategy::Just(Action::TransportClosed),
                proptest::strategy::Just(Action::TransportSend),
                proptest::strategy::Just(Action::Cancel),
            ], 1..64)
        ) {
            let runtime = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
            runtime.block_on(check_forwarding_model(actions));
        }
    }

    async fn check_forwarding_model(actions: Vec<Action>) {
        use std::cell::{Cell, RefCell};
        let changes = ToolListChanges::default();
        let shutdown = CancellationToken::new();
        let attempts = Cell::new(0);
        let completion = RefCell::new(None);
        let forwarding = forward_changes(changes.subscribe(), shutdown.clone(), || {
            attempts.set(attempts.get() + 1);
            let (send, receive) = tokio::sync::oneshot::channel();
            *completion.borrow_mut() = Some(send);
            async move { receive.await.unwrap() }
        });
        tokio::pin!(forwarding);
        let mut model = Model::Idle;
        let mut expected = 0;
        assert!(futures::poll!(&mut forwarding).is_pending());
        // Polling explicitly controls scheduling: each action runs to the next
        // suspension point, without scheduler sleeps or production test hooks.
        for action in actions.into_iter().chain([Action::Cancel]) {
            if matches!(model, Model::Stopped) {
                changes.publish();
                assert_eq!(attempts.get(), expected);
                continue;
            }
            match action {
                Action::Publish => {
                    changes.publish();
                    model = match model {
                        Model::Idle => {
                            expected += 1;
                            Model::Sending { pending: false }
                        }
                        Model::Sending { .. } => Model::Sending { pending: true },
                        Model::Stopped => Model::Stopped,
                    };
                }
                Action::Cancel => {
                    shutdown.cancel();
                    model = Model::Stopped;
                }
                Action::Succeed
                | Action::RecoverableFailure
                | Action::TransportClosed
                | Action::TransportSend => {
                    if let Model::Sending { pending } = model {
                        let result = match action {
                            Action::Succeed => Ok(()),
                            Action::RecoverableFailure => Err(ServiceError::UnexpectedResponse),
                            Action::TransportSend => Err(transport_send_error()),
                            _ => Err(ServiceError::TransportClosed),
                        };
                        completion
                            .borrow_mut()
                            .take()
                            .unwrap()
                            .send(result)
                            .unwrap();
                        model = if matches!(action, Action::TransportClosed | Action::TransportSend)
                        {
                            Model::Stopped
                        } else if pending {
                            expected += 1;
                            Model::Sending { pending: false }
                        } else {
                            Model::Idle
                        };
                    }
                }
            }
            assert_eq!(
                futures::poll!(&mut forwarding).is_ready(),
                matches!(model, Model::Stopped)
            );
            assert_eq!(attempts.get(), expected);
        }
    }

    #[test]
    fn pending_initialization_is_shared_and_released_with_its_final_owner() {
        let changes = ToolListChanges::default();
        let owner = LegacyToolNotifications::default();
        owner.initialize(&changes);
        changes.publish();
        owner.initialize(&changes);
        let clone = owner.clone();
        drop(owner);
        assert_eq!(changes.receiver_count(), 1);
        let state = clone.0.lock();
        let Lifecycle::AwaitingInitialized(receiver) = &*state else {
            panic!("expected pending initialization")
        };
        assert!(
            receiver.has_changed().unwrap(),
            "reinitialization must preserve pending invalidation"
        );
        drop(state);
        drop(clone);
        assert_eq!(changes.receiver_count(), 0);
    }

    #[rstest::rstest]
    #[tokio::test]
    #[timeout(std::time::Duration::from_secs(10))]
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

    #[rstest::rstest]
    #[tokio::test]
    #[timeout(std::time::Duration::from_secs(10))]
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

    #[rstest::rstest]
    #[tokio::test]
    #[timeout(std::time::Duration::from_secs(10))]
    async fn other_errors_defensively_allow_later_changes() {
        // rmcp notification sends currently expose transport errors. Preserve
        // the fallback policy if other ServiceError variants become possible.
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
    #[rstest::rstest]
    #[tokio::test(start_paused = true)]
    #[timeout(std::time::Duration::from_secs(10))]
    async fn slow_client_recovers_without_delaying_another_client() {
        let changes = ToolListChanges::default();
        let shutdown = CancellationToken::new();
        let (started, mut attempts) = tokio::sync::mpsc::unbounded_channel();
        let slow = forward_changes(changes.subscribe(), shutdown.clone(), || {
            let (complete, completed) = tokio::sync::oneshot::channel();
            started.send(complete).unwrap();
            async move { completed.await.unwrap() }
        });
        let (sent, mut received) = tokio::sync::mpsc::unbounded_channel();
        let fast = forward_changes(changes.subscribe(), shutdown.clone(), || {
            sent.send(()).unwrap();
            std::future::ready(Ok(()))
        });
        tokio::pin!(slow, fast);
        changes.publish();
        assert!(futures::poll!(&mut slow).is_pending());
        let first_send = attempts.try_recv().unwrap();
        assert!(futures::poll!(&mut fast).is_pending());
        received.try_recv().unwrap();

        // Cross the former cutoff while the first send is still outstanding.
        tokio::time::advance(Duration::from_secs(6)).await;
        assert!(futures::poll!(&mut slow).is_pending());
        assert!(!first_send.is_closed());
        for _ in 0..3 {
            changes.publish();
            assert!(futures::poll!(&mut slow).is_pending());
            assert!(futures::poll!(&mut fast).is_pending());
            received.try_recv().unwrap();
        }
        assert!(attempts.try_recv().is_err());
        first_send.send(Ok(())).unwrap();
        assert!(futures::poll!(&mut slow).is_pending());
        // All changes during the stall become one subsequent send.
        attempts.try_recv().unwrap().send(Ok(())).unwrap();
        assert!(futures::poll!(&mut slow).is_pending());
        assert!(attempts.try_recv().is_err());
        shutdown.cancel();
        assert!(futures::poll!(&mut slow).is_ready());
        assert!(futures::poll!(&mut fast).is_ready());
        assert_eq!(changes.receiver_count(), 0);
    }

    #[rstest::rstest]
    #[case::closed(ServiceError::TransportClosed)]
    #[case::send(transport_send_error())]
    #[tokio::test]
    #[timeout(std::time::Duration::from_secs(10))]
    async fn transport_failures_end_delivery(#[case] error: ServiceError) {
        let changes = ToolListChanges::default();
        let mut failure = Some(error);
        let forwarding = forward_changes(changes.subscribe(), CancellationToken::new(), || {
            std::future::ready(Err(failure.take().expect("only one send is attempted")))
        });
        changes.publish();
        forwarding.await;
        assert_eq!(changes.receiver_count(), 0);
        changes.publish();
    }

    #[rstest::rstest]
    #[tokio::test]
    #[timeout(std::time::Duration::from_secs(10))]
    async fn dropping_the_change_source_ends_delivery() {
        let changes = ToolListChanges::default();
        let receiver = changes.subscribe();
        drop(changes);
        forward_changes(receiver, CancellationToken::new(), || async {
            panic!("closed source must not send a notification")
        })
        .await;
    }

    #[rstest::rstest]
    #[tokio::test]
    #[timeout(std::time::Duration::from_secs(10))]
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
