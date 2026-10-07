use std::borrow::Cow;
use std::collections::HashMap;

/// Returns the granted scopes plus every scope they imply, following
/// `implied` transitively.
///
/// Granted scopes come first, in token order, followed by implied scopes in
/// discovery order, without duplicates. Cycles in `implied` terminate because
/// a scope already in the result is never expanded again.
pub(super) fn with_implied_scopes<'a>(
    granted: &'a [String],
    implied: &HashMap<String, Vec<String>>,
) -> Cow<'a, [String]> {
    if implied.is_empty() {
        return Cow::Borrowed(granted);
    }

    let mut scopes = granted.to_vec();
    let mut next = 0;
    while let Some(scope) = scopes.get(next) {
        if let Some(narrower) = implied.get(scope) {
            for implied_scope in narrower {
                if !scopes.contains(implied_scope) {
                    scopes.push(implied_scope.clone());
                }
            }
        }
        next += 1;
    }
    Cow::Owned(scopes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| v.to_string()).collect()
    }

    fn implied(entries: &[(&str, &[&str])]) -> HashMap<String, Vec<String>> {
        entries
            .iter()
            .map(|(broader, narrower)| (broader.to_string(), s(narrower)))
            .collect()
    }

    #[test]
    fn borrows_granted_scopes_when_nothing_is_implied() {
        let granted = s(&["read"]);

        let scopes = with_implied_scopes(&granted, &HashMap::new());

        assert!(matches!(scopes, Cow::Borrowed(_)), "got: {scopes:?}");
    }

    #[test]
    fn adds_directly_implied_scopes_after_granted_ones() {
        let granted = s(&["admin"]);
        let implied = implied(&[("admin", &["read", "write"])]);

        let scopes = with_implied_scopes(&granted, &implied);

        assert_eq!(scopes.as_ref(), s(&["admin", "read", "write"]).as_slice());
    }

    #[test]
    fn follows_implications_transitively() {
        let granted = s(&["admin"]);
        let implied = implied(&[("admin", &["write"]), ("write", &["read"])]);

        let scopes = with_implied_scopes(&granted, &implied);

        assert_eq!(scopes.as_ref(), s(&["admin", "write", "read"]).as_slice());
    }

    #[test]
    fn terminates_on_cycles() {
        let granted = s(&["a"]);
        let implied = implied(&[("a", &["b"]), ("b", &["a"])]);

        let scopes = with_implied_scopes(&granted, &implied);

        assert_eq!(scopes.as_ref(), s(&["a", "b"]).as_slice());
    }

    #[test]
    fn does_not_duplicate_scopes_the_token_already_has() {
        let granted = s(&["read", "admin"]);
        let implied = implied(&[("admin", &["read", "write"])]);

        let scopes = with_implied_scopes(&granted, &implied);

        assert_eq!(scopes.as_ref(), s(&["read", "admin", "write"]).as_slice());
    }

    #[test]
    fn narrower_scopes_do_not_imply_broader_ones() {
        let granted = s(&["read"]);
        let implied = implied(&[("admin", &["read"])]);

        let scopes = with_implied_scopes(&granted, &implied);

        assert_eq!(scopes.as_ref(), s(&["read"]).as_slice());
    }
}
