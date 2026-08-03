use rmcp::model::{Icon, IconTheme};
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer};

/// Server metadata configuration returned in the MCP initialize response.
/// All fields are optional and fall back to defaults if not provided.
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct ServerInfoConfig {
    /// The name of the MCP server implementation
    pub name: Option<String>,

    /// The version of the MCP server implementation
    pub version: Option<String>,

    /// Human-readable title for the server
    pub title: Option<String>,

    /// URL to the server's website or documentation
    pub website_url: Option<String>,

    /// A brief description of the server
    pub description: Option<String>,

    /// Icons representing the server, for clients that display one
    pub icons: Vec<IconConfig>,
}

/// A single icon representing the server. Clients pick whichever entry best
/// fits the surface they are rendering, so several may be supplied.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct IconConfig {
    /// URI of the icon: an `https://` URL or a base64-encoded `data:` URI
    #[serde(deserialize_with = "deserialize_icon_src")]
    pub src: String,

    /// MIME type of the icon, such as `image/png`. Set this when the source
    /// serves no MIME type of its own or serves a generic one
    #[serde(default)]
    pub mime_type: Option<String>,

    /// Sizes the icon is available in, each `WxH` (e.g. `48x48`) or `any` for
    /// scalable formats
    #[serde(default, deserialize_with = "deserialize_icon_sizes")]
    pub sizes: Option<Vec<String>>,

    /// Background theme this icon is designed for. Omit to let the client use
    /// it with any theme
    #[serde(default)]
    pub theme: Option<IconThemeConfig>,
}

/// Reject `src` values whose scheme the MCP spec instructs clients to refuse
/// (`javascript:`, `file:`, `ftp:`, `ws:`, and anything else that isn't
/// `https://` or `data:`), and reject well-schemed but empty inputs like a
/// bare `https://` or a `data:` URI without an `image/*` payload.
fn deserialize_icon_src<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let raw = String::deserialize(deserializer)?;
    let ok = if let Some(rest) = raw.strip_prefix("https://") {
        has_host(rest)
    } else if let Some(rest) = raw.strip_prefix("data:") {
        is_image_data_uri(rest)
    } else {
        false
    };
    if ok {
        Ok(raw)
    } else {
        Err(serde::de::Error::custom(format!(
            "server_info.icons[].src must be an `https://` URL with a host or an \
             `image/*` `data:` URI (got {raw:?})"
        )))
    }
}

fn has_host(after_scheme: &str) -> bool {
    let host_end = after_scheme
        .find(['/', '?', '#'])
        .unwrap_or(after_scheme.len());
    !after_scheme[..host_end].is_empty()
}

/// A `data:` URI is `mediatype[;base64],data`. Accept it only when the media
/// type is `image/<subtype>` and the payload is non-empty; the RFC allows
/// omitting either but neither shape makes sense for an icon source.
fn is_image_data_uri(after_scheme: &str) -> bool {
    let (metadata, data) = match after_scheme.split_once(',') {
        Some(pair) => pair,
        None => return false,
    };
    if data.is_empty() {
        return false;
    }
    let mime = metadata.split(';').next().unwrap_or("");
    match mime.strip_prefix("image/") {
        Some(subtype) => !subtype.is_empty(),
        None => false,
    }
}

fn deserialize_icon_sizes<'de, D>(deserializer: D) -> Result<Option<Vec<String>>, D::Error>
where
    D: Deserializer<'de>,
{
    let raw = Option::<Vec<String>>::deserialize(deserializer)?;
    if let Some(entries) = &raw {
        for entry in entries {
            if entry != "any" && !is_wxh_size(entry) {
                return Err(serde::de::Error::custom(format!(
                    "server_info.icons[].sizes entries must be `WxH` (e.g. `48x48`) or `any` (got {entry:?})"
                )));
            }
        }
    }
    Ok(raw)
}

fn is_wxh_size(entry: &str) -> bool {
    match entry.split_once('x') {
        Some((width, height)) => {
            !width.is_empty()
                && !height.is_empty()
                && width.bytes().all(|b| b.is_ascii_digit())
                && height.bytes().all(|b| b.is_ascii_digit())
        }
        None => false,
    }
}

/// Background theme an icon is designed for.
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum IconThemeConfig {
    Light,
    Dark,
}

impl From<IconThemeConfig> for IconTheme {
    fn from(theme: IconThemeConfig) -> Self {
        match theme {
            IconThemeConfig::Light => IconTheme::Light,
            IconThemeConfig::Dark => IconTheme::Dark,
        }
    }
}

impl From<&IconConfig> for Icon {
    fn from(config: &IconConfig) -> Self {
        let mut icon = Icon::new(config.src.clone());
        if let Some(mime_type) = config.mime_type.as_deref() {
            icon = icon.with_mime_type(mime_type);
        }
        if let Some(sizes) = config.sizes.clone() {
            icon = icon.with_sizes(sizes);
        }
        if let Some(theme) = config.theme {
            icon = icon.with_theme(theme.into());
        }
        icon
    }
}

impl ServerInfoConfig {
    pub fn name(&self) -> &str {
        self.name.as_deref().unwrap_or("Apollo MCP Server")
    }

    pub fn version(&self) -> &str {
        self.version.as_deref().unwrap_or(env!("CARGO_PKG_VERSION"))
    }

    pub fn title(&self) -> Option<&str> {
        self.title.as_deref().or(Some("Apollo MCP Server"))
    }

    pub fn website_url(&self) -> Option<&str> {
        self.website_url
            .as_deref()
            .or(Some("https://www.apollographql.com/docs/apollo-mcp-server"))
    }

    pub fn description(&self) -> Option<&str> {
        self.description.as_deref().or(Some(
            "A Model Context Protocol (MCP) server for exposing GraphQL APIs as tools.",
        ))
    }

    /// No icon is advertised unless one is configured; there is no sensible
    /// default to brand an embedding server with.
    pub fn icons(&self) -> Option<Vec<Icon>> {
        (!self.icons.is_empty()).then(|| self.icons.iter().map(Icon::from).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icons_is_none_when_no_entries_configured() {
        assert!(ServerInfoConfig::default().icons().is_none());
    }

    #[test]
    fn icons_forwards_optional_fields() {
        let config = ServerInfoConfig {
            icons: vec![IconConfig {
                src: "https://example.com/icon.png".to_string(),
                mime_type: Some("image/png".to_string()),
                sizes: Some(vec!["48x48".to_string(), "96x96".to_string()]),
                theme: Some(IconThemeConfig::Light),
            }],
            ..Default::default()
        };

        let icons = config.icons().expect("configured icons should surface");
        assert_eq!(icons.len(), 1);
        assert_eq!(icons[0].src, "https://example.com/icon.png");
        assert_eq!(icons[0].mime_type.as_deref(), Some("image/png"));
        assert_eq!(
            icons[0].sizes.as_deref(),
            Some(&["48x48".to_string(), "96x96".to_string()][..]),
        );
        assert_eq!(icons[0].theme, Some(IconTheme::Light));
    }

    #[test]
    fn icons_can_be_deserialized_from_yaml_with_minimal_fields() {
        let yaml = r#"
icons:
  - src: "https://example.com/mark.svg"
"#;
        let config: ServerInfoConfig = serde_yaml::from_str(yaml).unwrap();

        let icons = config.icons().expect("configured icons should surface");
        assert_eq!(icons.len(), 1);
        assert_eq!(icons[0].src, "https://example.com/mark.svg");
        assert!(icons[0].mime_type.is_none());
        assert!(icons[0].sizes.is_none());
        assert!(icons[0].theme.is_none());
    }

    #[test]
    fn icons_accept_https_and_data_sources() {
        let yaml = r#"
icons:
  - src: "https://example.com/mark.svg"
  - src: "data:image/png;base64,iVBORw0KGgo="
  - src: "data:image/svg+xml,%3Csvg/%3E"
"#;
        let config: ServerInfoConfig = serde_yaml::from_str(yaml).unwrap();

        assert_eq!(config.icons.len(), 3);
    }

    #[test]
    fn icons_reject_unsafe_src_schemes() {
        for src in [
            "http://example.com/icon.png",
            "javascript:alert(1)",
            "file:///etc/passwd",
            "ftp://example.com/icon.png",
            "ws://example.com/icon",
            "",
            "example.com/icon.png",
        ] {
            let yaml = format!("icons:\n  - src: \"{src}\"\n");
            let err = serde_yaml::from_str::<ServerInfoConfig>(&yaml)
                .expect_err(&format!("scheme {src:?} should be rejected"));
            assert!(
                err.to_string()
                    .contains("must be an `https://` URL with a host"),
                "unexpected error for {src:?}: {err}"
            );
        }
    }

    #[test]
    fn icons_reject_malformed_https_and_data_sources() {
        for src in [
            "https://",
            "https:///path",
            "https://?query",
            "data:",
            "data:,",
            "data:image/png,",
            "data:text/plain;base64,Zm9v",
            "data:image/;base64,AAAA",
            "data:base64,AAAA",
        ] {
            let yaml = format!("icons:\n  - src: \"{src}\"\n");
            let err = serde_yaml::from_str::<ServerInfoConfig>(&yaml)
                .expect_err(&format!("malformed src {src:?} should be rejected"));
            assert!(
                err.to_string()
                    .contains("must be an `https://` URL with a host"),
                "unexpected error for {src:?}: {err}"
            );
        }
    }

    #[test]
    fn icons_accept_wxh_and_any_sizes() {
        let yaml = r#"
icons:
  - src: "https://example.com/mark.svg"
    sizes: ["48x48", "1024x1024", "any"]
"#;
        let config: ServerInfoConfig = serde_yaml::from_str(yaml).unwrap();

        assert_eq!(
            config.icons[0].sizes.as_deref(),
            Some(
                &[
                    "48x48".to_string(),
                    "1024x1024".to_string(),
                    "any".to_string()
                ][..]
            ),
        );
    }

    #[test]
    fn icons_reject_malformed_size_entries() {
        for size in ["48x", "x48", "fooxbar", "48X48", "48", "", "48x48x48"] {
            let yaml = format!(
                "icons:\n  - src: \"https://example.com/icon.svg\"\n    sizes: [\"{size}\"]\n"
            );
            let err = serde_yaml::from_str::<ServerInfoConfig>(&yaml)
                .expect_err(&format!("size {size:?} should be rejected"));
            assert!(
                err.to_string().contains("must be `WxH`"),
                "unexpected error for {size:?}: {err}"
            );
        }
    }
}
