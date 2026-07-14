//! Derivation of an operation's service scope.
//!
//! SHORT-TERM: scope comes from the operation-name prefix (`slack_userByEmail` -> `slack`).
//! FUTURE: this is the single seam to change when scope comes from subgraph schemas /
//! federation metadata instead of a name prefix — callers depend only on `derive_scope`.

/// Split an operation name into `(scope, bare_name)` on the first `_`.
/// No underscore => no scope, and the bare name is the whole name.
pub(crate) fn derive_scope(operation_name: &str) -> (Option<&str>, &str) {
    match operation_name.split_once('_') {
        Some((prefix, rest)) if !prefix.is_empty() && !rest.is_empty() => (Some(prefix), rest),
        _ => (None, operation_name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_on_first_underscore() {
        assert_eq!(
            derive_scope("slack_userByEmail"),
            (Some("slack"), "userByEmail")
        );
    }
    #[test]
    fn multiple_underscores_keep_only_first_as_scope() {
        assert_eq!(derive_scope("a_b_c"), (Some("a"), "b_c"));
    }
    #[test]
    fn no_underscore_means_no_scope() {
        assert_eq!(derive_scope("userByEmail"), (None, "userByEmail"));
    }
    #[test]
    fn leading_or_lone_underscore_is_not_a_scope() {
        assert_eq!(derive_scope("_x"), (None, "_x"));
        assert_eq!(derive_scope("x_"), (None, "x_"));
    }
}
