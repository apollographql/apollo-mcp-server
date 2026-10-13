use std::{
    collections::HashMap,
    ops::Deref,
    sync::{Arc, PoisonError, RwLock as SyncRwLock},
};

use rmcp::model::Tool;
use tokio::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

use crate::operations::Operation;

type ToolsByName = HashMap<String, Tool>;

/// The predefined operations, plus a snapshot of their tools for synchronous lookups.
///
/// rmcp checks `Mcp-Param-*` headers through the synchronous `ServerHandler::get_tool`,
/// which can't wait on the operations lock. In-flight tool calls hold that lock, so the
/// snapshot answers lookups without waiting, even while a reload is queued.
#[derive(Clone, Default)]
pub(super) struct OperationCatalog {
    operations: Arc<RwLock<Vec<Operation>>>,
    tools: Arc<SyncRwLock<ToolsByName>>,
}

impl OperationCatalog {
    pub(super) fn new(operations: Vec<Operation>) -> Self {
        Self {
            tools: Arc::new(SyncRwLock::new(tools_by_name(&operations))),
            operations: Arc::new(RwLock::new(operations)),
        }
    }

    pub(super) async fn read(&self) -> RwLockReadGuard<'_, Vec<Operation>> {
        self.operations.read().await
    }

    #[cfg(test)]
    pub(super) fn try_read(
        &self,
    ) -> Result<RwLockReadGuard<'_, Vec<Operation>>, tokio::sync::TryLockError> {
        self.operations.try_read()
    }

    /// Locks the catalog for an update. Hold the guard for the whole rebuild so that
    /// concurrent updates can't interleave.
    pub(super) async fn write(&self) -> CatalogWriteGuard<'_> {
        CatalogWriteGuard {
            operations: self.operations.write().await,
            tools: &self.tools,
        }
    }

    /// The tool for the operation named `name`, without waiting on the operations lock.
    pub(super) fn tool(&self, name: &str) -> Option<Tool> {
        self.tools
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(name)
            .cloned()
    }
}

pub(super) struct CatalogWriteGuard<'a> {
    operations: RwLockWriteGuard<'a, Vec<Operation>>,
    tools: &'a SyncRwLock<ToolsByName>,
}

impl CatalogWriteGuard<'_> {
    /// Replaces the operations and their tool snapshot, then releases the lock.
    pub(super) fn replace(mut self, operations: Vec<Operation>) {
        let tools = tools_by_name(&operations);
        *self.tools.write().unwrap_or_else(PoisonError::into_inner) = tools;
        *self.operations = operations;
    }
}

impl Deref for CatalogWriteGuard<'_> {
    type Target = [Operation];

    fn deref(&self) -> &Self::Target {
        &self.operations
    }
}

/// Maps each name to its first operation, the one `find_and_execute_operation` runs.
fn tools_by_name(operations: &[Operation]) -> ToolsByName {
    operations
        .iter()
        .rev()
        .map(|operation| {
            let tool = operation.as_ref();
            (tool.name.to_string(), tool.clone())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use apollo_compiler::Schema;

    use super::*;
    use crate::operations::{MutationMode, RawOperation};

    fn operation(source_text: &str) -> Operation {
        let schema = Schema::parse_and_validate("type Query { id: ID }", "schema.graphql").unwrap();
        RawOperation::from((source_text.to_string(), None))
            .into_operation(
                &schema,
                None,
                MutationMode::None,
                false,
                false,
                false,
                &HashMap::new(),
                &HashMap::new(),
                &HashMap::new(),
            )
            .unwrap()
            .unwrap()
    }

    #[test]
    fn tool_finds_operation_by_name() {
        let catalog = OperationCatalog::new(vec![operation("query GetId { id }")]);

        assert_eq!(
            catalog.tool("GetId").map(|tool| tool.name.to_string()),
            Some("GetId".to_string())
        );
    }

    #[test]
    fn tool_uses_first_operation_with_duplicate_name() {
        let catalog = OperationCatalog::new(vec![
            operation("# First\nquery GetId { id }"),
            operation("# Second\nquery GetId { id }"),
        ]);

        assert_eq!(
            catalog
                .tool("GetId")
                .and_then(|tool| tool.description.map(|description| description.to_string())),
            Some("First".to_string())
        );
    }

    #[tokio::test]
    async fn replace_updates_tool_snapshot() {
        let catalog = OperationCatalog::new(vec![operation("query Old { id }")]);

        catalog
            .write()
            .await
            .replace(vec![operation("query New { id }")]);

        assert!(catalog.tool("Old").is_none() && catalog.tool("New").is_some());
    }

    #[tokio::test]
    async fn tool_does_not_wait_for_queued_writer() {
        let catalog = OperationCatalog::new(vec![operation("query GetId { id }")]);
        let in_flight_call = catalog.read().await;
        let queued_reload = tokio::spawn({
            let catalog = catalog.clone();
            async move { catalog.write().await.replace(vec![]) }
        });
        tokio::task::yield_now().await;

        assert!(catalog.tool("GetId").is_some());

        drop(in_flight_call);
        queued_reload.await.unwrap();
    }
}
