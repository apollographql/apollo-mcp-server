use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, de};

/// Per-operation OAuth scope requirements.
///
/// A flat list keeps the existing "all scopes are required" behavior:
///
/// ```yaml
/// DeleteUser:
///   - user:write
///   - admin
/// ```
///
/// A nested list mirrors Apollo Router's `@requiresScopes` semantics: each
/// inner list is an AND group, and the outer list is OR.
///
/// ```yaml
/// GetUser:
///   - [user:read]
///   - [admin]
/// ```
#[derive(Clone, Debug, JsonSchema, PartialEq, Eq)]
pub enum OperationRequiredScopes {
    /// The token must contain every listed scope.
    All(Vec<String>),
    /// The token must satisfy at least one listed scope group.
    AnyOf(Vec<Vec<String>>),
}

#[derive(Deserialize)]
#[serde(untagged)]
enum OperationRequiredScopesDefinition {
    All(Vec<String>),
    AnyOf(Vec<Vec<String>>),
}

impl<'de> Deserialize<'de> for OperationRequiredScopes {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        match OperationRequiredScopesDefinition::deserialize(deserializer)? {
            OperationRequiredScopesDefinition::All(required) => {
                Ok(OperationRequiredScopes::All(required))
            }
            OperationRequiredScopesDefinition::AnyOf(alternatives) => {
                if alternatives.iter().any(Vec::is_empty) {
                    return Err(de::Error::custom(
                        "required_scopes alternatives must not contain empty scope groups",
                    ));
                }
                Ok(OperationRequiredScopes::AnyOf(alternatives))
            }
        }
    }
}

impl OperationRequiredScopes {
    /// Returns true when the present token scopes satisfy this requirement.
    pub fn is_satisfied_by(&self, present: &[String]) -> bool {
        match self {
            OperationRequiredScopes::All(required) => {
                required.iter().all(|req| present.contains(req))
            }
            OperationRequiredScopes::AnyOf(alternatives) => alternatives
                .iter()
                .any(|required| required.iter().all(|req| present.contains(req))),
        }
    }

    /// Scopes to include in `WWW-Authenticate`.
    ///
    /// The OAuth bearer `scope` auth-param is a space-delimited list and cannot
    /// represent grouped OR conditions. For alternatives, return the complete
    /// group that requires the fewest additional scopes for this token.
    pub fn challenge_scopes(&self, present: &[String]) -> Vec<String> {
        match self {
            OperationRequiredScopes::All(required) => required.clone(),
            OperationRequiredScopes::AnyOf(alternatives) => alternatives
                .iter()
                .min_by_key(|required| missing_scope_count(required, present))
                .cloned()
                .unwrap_or_default(),
        }
    }
}

fn missing_scope_count(required: &[String], present: &[String]) -> usize {
    required
        .iter()
        .filter(|scope| !present.contains(*scope))
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scopes(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn flat_scopes_require_all_values() {
        let required = OperationRequiredScopes::All(scopes(&["read", "write"]));

        assert!(required.is_satisfied_by(&scopes(&["read", "write", "admin"])));
        assert!(!required.is_satisfied_by(&scopes(&["read"])));
    }

    #[test]
    fn nested_scopes_allow_any_satisfied_group() {
        let required =
            OperationRequiredScopes::AnyOf(vec![scopes(&["read", "write"]), scopes(&["admin"])]);

        assert!(required.is_satisfied_by(&scopes(&["read", "write"])));
        assert!(required.is_satisfied_by(&scopes(&["admin"])));
        assert!(!required.is_satisfied_by(&scopes(&["read"])));
    }

    #[test]
    fn challenge_scopes_returns_best_matching_alternative() {
        let required =
            OperationRequiredScopes::AnyOf(vec![scopes(&["read", "write"]), scopes(&["admin"])]);

        assert_eq!(
            required.challenge_scopes(&scopes(&["read"])),
            scopes(&["read", "write"])
        );
        assert_eq!(required.challenge_scopes(&[]), scopes(&["admin"]));
    }

    #[test]
    fn nested_scopes_reject_empty_alternatives() {
        let result = serde_json::from_value::<OperationRequiredScopes>(serde_json::json!([[]]));

        assert!(result.is_err());
    }
}
