use std::sync::Arc;

use parking_lot::Mutex;
use rhai::{CustomType, Dynamic, Engine, EvalAltResult, Position, TypeBuilder};
use tokio::sync::oneshot;

#[derive(Clone, Debug)]
pub enum PromiseState {
    Pending,
    Resolved,
    Rejected,
}

type PromiseReceiver = Arc<Mutex<Option<oneshot::Receiver<Result<Dynamic, String>>>>>;

#[derive(Clone, Debug, CustomType)]
pub struct Promise {
    pub state: PromiseState,
    pub resolved_value: Option<Dynamic>,
    pub receiver: PromiseReceiver,
}

impl Promise {
    pub fn register(engine: &mut Engine) {
        engine
            .register_type::<Promise>()
            .register_fn("to_string", Promise::to_string)
            .register_fn("to_debug", Promise::to_debug)
            .register_fn("wait", Promise::resolve);
    }

    pub fn resolve(promise: &mut Self) -> Result<Dynamic, Box<EvalAltResult>> {
        if matches!(promise.state, PromiseState::Resolved)
            || matches!(promise.state, PromiseState::Rejected)
        {
            return match &promise.resolved_value {
                Some(value) => Ok(value.clone()),
                None => Err(
                    "Unexpected state: Promise was resolved or rejected but contained no resolved value.".into(),
                ),
            };
        }

        let receiver = promise
            .receiver
            .lock()
            .take()
            .ok_or_else(|| -> Box<EvalAltResult> {
                "Unexpected state: Promise was pending but no async task was found".into()
            })?;

        let result =
            tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(receiver));

        match result {
            Ok(Ok(value)) => {
                promise.state = PromiseState::Resolved;
                promise.resolved_value = Some(value.clone());
                Ok(value)
            }
            Ok(Err(err)) => {
                promise.state = PromiseState::Rejected;
                promise.resolved_value = Some(err.clone().into());
                Err(err.into())
            }
            Err(_recv_err) => {
                promise.state = PromiseState::Rejected;
                Err("Unexpected state: Promise task was dropped".into())
            }
        }
    }

    pub fn to_string(promise: &mut Self) -> String {
        format!("Promise {{ state: {:?} }}  ", promise.state)
    }

    pub fn to_debug(promise: &mut Self) -> String {
        format!("Promise {{ state: {:?} }}  ", promise.state)
    }
}
