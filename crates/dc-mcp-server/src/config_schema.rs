//! Binary to output the JSON Schema for Apollo MCP Server configuration files

// Most runtime code is unused by this binary
#![allow(unused_imports, dead_code)]

use anyhow::Context;
use schemars::schema_for;

mod runtime;

fn main() -> anyhow::Result<()> {
    println!(
        "{}",
        serde_json::to_string_pretty(&schema_for!(runtime::Config))
            .with_context(|| "Failed to generate schema")?
    );
    Ok(())
}
