//! Test file with intentional best practice violations.
//! This file is for testing the AI code review workflow.
//! DELETE THIS FILE after testing.

use std::collections::HashMap;

/// Process items - violation: collect() then iterate when chaining would work
pub fn process_items(items: Vec<String>) -> Vec<String> {
    let uppercased: Vec<String> = items.iter().map(|s| s.to_uppercase()).collect();
    uppercased.iter().map(|s| s.trim().to_string()).collect()
}

/// Clone items in a loop - violation: cloning inside loop when borrowing would work
pub fn find_matching(items: &[String], prefix: &str) -> Vec<String> {
    let mut results = Vec::new();
    for item in items {
        let cloned = item.clone(); // unnecessary clone
        if cloned.starts_with(prefix) {
            results.push(cloned);
        }
    }
    results
}

/// Get config value - violation: unwrap_or with allocation instead of unwrap_or_else
pub fn get_config_value(config: &HashMap<String, String>, key: &str) -> String {
    config.get(key).cloned().unwrap_or(String::from("default"))
}

/// Format user data - violation: using format! when push_str would be more efficient
pub fn build_greeting(names: &[String]) -> String {
    let mut result = String::new();
    for name in names {
        result = format!("{result}Hello, {name}! ");
    }
    result
}
