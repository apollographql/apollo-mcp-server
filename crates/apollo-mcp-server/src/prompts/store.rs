use std::collections::HashMap;
use std::collections::hash_map::Entry;

use rmcp::model::{
    AnnotateAble, GetPromptResult, Prompt, PromptArgument, PromptMessage, PromptMessageContent,
    PromptMessageRole, RawEmbeddedResource, RawImageContent, RawResource,
};

use crate::errors::PromptError;

use super::config::{
    PromptConfig, PromptContentConfig, PromptMessageRoleConfig, ResourceContentConfig,
};
use super::render::render_template;

/// Holds prompt definitions and handles lookup, rendering, and rmcp conversion.
#[derive(Debug, Clone)]
pub(crate) struct Prompts {
    prompts: Vec<Prompt>,
    configs: HashMap<String, PromptConfig>,
}

impl Prompts {
    /// Create a new Prompts from raw prompt configs.
    ///
    /// Validates non-empty names and unique names.
    pub(crate) fn new(configs: Vec<PromptConfig>) -> Result<Self, PromptError> {
        let mut prompts = Vec::with_capacity(configs.len());
        let mut config_map = HashMap::with_capacity(configs.len());

        for config in configs {
            if config.name.is_empty() {
                return Err(PromptError::EmptyName);
            }
            match config_map.entry(config.name.clone()) {
                Entry::Occupied(_) => return Err(PromptError::DuplicateName(config.name)),
                Entry::Vacant(e) => {
                    let config_ref = e.insert(config);
                    prompts.push(Prompt::from(&*config_ref));
                }
            }
        }

        Ok(Self {
            prompts,
            configs: config_map,
        })
    }

    /// Returns rmcp prompt definitions for `list_prompts`.
    pub(crate) fn list(&self) -> &[Prompt] {
        &self.prompts
    }

    /// Retrieve and render a prompt by name, returning an rmcp `GetPromptResult` directly.
    pub(crate) fn get(
        &self,
        name: &str,
        arguments: &HashMap<String, String>,
    ) -> Result<GetPromptResult, PromptError> {
        let prompt = self
            .configs
            .get(name)
            .ok_or_else(|| PromptError::NotFound(name.to_string()))?;

        let defined_arg_names: Vec<&str> = prompt
            .arguments
            .as_ref()
            .map(|args| args.iter().map(|a| a.name.as_str()).collect::<Vec<_>>())
            .unwrap_or_default();

        // Validate required arguments
        if let Some(arg_defs) = &prompt.arguments {
            for arg_def in arg_defs {
                if arg_def.required == Some(true) && !arguments.contains_key(&arg_def.name) {
                    return Err(PromptError::MissingRequiredArgument {
                        prompt_name: name.to_string(),
                        argument: arg_def.name.clone(),
                    });
                }
            }
        }

        // Render messages and convert directly to rmcp types
        let messages = prompt
            .messages
            .iter()
            .map(|msg| {
                let role =
                    PromptMessageRole::from(msg.role.unwrap_or(PromptMessageRoleConfig::User));
                let content = render_content(&msg.content, arguments, &defined_arg_names);
                PromptMessage::new(role, content)
            })
            .collect();

        let mut result = GetPromptResult::new(messages);
        if let Some(desc) = &prompt.description {
            result = result.with_description(desc);
        }
        Ok(result)
    }
}

/// Render template placeholders in content and convert to rmcp `PromptMessageContent`.
fn render_content(
    content: &PromptContentConfig,
    arguments: &HashMap<String, String>,
    defined_arg_names: &[&str],
) -> PromptMessageContent {
    match content {
        PromptContentConfig::Text { text } => {
            let rendered = render_template(text, arguments, defined_arg_names);
            PromptMessageContent::text(rendered)
        }
        PromptContentConfig::Image { data, mime_type } => PromptMessageContent::Image {
            image: RawImageContent {
                data: data.clone(),
                mime_type: mime_type.clone(),
                meta: None,
            }
            .no_annotation(),
        },
        PromptContentConfig::Resource { resource } => {
            let contents = rmcp::model::ResourceContents::from(resource.clone());
            let embedded = RawEmbeddedResource::new(contents).no_annotation();
            PromptMessageContent::Resource { resource: embedded }
        }
        PromptContentConfig::ResourceLink {
            uri,
            name,
            description,
            mime_type,
        } => {
            let mut resource = RawResource::new(uri, name);
            resource.description = description.clone();
            resource.mime_type = mime_type.clone();
            PromptMessageContent::resource_link(resource.no_annotation())
        }
    }
}

impl From<&PromptConfig> for Prompt {
    fn from(config: &PromptConfig) -> Self {
        let arguments = config.arguments.as_ref().map(|args| {
            args.iter()
                .map(|a| {
                    let mut arg = PromptArgument::new(&a.name);
                    if let Some(title) = &a.title {
                        arg = arg.with_title(title);
                    }
                    if let Some(desc) = &a.description {
                        arg = arg.with_description(desc);
                    }
                    if let Some(req) = a.required {
                        arg = arg.with_required(req);
                    }
                    arg
                })
                .collect()
        });

        let mut prompt = Prompt::new(&config.name, config.description.as_deref(), arguments);
        if let Some(title) = &config.title {
            prompt = prompt.with_title(title);
        }
        prompt
    }
}

impl From<PromptMessageRoleConfig> for PromptMessageRole {
    fn from(role: PromptMessageRoleConfig) -> Self {
        match role {
            PromptMessageRoleConfig::User => PromptMessageRole::User,
            PromptMessageRoleConfig::Assistant => PromptMessageRole::Assistant,
        }
    }
}

impl From<ResourceContentConfig> for rmcp::model::ResourceContents {
    fn from(config: ResourceContentConfig) -> Self {
        match config {
            ResourceContentConfig::Text {
                uri,
                mime_type,
                text,
            } => Self::TextResourceContents {
                uri,
                mime_type,
                text,
                meta: None,
            },
            ResourceContentConfig::Blob {
                uri,
                mime_type,
                blob,
            } => Self::BlobResourceContents {
                uri,
                mime_type,
                blob,
                meta: None,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use rmcp::model::PromptMessageContent;

    use crate::prompts::{
        PromptArgumentConfig, PromptContentConfig, PromptMessageConfig, ResourceContentConfig,
    };

    use super::*;

    fn text_message(text: &str) -> PromptMessageConfig {
        PromptMessageConfig {
            role: None,
            content: PromptContentConfig::Text {
                text: text.to_string(),
            },
        }
    }

    fn make_config(name: &str, messages: Vec<PromptMessageConfig>) -> PromptConfig {
        PromptConfig {
            name: name.to_string(),
            title: None,
            description: None,
            arguments: None,
            messages,
        }
    }

    fn args(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn get_text(result: &GetPromptResult, index: usize) -> &str {
        match &result.messages[index].content {
            PromptMessageContent::Text { text } => text.as_str(),
            _ => panic!("Expected text content"),
        }
    }

    // --- Validation Tests ---

    #[test]
    fn new_with_valid_configs() {
        let store = Prompts::new(vec![make_config("test", vec![text_message("hello")])]).unwrap();
        assert_eq!(store.list().len(), 1);
        assert_eq!(store.list()[0].name, "test");
    }

    #[test]
    fn duplicate_names_fail_validation() {
        let result = Prompts::new(vec![
            PromptConfig {
                name: "dup".to_string(),
                description: Some("first".to_string()),
                ..make_config("dup", vec![text_message("first")])
            },
            PromptConfig {
                name: "dup".to_string(),
                description: Some("second".to_string()),
                ..make_config("dup", vec![text_message("second")])
            },
        ]);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), PromptError::DuplicateName(name) if name == "dup"));
    }

    #[test]
    fn empty_config_vec() {
        let store = Prompts::new(vec![]).unwrap();
        assert!(store.list().is_empty());
    }

    #[test]
    fn list_returns_correct_metadata() {
        let config = PromptConfig {
            name: "check_order".to_string(),
            title: Some("Check Order".to_string()),
            description: Some("Look up orders".to_string()),
            arguments: Some(vec![PromptArgumentConfig {
                name: "email".to_string(),
                title: Some("User Email".to_string()),
                description: Some("The email".to_string()),
                required: Some(true),
            }]),
            messages: vec![text_message("hello")],
        };
        let store = Prompts::new(vec![config]).unwrap();
        let list = store.list();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "check_order");
        assert_eq!(list[0].title.as_deref(), Some("Check Order"));
        assert_eq!(list[0].description.as_deref(), Some("Look up orders"));
        let args = list[0].arguments.as_ref().unwrap();
        assert_eq!(args.len(), 1);
        assert_eq!(args[0].name, "email");
        assert_eq!(args[0].title.as_deref(), Some("User Email"));
        assert_eq!(args[0].description.as_deref(), Some("The email"));
        assert_eq!(args[0].required, Some(true));
    }

    #[test]
    fn invalid_empty_name_fails() {
        let result = Prompts::new(vec![make_config("", vec![text_message("hello")])]);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), PromptError::EmptyName));
    }

    #[test]
    fn prompt_with_no_arguments() {
        let store = Prompts::new(vec![make_config("simple", vec![text_message("hello")])]).unwrap();
        let list = store.list();
        assert!(list[0].arguments.is_none());
    }

    #[test]
    fn prompt_name_with_special_characters() {
        let store = Prompts::new(vec![make_config(
            "my prompt/v2 (test)",
            vec![text_message("hello")],
        )])
        .unwrap();
        assert!(store.configs.contains_key("my prompt/v2 (test)"));
    }

    // --- get() Tests ---

    #[test]
    fn get_prompt_with_substitution() {
        let config = PromptConfig {
            name: "test".to_string(),
            title: None,
            description: Some("A test prompt".to_string()),
            arguments: Some(vec![PromptArgumentConfig {
                name: "email".to_string(),
                title: None,
                description: None,
                required: Some(true),
            }]),
            messages: vec![text_message("User: {{email}}")],
        };
        let store = Prompts::new(vec![config]).unwrap();
        let result = store
            .get("test", &args(&[("email", "alice@example.com")]))
            .unwrap();
        assert_eq!(result.description.as_deref(), Some("A test prompt"));
        assert_eq!(result.messages.len(), 1);
        assert_eq!(get_text(&result, 0), "User: alice@example.com");
    }

    #[test]
    fn get_prompt_not_found() {
        let store = Prompts::new(vec![]).unwrap();
        let result = store.get("nonexistent", &HashMap::new());
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            PromptError::NotFound(name) if name == "nonexistent"
        ));
    }

    #[test]
    fn get_prompt_missing_required_argument() {
        let config = PromptConfig {
            arguments: Some(vec![PromptArgumentConfig {
                name: "email".to_string(),
                required: Some(true),
                ..Default::default()
            }]),
            ..make_config("test", vec![text_message("{{email}}")])
        };
        let store = Prompts::new(vec![config]).unwrap();
        let result = store.get("test", &HashMap::new());
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            PromptError::MissingRequiredArgument { argument, .. } if argument == "email"
        ));
    }

    #[test]
    fn get_prompt_optional_argument_omitted() {
        let config = PromptConfig {
            arguments: Some(vec![PromptArgumentConfig {
                name: "limit".to_string(),
                required: None,
                ..Default::default()
            }]),
            ..make_config("test", vec![text_message("Limit: {{limit}}")])
        };
        let store = Prompts::new(vec![config]).unwrap();
        let result = store.get("test", &HashMap::new()).unwrap();
        assert_eq!(get_text(&result, 0), "Limit: ");
    }

    #[test]
    fn get_prompt_non_text_content_passthrough() {
        let config = PromptConfig {
            name: "img".to_string(),
            title: None,
            description: None,
            arguments: Some(vec![PromptArgumentConfig {
                name: "unused".to_string(),
                ..Default::default()
            }]),
            messages: vec![PromptMessageConfig {
                role: None,
                content: PromptContentConfig::Image {
                    data: "base64data".to_string(),
                    mime_type: "image/png".to_string(),
                },
            }],
        };
        let store = Prompts::new(vec![config]).unwrap();
        let result = store.get("img", &HashMap::new()).unwrap();
        match &result.messages[0].content {
            PromptMessageContent::Image { image } => {
                assert_eq!(image.data, "base64data");
                assert_eq!(image.mime_type, "image/png");
            }
            _ => panic!("Expected image content"),
        }
    }

    #[test]
    fn get_prompt_no_messages() {
        let config = make_config("empty", vec![]);
        let store = Prompts::new(vec![config]).unwrap();
        let result = store.get("empty", &HashMap::new()).unwrap();
        assert!(result.messages.is_empty());
    }

    #[test]
    fn get_prompt_returns_description() {
        let config = PromptConfig {
            description: Some("My desc".to_string()),
            ..make_config("test", vec![text_message("hello")])
        };
        let store = Prompts::new(vec![config]).unwrap();
        let result = store.get("test", &HashMap::new()).unwrap();
        assert_eq!(result.description.as_deref(), Some("My desc"));
    }

    #[test]
    fn resource_content_passthrough() {
        let config = PromptConfig {
            name: "res".to_string(),
            title: None,
            description: None,
            arguments: None,
            messages: vec![PromptMessageConfig {
                role: None,
                content: PromptContentConfig::Resource {
                    resource: ResourceContentConfig::Text {
                        uri: "file:///test.txt".to_string(),
                        mime_type: None,
                        text: "hello".to_string(),
                    },
                },
            }],
        };

        let store = Prompts::new(vec![config]).unwrap();
        let result = store.get("res", &HashMap::new()).unwrap();
        match &result.messages[0].content {
            PromptMessageContent::Resource { resource } => {
                assert!(matches!(
                    &resource.resource,
                    rmcp::model::ResourceContents::TextResourceContents { uri, text, .. }
                        if uri == "file:///test.txt" && text == "hello"
                ));
            }
            _ => panic!("Expected resource content"),
        }
    }

    // --- Multi-message Tests ---

    #[test]
    fn multi_message_different_roles() {
        let config = PromptConfig {
            name: "multi".to_string(),
            title: None,
            description: None,
            arguments: None,
            messages: vec![
                PromptMessageConfig {
                    role: Some(PromptMessageRoleConfig::User),
                    content: PromptContentConfig::Text {
                        text: "Hello".to_string(),
                    },
                },
                PromptMessageConfig {
                    role: Some(PromptMessageRoleConfig::Assistant),
                    content: PromptContentConfig::Text {
                        text: "Hi there!".to_string(),
                    },
                },
            ],
        };
        let store = Prompts::new(vec![config]).unwrap();
        let result = store.get("multi", &HashMap::new()).unwrap();
        assert_eq!(result.messages.len(), 2);
        assert_eq!(result.messages[0].role, PromptMessageRole::User);
        assert_eq!(result.messages[1].role, PromptMessageRole::Assistant);
        assert_eq!(get_text(&result, 0), "Hello");
        assert_eq!(get_text(&result, 1), "Hi there!");
    }

    #[test]
    fn default_role_is_user() {
        let config = make_config("test", vec![text_message("hello")]);
        let store = Prompts::new(vec![config]).unwrap();
        let result = store.get("test", &HashMap::new()).unwrap();
        assert_eq!(result.messages[0].role, PromptMessageRole::User);
    }

    #[test]
    fn three_messages_in_order() {
        let config = PromptConfig {
            name: "three".to_string(),
            title: None,
            description: None,
            arguments: None,
            messages: vec![
                text_message("first"),
                PromptMessageConfig {
                    role: Some(PromptMessageRoleConfig::Assistant),
                    content: PromptContentConfig::Text {
                        text: "second".to_string(),
                    },
                },
                text_message("third"),
            ],
        };
        let store = Prompts::new(vec![config]).unwrap();
        let result = store.get("three", &HashMap::new()).unwrap();
        assert_eq!(result.messages.len(), 3);
        let texts: Vec<&str> = (0..3).map(|i| get_text(&result, i)).collect();
        assert_eq!(texts, vec!["first", "second", "third"]);
    }

    #[test]
    fn all_messages_have_substitution() {
        let config = PromptConfig {
            arguments: Some(vec![PromptArgumentConfig {
                name: "name".to_string(),
                required: Some(true),
                ..Default::default()
            }]),
            ..make_config(
                "test",
                vec![
                    text_message("Hello {{name}}"),
                    text_message("Goodbye {{name}}"),
                ],
            )
        };
        let store = Prompts::new(vec![config]).unwrap();
        let result = store.get("test", &args(&[("name", "Alice")])).unwrap();
        assert_eq!(get_text(&result, 0), "Hello Alice");
        assert_eq!(get_text(&result, 1), "Goodbye Alice");
    }

    #[test]
    fn blob_resource_content_passthrough() {
        let config = PromptConfig {
            name: "blob".to_string(),
            title: None,
            description: None,
            arguments: None,
            messages: vec![PromptMessageConfig {
                role: None,
                content: PromptContentConfig::Resource {
                    resource: ResourceContentConfig::Blob {
                        uri: "file:///data.bin".to_string(),
                        mime_type: Some("application/octet-stream".to_string()),
                        blob: "YmluYXJ5".to_string(),
                    },
                },
            }],
        };
        let store = Prompts::new(vec![config]).unwrap();
        let result = store.get("blob", &HashMap::new()).unwrap();
        match &result.messages[0].content {
            PromptMessageContent::Resource { resource } => {
                assert!(matches!(
                    &resource.resource,
                    rmcp::model::ResourceContents::BlobResourceContents { uri, blob, .. }
                        if uri == "file:///data.bin" && blob == "YmluYXJ5"
                ));
            }
            _ => panic!("Expected resource content"),
        }
    }

    #[test]
    fn resource_link_content_passthrough() {
        let config = PromptConfig {
            name: "link".to_string(),
            title: None,
            description: None,
            arguments: None,
            messages: vec![PromptMessageConfig {
                role: None,
                content: PromptContentConfig::ResourceLink {
                    uri: "file:///schema.graphql".to_string(),
                    name: "schema.graphql".to_string(),
                    description: Some("The GraphQL schema".to_string()),
                    mime_type: Some("application/graphql".to_string()),
                },
            }],
        };
        let store = Prompts::new(vec![config]).unwrap();
        let result = store.get("link", &HashMap::new()).unwrap();
        match &result.messages[0].content {
            PromptMessageContent::ResourceLink { link } => {
                assert_eq!(link.uri, "file:///schema.graphql");
                assert_eq!(link.name, "schema.graphql");
                assert_eq!(link.description.as_deref(), Some("The GraphQL schema"));
                assert_eq!(link.mime_type.as_deref(), Some("application/graphql"));
            }
            _ => panic!("Expected resource_link content"),
        }
    }

    #[test]
    fn multiple_prompts_stored_and_retrieved() {
        let store = Prompts::new(vec![
            make_config("alpha", vec![text_message("First")]),
            make_config("beta", vec![text_message("Second")]),
            make_config("gamma", vec![text_message("Third")]),
        ])
        .unwrap();

        assert_eq!(store.list().len(), 3);

        let r1 = store.get("alpha", &HashMap::new()).unwrap();
        assert_eq!(get_text(&r1, 0), "First");

        let r2 = store.get("beta", &HashMap::new()).unwrap();
        assert_eq!(get_text(&r2, 0), "Second");

        let r3 = store.get("gamma", &HashMap::new()).unwrap();
        assert_eq!(get_text(&r3, 0), "Third");
    }

    #[test]
    fn required_false_argument_not_enforced() {
        let config = PromptConfig {
            arguments: Some(vec![PromptArgumentConfig {
                name: "opt".to_string(),
                required: Some(false),
                ..Default::default()
            }]),
            ..make_config("test", vec![text_message("val={{opt}}")])
        };
        let store = Prompts::new(vec![config]).unwrap();
        // Omitting required=false argument should not error
        let result = store.get("test", &HashMap::new()).unwrap();
        assert_eq!(get_text(&result, 0), "val=");
    }

    #[test]
    fn extra_arguments_ignored() {
        let config = PromptConfig {
            arguments: Some(vec![PromptArgumentConfig {
                name: "known".to_string(),
                required: Some(true),
                ..Default::default()
            }]),
            ..make_config("test", vec![text_message("{{known}}")])
        };
        let store = Prompts::new(vec![config]).unwrap();
        let result = store
            .get("test", &args(&[("known", "val"), ("extra", "ignored")]))
            .unwrap();
        assert_eq!(get_text(&result, 0), "val");
    }

    #[test]
    fn get_prompt_no_description_returns_none() {
        let config = make_config("test", vec![text_message("hello")]);
        let store = Prompts::new(vec![config]).unwrap();
        let result = store.get("test", &HashMap::new()).unwrap();
        assert!(result.description.is_none());
    }

    #[test]
    fn text_template_in_resource_not_rendered() {
        // Template placeholders in non-text content should not be rendered
        let config = PromptConfig {
            name: "img_tmpl".to_string(),
            title: None,
            description: None,
            arguments: Some(vec![PromptArgumentConfig {
                name: "data".to_string(),
                ..Default::default()
            }]),
            messages: vec![PromptMessageConfig {
                role: None,
                content: PromptContentConfig::Image {
                    data: "{{data}}".to_string(),
                    mime_type: "image/png".to_string(),
                },
            }],
        };
        let store = Prompts::new(vec![config]).unwrap();
        let result = store
            .get("img_tmpl", &args(&[("data", "replaced")]))
            .unwrap();
        match &result.messages[0].content {
            PromptMessageContent::Image { image } => {
                // Image data should NOT be template-rendered
                assert_eq!(image.data, "{{data}}");
            }
            _ => panic!("Expected image content"),
        }
    }
}
