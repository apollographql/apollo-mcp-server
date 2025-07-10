use rmcp::model::Tool;
use rmcp::serde_json;
use std::fmt;
use tiktoken_rs::{cl100k_base, o200k_base, p50k_base};

#[derive(Debug, Clone)]
pub struct TokenEstimates {
    pub anthropic: Option<usize>,
    pub gemini: Option<usize>,
    pub openai: Option<usize>,
    pub fallback: usize,
}

impl fmt::Display for TokenEstimates {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut estimates = Vec::new();

        if let Some(count) = self.anthropic {
            estimates.push(format!("{count} Anthropic tokens"));
        }
        if let Some(count) = self.gemini {
            estimates.push(format!("{count} Gemini tokens"));
        }
        if let Some(count) = self.openai {
            estimates.push(format!("{count} OpenAI tokens"));
        }

        if estimates.is_empty() {
            write!(f, "~{} tokens (fallback estimate)", self.fallback)
        } else {
            write!(f, "{}", estimates.join(", "))
        }
    }
}

pub fn count_tokens_from_tool(tool: &Tool) -> TokenEstimates {
    let tokenizer = TokenCounter;
    let tool_text = format!(
        "{}\n{}\n{}",
        tool.name,
        tool.description.as_ref().map(|d| d.as_ref()).unwrap_or(""),
        serde_json::to_string_pretty(&tool.input_schema).unwrap_or_default()
    );
    tokenizer.count_tokens(&tool_text)
}

struct TokenCounter;

impl TokenCounter {
    pub fn count_tokens(&self, text: &str) -> TokenEstimates {
        let fallback = self.estimate_tokens(text);
        TokenEstimates {
            anthropic: self.count_anthropic_tokens(text),
            gemini: self.count_gemini_tokens(text),
            openai: self.count_openai_tokens(text),
            fallback,
        }
    }

    fn count_openai_tokens(&self, text: &str) -> Option<usize> {
        // Start with o200k_base (GPT-4o, o1 models)
        if let Ok(tokenizer) = o200k_base() {
            return Some(tokenizer.encode_with_special_tokens(text).len());
        }

        // Fallback to cl100k_base (ChatGPT, GPT-4)
        if let Ok(tokenizer) = cl100k_base() {
            return Some(tokenizer.encode_with_special_tokens(text).len());
        }

        // Final fallback to p50k_base (GPT-3.5, Codex)
        if let Ok(tokenizer) = p50k_base() {
            return Some(tokenizer.encode_with_special_tokens(text).len());
        }

        None
    }

    // TODO: Implement using Anthropic's SDK or REST API (https://docs.anthropic.com/en/docs/build-with-claude/token-counting)
    fn count_anthropic_tokens(&self, _text: &str) -> Option<usize> {
        None
    }

    // TODO: Implement their Gemini's SDK or REST API (https://ai.google.dev/api/tokens#v1beta.models.countTokens)
    fn count_gemini_tokens(&self, _text: &str) -> Option<usize> {
        None
    }

    fn estimate_tokens(&self, text: &str) -> usize {
        let character_count = text.chars().count();
        character_count / 4
    }
}
