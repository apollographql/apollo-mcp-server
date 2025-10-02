# [1.0.0] - 2025-10-01

## 🐛 Fixes

### fix: remove verbose logging - @swcollard PR #401

The tracing-subscriber crate we are using to create logs does not have a configuration to exclude the span name and attributes from the log line. This led to rather verbose logs on app startup which would dump the full operation object into the logs before the actual log line. 

This change strips the attributes from the top level spans so that we still have telemetry and tracing during this important work the server is doing, but they don't make it into the logs. The relevant details are provided in child spans after the operation has been parsed so we aren't losing any information other than a large json blob in the top level trace of generating Tools from GraphQL Operations.

## 🛠 Maintenance

### deps: update rust to v1.90.0 - @DaleSeo PR #387

Updates the Rust version to v1.90.0

