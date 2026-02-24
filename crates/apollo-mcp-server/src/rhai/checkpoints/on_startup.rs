use std::sync::Arc;

use parking_lot::Mutex;
use tracing::error;

use crate::{errors::ServerError, rhai::engine::RhaiEngine};

pub fn on_startup(engine: &Arc<Mutex<RhaiEngine>>) -> Result<(), ServerError> {
    engine
        .lock()
        .execute_hook("on_startup", ())
        .map_err(|err| {
            error!("Error when executing on_startup hook: {err}");
            ServerError::RhaiError
        })?;
    Ok(())
}
