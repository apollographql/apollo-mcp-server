use schemars::JsonSchema;
use serde::Deserialize;

/// Configuration for a single prompt definition in the YAML config
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PromptConfig {
    /// Unique identifier for the prompt
    pub name: String,
    /// Human-readable title
    #[serde(default)]
    pub title: Option<String>,
    /// What the prompt does
    #[serde(default)]
    pub description: Option<String>,
    /// Template arguments
    #[serde(default)]
    pub arguments: Option<Vec<PromptArgumentConfig>>,
    /// Conversation messages
    #[serde(default)]
    pub messages: Vec<PromptMessageConfig>,
}

/// Configuration for a prompt argument
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PromptArgumentConfig {
    /// Argument identifier, used in `{{name}}` placeholders
    pub name: String,
    /// Human-readable title
    #[serde(default)]
    pub title: Option<String>,
    /// Purpose of the argument
    #[serde(default)]
    pub description: Option<String>,
    /// Whether the argument must be provided
    #[serde(default)]
    pub required: Option<bool>,
}

/// Configuration for a prompt message
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PromptMessageConfig {
    /// Message sender role (defaults to "user")
    #[serde(default)]
    pub role: Option<PromptMessageRoleConfig>,
    /// Message content
    pub content: PromptContentConfig,
}

/// The role of a message sender
#[derive(Debug, Clone, Copy, Default, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PromptMessageRoleConfig {
    #[default]
    User,
    Assistant,
}

/// Content types that can be included in prompt messages.
/// Discriminated by the `type` field, matching MCP `PromptMessageContent`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum PromptContentConfig {
    /// Plain text content with optional `{{arg}}` placeholders
    Text { text: String },
    /// Image content with base64-encoded data
    Image {
        data: String,
        #[serde(rename = "mimeType")]
        mime_type: String,
    },
    /// Embedded server-side resource
    Resource { resource: ResourceContentConfig },
    /// A link to a resource
    ResourceLink {
        uri: String,
        name: String,
        #[serde(default)]
        description: Option<String>,
        #[serde(default, rename = "mimeType")]
        mime_type: Option<String>,
    },
}

/// Embedded resource content (text or binary blob)
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResourceContentConfig {
    /// Text resource with URI
    Text {
        uri: String,
        #[serde(default)]
        mime_type: Option<String>,
        text: String,
    },
    /// Binary blob resource with URI (base64-encoded)
    Blob {
        uri: String,
        #[serde(default)]
        mime_type: Option<String>,
        blob: String,
    },
}

impl Default for PromptContentConfig {
    fn default() -> Self {
        Self::Text {
            text: String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_full_prompt_config() {
        let yaml = r#"
name: check_order_history
title: "Check Order History"
description: "Look up a user's recent orders"
arguments:
  - name: email
    title: "User Email"
    description: "The email address"
    required: true
  - name: limit
    description: "Max orders"
messages:
  - role: user
    content:
      type: text
      text: "GetUserByEmail(email={{email}})"
  - role: assistant
    content:
      type: text
      text: "I'll look that up."
"#;
        let config: PromptConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.name, "check_order_history");
        assert_eq!(config.title.as_deref(), Some("Check Order History"));
        assert_eq!(
            config.description.as_deref(),
            Some("Look up a user's recent orders")
        );
        let args = config.arguments.unwrap();
        assert_eq!(args.len(), 2);
        assert_eq!(args[0].name, "email");
        assert_eq!(args[0].required, Some(true));
        assert_eq!(args[1].name, "limit");
        assert_eq!(args[1].required, None);
        assert_eq!(config.messages.len(), 2);
    }

    #[test]
    fn parse_minimal_prompt_config() {
        let yaml = r#"
name: simple
messages:
  - content:
      type: text
      text: "Hello!"
"#;
        let config: PromptConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.name, "simple");
        assert!(config.title.is_none());
        assert!(config.description.is_none());
        assert!(config.arguments.is_none());
        assert_eq!(config.messages.len(), 1);
    }

    #[test]
    fn parse_empty_prompts_vec() {
        let yaml = "[]";
        let configs: Vec<PromptConfig> = serde_yaml::from_str(yaml).unwrap();
        assert!(configs.is_empty());
    }

    #[test]
    fn deny_unknown_fields_rejects_invalid() {
        let yaml = r#"
name: test
unknown_field: true
messages: []
"#;
        let result = serde_yaml::from_str::<PromptConfig>(yaml);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("unknown field"));
    }

    #[test]
    fn missing_name_produces_error() {
        let yaml = r#"
messages:
  - content:
      type: text
      text: "Hello"
"#;
        let result = serde_yaml::from_str::<PromptConfig>(yaml);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("name"));
    }

    #[test]
    fn role_defaults_to_user() {
        let yaml = r#"
name: test
messages:
  - content:
      type: text
      text: "Hello"
"#;
        let config: PromptConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(config.messages[0].role.is_none());
    }

    #[test]
    fn parse_image_content() {
        let yaml = r#"
name: visual
messages:
  - role: user
    content:
      type: image
      data: "base64data"
      mimeType: "image/png"
"#;
        let config: PromptConfig = serde_yaml::from_str(yaml).unwrap();
        match &config.messages[0].content {
            PromptContentConfig::Image { data, mime_type } => {
                assert_eq!(data, "base64data");
                assert_eq!(mime_type, "image/png");
            }
            _ => panic!("Expected image content"),
        }
    }

    #[test]
    fn parse_resource_link_content() {
        let yaml = r#"
name: linked
messages:
  - content:
      type: resource_link
      uri: "file:///test.txt"
      name: "test.txt"
      description: "A test file"
      mimeType: "text/plain"
"#;
        let config: PromptConfig = serde_yaml::from_str(yaml).unwrap();
        match &config.messages[0].content {
            PromptContentConfig::ResourceLink {
                uri,
                name,
                description,
                mime_type,
            } => {
                assert_eq!(uri, "file:///test.txt");
                assert_eq!(name, "test.txt");
                assert_eq!(description.as_deref(), Some("A test file"));
                assert_eq!(mime_type.as_deref(), Some("text/plain"));
            }
            _ => panic!("Expected resource_link content"),
        }
    }

    #[test]
    fn parse_resource_link_minimal() {
        let yaml = r#"
name: linked_min
messages:
  - content:
      type: resource_link
      uri: "file:///test.txt"
      name: "test.txt"
"#;
        let config: PromptConfig = serde_yaml::from_str(yaml).unwrap();
        match &config.messages[0].content {
            PromptContentConfig::ResourceLink {
                description,
                mime_type,
                ..
            } => {
                assert!(description.is_none());
                assert!(mime_type.is_none());
            }
            _ => panic!("Expected resource_link content"),
        }
    }

    #[test]
    fn parse_resource_text_content() {
        let yaml = r#"
name: res_text
messages:
  - content:
      type: resource
      resource:
        type: text
        uri: "file:///doc.txt"
        text: "file contents"
"#;
        let config: PromptConfig = serde_yaml::from_str(yaml).unwrap();
        match &config.messages[0].content {
            PromptContentConfig::Resource { resource } => match resource {
                ResourceContentConfig::Text { uri, text, .. } => {
                    assert_eq!(uri, "file:///doc.txt");
                    assert_eq!(text, "file contents");
                }
                _ => panic!("Expected text resource"),
            },
            _ => panic!("Expected resource content"),
        }
    }

    #[test]
    fn parse_resource_blob_content() {
        let yaml = r#"
name: res_blob
messages:
  - content:
      type: resource
      resource:
        type: blob
        uri: "file:///image.bin"
        blob: "YmluYXJ5ZGF0YQ=="
"#;
        let config: PromptConfig = serde_yaml::from_str(yaml).unwrap();
        match &config.messages[0].content {
            PromptContentConfig::Resource { resource } => match resource {
                ResourceContentConfig::Blob { uri, blob, .. } => {
                    assert_eq!(uri, "file:///image.bin");
                    assert_eq!(blob, "YmluYXJ5ZGF0YQ==");
                }
                _ => panic!("Expected blob resource"),
            },
            _ => panic!("Expected resource content"),
        }
    }

    #[test]
    fn parse_assistant_role() {
        let yaml = r#"
name: test
messages:
  - role: assistant
    content:
      type: text
      text: "I'll help with that."
"#;
        let config: PromptConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(
            config.messages[0].role,
            Some(PromptMessageRoleConfig::Assistant)
        );
    }

    #[test]
    fn argument_required_defaults_to_none() {
        let yaml = r#"
name: test
arguments:
  - name: opt_arg
messages:
  - content:
      type: text
      text: "hello"
"#;
        let config: PromptConfig = serde_yaml::from_str(yaml).unwrap();
        let args = config.arguments.unwrap();
        assert_eq!(args[0].required, None);
        assert!(args[0].title.is_none());
        assert!(args[0].description.is_none());
    }
}
