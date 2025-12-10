//! Operations
//!
//! This module includes transformation utilities that convert GraphQL operations
//! into MCP tools.

mod execution;
mod mutation_mode;
mod operation;
mod operation_source;
mod raw_operation;
mod schema_walker;

pub(crate) use execution::{execute_operation, find_and_execute_operation};
pub use mutation_mode::MutationMode;
pub(crate) use operation::{Operation, operation_defs, operation_name};
pub use operation_source::OperationSource;
pub(crate) use raw_operation::RawOperation;
