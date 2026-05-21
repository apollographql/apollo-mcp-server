use std::sync::Arc;

use apollo_compiler::{Schema, validation::Valid};
use apollo_schema_index::SchemaIndex;
use reqwest::header::HeaderMap;
use tokio::sync::RwLock;
use url::Url;

use crate::custom_scalar_map::CustomScalarMap;
use crate::headers::ForwardHeaders;
use crate::operations::{MutationMode, Operation};

use super::credentials::CredentialProvider;

/// One graph's worth of state. Held by `Running` inside a `HashMap<String, GraphContext>`.
pub struct GraphContext {
    pub name: String,
    pub schema: Arc<RwLock<Valid<Schema>>>,
    pub endpoint: Url,
    pub headers: HeaderMap,
    pub forward_headers: ForwardHeaders,
    pub operations: Arc<RwLock<Vec<Operation>>>,
    pub search_index: SchemaIndex,
    pub mutation_mode: MutationMode,
    pub custom_scalar_map: Option<CustomScalarMap>,
    pub credentials: Arc<dyn CredentialProvider>,
}

impl std::fmt::Debug for GraphContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GraphContext")
            .field("name", &self.name)
            .field("endpoint", &self.endpoint)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graphs::credentials::default_provider;
    use apollo_schema_index::OperationType;

    fn parsed_schema() -> Valid<Schema> {
        Schema::parse("type Query { id: String }", "schema.graphql")
            .unwrap()
            .validate()
            .unwrap()
    }

    #[tokio::test]
    async fn it_constructs_a_context() {
        let schema = Arc::new(RwLock::new(parsed_schema()));
        let locked = schema.try_read().unwrap();
        let index = SchemaIndex::new(&locked, OperationType::Query.into(), 15_000_000).unwrap();
        drop(locked);

        let ctx = GraphContext {
            name: "g".into(),
            schema,
            endpoint: Url::parse("http://localhost:4000/").unwrap(),
            headers: HeaderMap::new(),
            forward_headers: vec![],
            operations: Arc::new(RwLock::new(vec![])),
            search_index: index,
            mutation_mode: MutationMode::None,
            custom_scalar_map: None,
            credentials: default_provider(),
        };

        assert_eq!(ctx.name, "g");
    }
}
