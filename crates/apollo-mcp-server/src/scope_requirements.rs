use std::borrow::Cow;
use std::collections::HashMap;

use schemars::{JsonSchema, Schema, SchemaGenerator};
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
///
/// Always holds at least one group, and every group holds at least one
/// scope: [`new`](Self::new) and `Deserialize` both reject anything else, so
/// a value of this type can never be vacuously satisfied by every token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OperationRequiredScopes(Vec<Vec<String>>);

/// Error constructing [`OperationRequiredScopes`] from invalid scope groups.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum InvalidScopeRequirement {
    #[error("required_scopes must have at least one scope group")]
    NoGroups,
    #[error("required_scopes alternatives must not contain empty scope groups")]
    EmptyGroup,
}

impl OperationRequiredScopes {
    /// Builds a scope requirement from one or more alternative scope groups
    /// (OR of AND). Rejects zero groups and any empty group, so it's
    /// impossible to construct a value that's vacuously satisfied by every
    /// token, whether from config or directly through this API.
    pub fn new(groups: Vec<Vec<String>>) -> Result<Self, InvalidScopeRequirement> {
        if groups.is_empty() {
            return Err(InvalidScopeRequirement::NoGroups);
        }
        if groups.iter().any(Vec::is_empty) {
            return Err(InvalidScopeRequirement::EmptyGroup);
        }
        Ok(Self(groups))
    }

    /// Returns true when the present token scopes satisfy this requirement.
    pub fn is_satisfied_by(&self, present: &[String]) -> bool {
        self.0
            .iter()
            .any(|group| group.iter().all(|req| present.contains(req)))
    }

    /// Scopes to include in `WWW-Authenticate`.
    ///
    /// The OAuth bearer `scope` auth-param is a space-delimited list and cannot
    /// represent grouped OR conditions. Returns the complete group that
    /// requires the fewest additional scopes for this token; ties go to the
    /// first listed alternative.
    pub fn challenge_scopes(&self, present: &[String]) -> Vec<String> {
        #[allow(clippy::expect_used)] // `new` guarantees at least one group
        self.0
            .iter()
            .min_by_key(|group| missing_scope_count(group, present))
            .cloned()
            .expect("OperationRequiredScopes always has at least one group")
    }
}

/// Per-operation OAuth scope requirements keyed by operation name.
///
/// Existing builder callers can continue passing `HashMap<String, Vec<String>>`,
/// which preserves the flat "all scopes are required" behavior. Parsed config
/// can pass `HashMap<String, OperationRequiredScopes>` to include nested
/// alternatives.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OperationScopeRequirements(HashMap<String, OperationRequiredScopes>);

impl OperationScopeRequirements {
    pub(crate) fn into_inner(self) -> HashMap<String, OperationRequiredScopes> {
        self.0
    }
}

impl From<HashMap<String, Vec<String>>> for OperationScopeRequirements {
    fn from(required_scopes: HashMap<String, Vec<String>>) -> Self {
        let required_scopes = required_scopes
            .into_iter()
            // An empty scope list means "no requirement," which is exactly
            // the behavior of leaving the operation out of the map entirely.
            // Construct nothing rather than a vacuously satisfied
            // `OperationRequiredScopes`.
            .filter(|(_, scopes)| !scopes.is_empty())
            .map(|(operation, scopes)| {
                #[allow(clippy::expect_used)] // filtered to non-empty above
                let required = OperationRequiredScopes::new(vec![scopes])
                    .expect("a single non-empty scope group is always valid");
                (operation, required)
            })
            .collect();
        Self(required_scopes)
    }
}

impl From<HashMap<String, OperationRequiredScopes>> for OperationScopeRequirements {
    fn from(required_scopes: HashMap<String, OperationRequiredScopes>) -> Self {
        Self(required_scopes)
    }
}

/// Untagged wire shape for [`OperationRequiredScopes`], shared between
/// `Deserialize` and `JsonSchema` so the generated schema and the parser
/// can't drift apart.
#[derive(Deserialize, JsonSchema)]
#[serde(untagged)]
enum OperationRequiredScopesDefinition {
    All(#[schemars(length(min = 1))] Vec<String>),
    AnyOf(#[schemars(length(min = 1))] Vec<Vec<String>>),
}

impl<'de> Deserialize<'de> for OperationRequiredScopes {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let definition =
            OperationRequiredScopesDefinition::deserialize(deserializer).map_err(|_| {
                de::Error::custom(
                    "required_scopes entries must be a list of scopes, or a list of scope lists",
                )
            })?;
        let groups = match definition {
            OperationRequiredScopesDefinition::All(scopes) => vec![scopes],
            OperationRequiredScopesDefinition::AnyOf(alternatives) => alternatives,
        };
        OperationRequiredScopes::new(groups).map_err(de::Error::custom)
    }
}

impl JsonSchema for OperationRequiredScopes {
    fn schema_name() -> Cow<'static, str> {
        "OperationRequiredScopes".into()
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        OperationRequiredScopesDefinition::json_schema(generator)
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
    use crate::schema_from_type;
    use serde_json::Value;

    fn scopes(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    fn required(groups: Vec<Vec<String>>) -> OperationRequiredScopes {
        OperationRequiredScopes::new(groups).expect("valid groups in test fixture")
    }

    #[test]
    fn flat_scopes_require_all_values() {
        let required = required(vec![scopes(&["read", "write"])]);

        assert!(required.is_satisfied_by(&scopes(&["read", "write", "admin"])));
        assert!(!required.is_satisfied_by(&scopes(&["read"])));
    }

    #[test]
    fn flat_scope_map_converts_to_operation_requirements() {
        let converted = OperationScopeRequirements::from(HashMap::from([(
            "GetUser".to_string(),
            scopes(&["read", "write"]),
        )]))
        .into_inner();

        assert_eq!(
            converted.get("GetUser"),
            Some(&required(vec![scopes(&["read", "write"])]))
        );
    }

    #[test]
    fn empty_flat_scope_list_is_dropped_not_stored_as_vacuous() {
        let converted = OperationScopeRequirements::from(HashMap::from([(
            "PublicOp".to_string(),
            Vec::<String>::new(),
        )]))
        .into_inner();

        assert!(
            converted.get("PublicOp").is_none(),
            "an empty scope list must not produce a stored, vacuously-satisfied requirement"
        );
    }

    #[test]
    fn nested_scopes_allow_any_satisfied_group() {
        let required = required(vec![scopes(&["read", "write"]), scopes(&["admin"])]);

        assert!(required.is_satisfied_by(&scopes(&["read", "write"])));
        assert!(required.is_satisfied_by(&scopes(&["admin"])));
        assert!(!required.is_satisfied_by(&scopes(&["read"])));
    }

    #[test]
    fn challenge_scopes_returns_best_matching_alternative() {
        let required = required(vec![scopes(&["read", "write"]), scopes(&["admin"])]);

        assert_eq!(
            required.challenge_scopes(&scopes(&["read"])),
            scopes(&["read", "write"])
        );
        assert_eq!(required.challenge_scopes(&[]), scopes(&["admin"]));
    }

    #[test]
    fn challenge_scopes_ties_go_to_the_first_listed_alternative() {
        let required = required(vec![scopes(&["a", "b"]), scopes(&["c", "d"])]);

        // Neither group is present at all, so both are tied at "2 missing."
        assert_eq!(required.challenge_scopes(&[]), scopes(&["a", "b"]));
    }

    #[test]
    fn new_rejects_zero_groups() {
        let error = OperationRequiredScopes::new(vec![]).unwrap_err();
        assert_eq!(error, InvalidScopeRequirement::NoGroups);
    }

    #[test]
    fn new_rejects_an_empty_group() {
        let error = OperationRequiredScopes::new(vec![scopes(&["read"]), vec![]]).unwrap_err();
        assert_eq!(error, InvalidScopeRequirement::EmptyGroup);
    }

    #[test]
    fn nested_scopes_reject_empty_alternatives() {
        let result = serde_json::from_value::<OperationRequiredScopes>(serde_json::json!([[]]));

        assert!(result.is_err());
    }

    #[test]
    fn flat_empty_list_is_rejected_not_treated_as_no_requirement() {
        // Serde's untagged resolution tries `All(Vec<String>)` before
        // `AnyOf`, so a bare `[]` must be rejected here too - not just the
        // nested `[[]]` form - or `DeleteUser: []` would silently mean "no
        // scopes required."
        let result = serde_json::from_value::<OperationRequiredScopes>(serde_json::json!([]));

        assert!(result.is_err());
    }

    #[test]
    fn malformed_entry_reports_a_clear_error() {
        let result =
            serde_json::from_value::<OperationRequiredScopes>(serde_json::json!(["admin", 5]));

        let error = result.expect_err("mixed string/number entries are not a valid shape");
        assert!(
            error.to_string().contains(
                "required_scopes entries must be a list of scopes, or a list of scope lists"
            ),
            "got: {error}"
        );
    }

    #[test]
    fn generated_schema_accepts_documented_yaml_examples() {
        let schema = serde_json::Value::Object(schema_from_type!(OperationRequiredScopes));

        let flat = serde_json::json!(["user:write", "admin"]);
        assert!(
            jsonschema::is_valid(&schema, &flat),
            "flat example should validate against the generated schema"
        );

        let nested = serde_json::json!([["user:read"], ["admin"]]);
        assert!(
            jsonschema::is_valid(&schema, &nested),
            "nested example should validate against the generated schema"
        );

        let empty = serde_json::json!([]);
        assert!(
            !jsonschema::is_valid(&schema, &empty),
            "the parser rejects an empty list, so the schema should reject it too"
        );

        // Known, accepted gap: `length(min = 1)` on the outer `Vec<Vec<String>>`
        // stops a schema author from writing zero alternatives, but it doesn't
        // reach inside to constrain each alternative's own length. Catching
        // that too would need a dedicated newtype for a scope group; the real
        // enforcement for this case lives in `OperationRequiredScopes::new`
        // regardless, so this is editor-hinting only, not a security gap.
        let empty_inner_group = serde_json::json!([[]]);
        assert!(
            jsonschema::is_valid(&schema, &empty_inner_group),
            "documenting a known schema limitation, not asserting desired behavior"
        );
    }
}
