use std::{collections::HashMap, str::FromStr};

use apollo_parser::{
    Parser,
    cst::{Definition, OperationDefinition},
};
use serde::{Deserialize, Deserializer, Serialize};

use super::error::RoverClientError;

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct ApolloPersistedQueryManifest {
    pub operations: Vec<PersistedQueryOperation>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Ord, PartialOrd)]
#[serde(rename_all = "camelCase")]
pub struct PersistedQueryOperation {
    pub name: String,
    pub r#type: PersistedQueryOperationType,
    pub body: String,
    pub id: String,
    pub client_name: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Ord, PartialOrd)]
pub enum PersistedQueryOperationType {
    Query,
    Mutation,
    Subscription,
}

impl FromStr for PersistedQueryOperationType {
    type Err = RoverClientError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "query" => Ok(Self::Query),
            "mutation" => Ok(Self::Mutation),
            "subscription" => Ok(Self::Subscription),
            input => Err(RoverClientError::AdhocError {
                msg: format!(
                    "'{input}' is not a valid operation type. Must be one of: 'QUERY', 'MUTATION', or 'SUBSCRIPTION'."
                ),
            }),
        }
    }
}

impl<'de> Deserialize<'de> for PersistedQueryOperationType {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        FromStr::from_str(&s).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RelayPersistedQueryManifest {
    #[serde(flatten)]
    operations: HashMap<String, String>,
}

impl TryFrom<RelayPersistedQueryManifest> for ApolloPersistedQueryManifest {
    type Error = RoverClientError;

    fn try_from(relay_manifest: RelayPersistedQueryManifest) -> Result<Self, Self::Error> {
        let mut anonymous_operations = Vec::new();
        let mut syntax_errors = Vec::new();
        let mut ids_with_multiple_operations = Vec::new();
        let mut ids_with_no_operations = Vec::new();
        let mut operations = Vec::new();
        for (id, body) in relay_manifest.operations {
            let ast = Parser::new(&body).parse();

            let operation_definitions: Vec<OperationDefinition> = ast
                .clone()
                .document()
                .definitions()
                .filter_map(|definition| {
                    if let Definition::OperationDefinition(operation_definition) = definition {
                        Some(operation_definition)
                    } else {
                        None
                    }
                })
                .collect();

            let maybe_definition = match &operation_definitions[..] {
                [operation_definition] => Some(operation_definition),
                [] => {
                    ids_with_no_operations.push(id.clone());
                    None
                }
                _ => {
                    ids_with_multiple_operations.push(id.clone());
                    None
                }
            };

            if let Some(operation_definition) = maybe_definition {
                // attempt to extract operation type, defaulting to "query" if we can't find one
                let operation_type = match operation_definition.operation_type() {
                    Some(operation_type) => {
                        match (
                            operation_type.mutation_token(),
                            operation_type.query_token(),
                            operation_type.subscription_token(),
                        ) {
                            (Some(_mutation), _, _) => PersistedQueryOperationType::Mutation,
                            (_, Some(_query), _) => PersistedQueryOperationType::Query,
                            (_, _, Some(_subscription)) => {
                                PersistedQueryOperationType::Subscription
                            }
                            // this should probably be unreachable, but just default to query regardless
                            _ => PersistedQueryOperationType::Query,
                        }
                    }
                    None => PersistedQueryOperationType::Query,
                };

                // track valid operations and the IDs of invalid operations
                if let Some(operation_name) = operation_definition.name() {
                    operations.push(PersistedQueryOperation {
                        name: operation_name.text().to_string(),
                        r#type: operation_type,
                        body: body.to_string(),
                        id: id.to_string(),
                        // Relay format has no way to include client names in
                        // the manifest file; you can still use the
                        // `--for-client-name` flag.
                        client_name: None,
                    });
                } else {
                    // `apollo-parser` may sometimes be able to detect an operation name
                    // even if there are syntax errors
                    // we only report syntax errors when the operation name cannot be detected
                    // to relax GraphQL parsing as much as possible
                    let mut parse_errors = ast.errors().peekable();
                    if parse_errors.peek().is_some() {
                        syntax_errors.push((
                            id.clone(),
                            parse_errors
                                .map(|err| err.to_string())
                                .collect::<Vec<_>>()
                                .join("\n"),
                        ));
                    } else {
                        anonymous_operations.push(id.clone());
                    }
                }
            }
        }

        let mut errors = Vec::new();

        if !anonymous_operations.is_empty() {
            errors.push(format!(
                "The following operation IDs do not have a name: {}.",
                anonymous_operations.join(", ")
            ));
        }

        if !ids_with_multiple_operations.is_empty() {
            errors.push(format!(
                "The following operation IDs contained multiple operations: {}.",
                ids_with_multiple_operations.join(", ")
            ));
        }

        if !ids_with_no_operations.is_empty() {
            errors.push(format!(
                "The following operation IDs contained no operations: {}.",
                ids_with_no_operations.join(", ")
            ));
        }

        if !syntax_errors.is_empty() {
            for (id, syntax_errors) in syntax_errors {
                errors.push(format!("The operation with ID {id} contained the following syntax errors:\n\n{syntax_errors}"));
            }
        }

        if errors.is_empty() {
            let manifest = ApolloPersistedQueryManifest { operations };
            if let Ok(json) = serde_json::to_string(&manifest) {
                tracing::debug!(
                    json,
                    "successfully converted relay persisted query manifest to apollo persisted query manifest"
                );
            }
            Ok(manifest)
        } else {
            Err(RoverClientError::RelayOperationParseFailures {
                errors: errors.join("\n"),
            })
        }
    }
}
