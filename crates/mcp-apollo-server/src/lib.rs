#![cfg_attr(
    not(test),
    deny(
        clippy::exit,
        // clippy::panic, - TODO: fix existing cases
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
    )
)]

pub mod errors;
pub mod operations;
pub mod sanitize;
pub mod server;
pub(crate) mod tree_shake;

use operations::Operation;
pub type OperationsList = Vec<Operation>;

pub use rover_client::operations::persisted_queries::publish::{
    ApolloPersistedQueryManifest, RelayPersistedQueryManifest,
};
