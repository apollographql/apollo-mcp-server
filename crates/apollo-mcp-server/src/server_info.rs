use rmcp::model::{Icon, IconTheme};
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer};
use url::Url;

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
    /// URI of the icon, such as an `https://` URL or a base64-encoded `data:` URI.
    /// Not `Url`: the value is advertised to clients verbatim, and `Url::parse`
    /// normalizes its input
    #[serde(deserialize_with = "deserialize_icon_src")]
    #[schemars(with = "Url")]
    pub src: String,

    /// MIME type of the icon, such as `image/png`. Set this when the source
    /// serves no MIME type of its own or serves a generic one
    #[serde(default)]
    pub mime_type: Option<String>,

    /// Sizes the icon is available in, each `WxH` (e.g. `48x48`) or `any` for
    /// scalable formats
    #[serde(default)]
    pub sizes: Option<Vec<String>>,

    /// Background theme this icon is designed for. Omit to let the client use
    /// it with any theme
    #[serde(default)]
    pub theme: Option<IconThemeConfig>,
}

/// `Icon.src` is `@format uri` in the MCP schema, so the one thing it has to do
/// is parse as a URI.
fn deserialize_icon_src<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de::Error;
    let src = String::deserialize(deserializer)?;
    Url::parse(&src).map_err(|e| D::Error::custom(format!("invalid icon src {src:?}: {e}")))?;
    Ok(src)
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
    fn icons_preserve_src_verbatim() {
        let yaml = r#"
icons:
  - src: "https://example.com"
"#;
        let config: ServerInfoConfig = serde_yaml::from_str(yaml).unwrap();

        assert_eq!(config.icons[0].src, "https://example.com");
    }

    #[test]
    fn icons_reject_src_that_is_not_a_uri() {
        for src in ["", "example.com/icon.png", "/icons/mark.svg", "https://"] {
            let yaml = format!("icons:\n  - src: \"{src}\"\n");
            let err = serde_yaml::from_str::<ServerInfoConfig>(&yaml)
                .expect_err(&format!("src {src:?} should be rejected"));
            assert!(
                err.to_string().contains("invalid icon src"),
                "unexpected error for {src:?}: {err}"
            );
        }
    }

    #[test]
    fn icons_pass_sizes_through_unchanged() {
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
    fn icons_reject_sizes_that_are_not_an_array_of_strings() {
        let yaml = "icons:\n  - src: \"https://example.com/mark.svg\"\n    sizes: \"48x48\"\n";

        assert!(serde_yaml::from_str::<ServerInfoConfig>(yaml).is_err());
    }
}
