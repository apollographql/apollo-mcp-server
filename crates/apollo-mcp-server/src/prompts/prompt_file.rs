use std::path::Path;

use rmcp::model::{Prompt, PromptArgument};
use serde::Deserialize;
use serde_json::Map;
use tracing::debug;

/// A loaded prompt file, containing the MCP prompt metadata and the template body.
#[derive(Clone, Debug)]
pub(crate) struct PromptFile {
    /// The MCP prompt definition (name, description, arguments).
    pub(crate) prompt: Prompt,
    /// The template body text with `{{arg}}` placeholders.
    pub(crate) template: String,
}

#[derive(Debug, Deserialize)]
struct PromptFrontmatter {
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    arguments: Vec<FrontmatterArgument>,
}

#[derive(Debug, Deserialize)]
struct FrontmatterArgument {
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    required: bool,
}

/// Parse YAML frontmatter and body from a Markdown string.
///
/// Expects the content to start with `---`, followed by YAML, then a closing `---`,
/// and the remaining text is the prompt template body.
fn parse_frontmatter(content: &str) -> Result<(PromptFrontmatter, String), String> {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return Err("Missing opening frontmatter delimiter '---'".to_string());
    }

    let after_opening = &trimmed[3..];
    let closing_pos = after_opening
        .find("\n---")
        .ok_or_else(|| "Missing closing frontmatter delimiter '---'".to_string())?;

    let yaml_str = &after_opening[..closing_pos];
    let body_start = closing_pos + 4; // skip "\n---"
    let body = after_opening
        .get(body_start..)
        .unwrap_or("")
        .trim()
        .to_string();

    let frontmatter: PromptFrontmatter =
        serde_yaml::from_str(yaml_str).map_err(|err| format!("Invalid frontmatter YAML: {err}"))?;

    Ok((frontmatter, body))
}

/// Replace `{{name}}` placeholders in the template with argument values.
pub(crate) fn substitute_args(
    template: &str,
    arguments: &Map<String, serde_json::Value>,
) -> String {
    let mut result = template.to_string();
    for (key, value) in arguments {
        let placeholder = format!("{{{{{key}}}}}");
        if let Some(s) = value.as_str() {
            result = result.replace(&placeholder, s);
        } else {
            let replacement = value.to_string();
            result = result.replace(&placeholder, &replacement);
        }
    }
    result
}

fn load_single_file(path: &Path) -> Result<PromptFile, String> {
    let content = std::fs::read_to_string(path)
        .map_err(|err| format!("Failed to read {}: {err}", path.display()))?;

    let (frontmatter, body) = parse_frontmatter(&content)
        .map_err(|err| format!("Failed to parse {}: {err}", path.display()))?;

    let arguments: Vec<PromptArgument> = frontmatter
        .arguments
        .into_iter()
        .map(|arg| {
            let mut pa = PromptArgument::new(&arg.name).with_required(arg.required);
            if let Some(desc) = arg.description {
                pa = pa.with_description(desc);
            }
            pa
        })
        .collect();

    let prompt = Prompt::new(
        &frontmatter.name,
        frontmatter.description.as_deref(),
        if arguments.is_empty() {
            None
        } else {
            Some(arguments)
        },
    );

    Ok(PromptFile {
        prompt,
        template: body,
    })
}

/// Load all `.md` prompt files from the given directory.
///
/// Returns `Ok(vec![])` if the directory does not exist.
pub(crate) fn load_from_path(path: &Path) -> Result<Vec<PromptFile>, String> {
    let Ok(dir) = path.read_dir() else {
        return Ok(Vec::new());
    };

    let mut prompts = Vec::new();
    for entry in dir {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                debug!("Failed to read prompts directory entry, ignoring: {err}");
                continue;
            }
        };
        let file_path = entry.path();
        if !file_path.is_file() {
            debug!("{} is not a file, ignoring", file_path.display());
            continue;
        }
        if file_path.extension().and_then(|ext| ext.to_str()) != Some("md") {
            debug!("{} is not a .md file, ignoring", file_path.display());
            continue;
        }

        let prompt = load_single_file(&file_path)?;
        debug!(
            "Loaded prompt '{}' from {}",
            prompt.prompt.name,
            file_path.display()
        );
        prompts.push(prompt);
    }

    Ok(prompts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_fs::{TempDir, prelude::*};

    #[test]
    fn parse_valid_frontmatter() {
        let content = r#"---
name: check_order_history
description: "Look up a user's recent orders"
arguments:
  - name: email
    required: true
---

1. GetUserByEmail(email={{email}}) → get userId
2. GetUserOrders(userId, first=10)
"#;
        let (fm, body) = parse_frontmatter(content).unwrap();
        assert_eq!(fm.name, "check_order_history");
        assert_eq!(
            fm.description.as_deref(),
            Some("Look up a user's recent orders")
        );
        assert_eq!(fm.arguments.len(), 1);
        assert_eq!(fm.arguments[0].name, "email");
        assert!(fm.arguments[0].required);
        assert!(body.starts_with("1. GetUserByEmail"));
    }

    #[test]
    fn parse_frontmatter_no_arguments() {
        let content = "---\nname: simple\n---\nHello world";
        let (fm, body) = parse_frontmatter(content).unwrap();
        assert_eq!(fm.name, "simple");
        assert!(fm.description.is_none());
        assert!(fm.arguments.is_empty());
        assert_eq!(body, "Hello world");
    }

    #[test]
    fn parse_frontmatter_missing_opening() {
        let content = "name: bad\n---\nBody";
        let err = parse_frontmatter(content).unwrap_err();
        assert!(err.contains("Missing opening frontmatter delimiter"));
    }

    #[test]
    fn parse_frontmatter_missing_closing() {
        let content = "---\nname: bad\nBody without closing";
        let err = parse_frontmatter(content).unwrap_err();
        assert!(err.contains("Missing closing frontmatter delimiter"));
    }

    #[test]
    fn substitute_args_single() {
        let mut args = Map::new();
        args.insert(
            "email".to_string(),
            serde_json::Value::String("test@example.com".to_string()),
        );
        let result = substitute_args("GetUser(email={{email}})", &args);
        assert_eq!(result, "GetUser(email=test@example.com)");
    }

    #[test]
    fn substitute_args_multiple() {
        let mut args = Map::new();
        args.insert(
            "name".to_string(),
            serde_json::Value::String("Alice".to_string()),
        );
        args.insert("count".to_string(), serde_json::json!(10));
        let result = substitute_args("Hello {{name}}, you have {{count}} items", &args);
        assert_eq!(result, "Hello Alice, you have 10 items");
    }

    #[test]
    fn substitute_args_unmatched_left_as_is() {
        let args = Map::new();
        let result = substitute_args("Hello {{unknown}}", &args);
        assert_eq!(result, "Hello {{unknown}}");
    }

    #[test]
    fn load_from_nonexistent_directory() {
        let result = load_from_path(Path::new("nonexistent_prompts_dir"));
        assert_eq!(result.unwrap().len(), 0);
    }

    #[test]
    fn load_from_directory_with_prompt_files() {
        let temp = TempDir::new().expect("Could not create temporary directory for test");
        temp.child("greeting.md")
            .write_str(
                "---\nname: greeting\ndescription: \"A greeting prompt\"\narguments:\n  - name: name\n    required: true\n---\nHello {{name}}!",
            )
            .unwrap();
        temp.child("simple.md")
            .write_str("---\nname: simple\n---\nNo args needed.")
            .unwrap();
        // Non-md file should be ignored
        temp.child("readme.txt").write_str("not a prompt").unwrap();

        let prompts = load_from_path(temp.path()).unwrap();
        assert_eq!(prompts.len(), 2);

        let greeting = prompts
            .iter()
            .find(|p| p.prompt.name == "greeting")
            .unwrap();
        assert_eq!(
            greeting.prompt.description.as_deref(),
            Some("A greeting prompt")
        );
        assert_eq!(greeting.prompt.arguments.as_ref().unwrap().len(), 1);
        assert_eq!(greeting.template, "Hello {{name}}!");

        let simple = prompts.iter().find(|p| p.prompt.name == "simple").unwrap();
        assert!(simple.prompt.arguments.is_none());
        assert_eq!(simple.template, "No args needed.");
    }

    #[test]
    fn load_single_file_with_argument_description() {
        let temp = TempDir::new().expect("Could not create temporary directory for test");
        let file = temp.child("with_desc.md");
        file.write_str(
            "---\nname: with_desc\narguments:\n  - name: email\n    description: \"The user email\"\n    required: true\n---\nLookup {{email}}",
        )
        .unwrap();

        let prompt = super::load_single_file(file.path()).unwrap();
        let args = prompt.prompt.arguments.unwrap();
        assert_eq!(args[0].description.as_deref(), Some("The user email"));
        assert_eq!(args[0].required, Some(true));
    }
}
