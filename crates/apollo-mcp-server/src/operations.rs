//! Operations
//!
//! This module includes transformation utilities that convert GraphQL operations
//! into MCP tools.

mod mutation_mode;
mod operation;
mod operation_source;
mod raw_operation;
mod schema_walker;

pub use mutation_mode::MutationMode;
pub use operation::{Operation, operation_defs, operation_name};
pub use operation_source::OperationSource;
pub use raw_operation::RawOperation;
