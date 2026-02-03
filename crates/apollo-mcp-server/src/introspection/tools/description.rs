use std::borrow::Cow;

pub(crate) fn append_description_hint<'a>(default: &'a str, hint: Option<&str>) -> Cow<'a, str> {
    match hint.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then_some(trimmed)
    }) {
        Some(hint) => Cow::Owned(format!("{default}\nHint: {hint}")),
        None => Cow::Borrowed(default),
    }
}

#[cfg(test)]
mod tests {
    use super::append_description_hint;

    #[test]
    fn returns_default_when_hint_is_none() {
        let result = append_description_hint("Default", None);
        assert_eq!(result, "Default");
    }

    #[test]
    fn returns_default_when_hint_is_whitespace() {
        let result = append_description_hint("Default", Some("   \n\t"));
        assert_eq!(result, "Default");
    }

    #[test]
    fn appends_additional_instructions_when_hint_is_present() {
        let result =
            append_description_hint("Default", Some("Use carts(where: { status: ACTIVE })"));
        assert_eq!(
            result,
            "Default\nHint: Use carts(where: { status: ACTIVE })"
        );
    }
}
