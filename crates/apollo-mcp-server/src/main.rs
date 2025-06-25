use anyhow::bail;
use apollo_mcp_registry::platform_api::PlatformApiConfig;
use apollo_mcp_registry::platform_api::operation_collections::collection_poller::CollectionSource;
use apollo_mcp_registry::uplink::persisted_queries::ManifestSource;
use apollo_mcp_registry::uplink::schema::SchemaSource;
use apollo_mcp_registry::uplink::{Endpoints, SecretString, UplinkConfig};
use apollo_mcp_server::custom_scalar_map::CustomScalarMap;
use apollo_mcp_server::errors::ServerError;
use apollo_mcp_server::operations::{MutationMode, OperationSource};
use apollo_mcp_server::server::Server;
use apollo_mcp_server::server::Transport;
use clap::builder::Styles;
use clap::builder::styling::{AnsiColor, Effects};
use clap::{ArgAction, Parser};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use std::env;
use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;
use tracing::{Level, info};
use tracing_subscriber::EnvFilter;
use url::{ParseError, Url};

/// Clap styling
const STYLES: Styles = Styles::styled()
    .header(AnsiColor::Green.on_default().effects(Effects::BOLD))
    .usage(AnsiColor::Green.on_default().effects(Effects::BOLD))
    .literal(AnsiColor::Cyan.on_default().effects(Effects::BOLD))
    .placeholder(AnsiColor::Cyan.on_default());

/// Arguments to the MCP server
#[derive(Debug, clap::Parser)]
#[command(
    styles = STYLES,
    about = "Apollo MCP Server - invoke GraphQL operations from an AI agent",
)]
struct Args {
    /// The working directory to use
    #[arg(long, short = 'd')]
    directory: Option<PathBuf>,

    /// The path to the GraphQL API schema file
    #[arg(long, short = 's')]
    schema: Option<PathBuf>,

    /// The path to the GraphQL custom_scalars_config file
    #[arg(long, short = 'c', required = false)]
    custom_scalars_config: Option<PathBuf>,

    /// The GraphQL endpoint the server will invoke
    #[arg(long, short = 'e', default_value = "http://127.0.0.1:4000")]
    endpoint: String,

    /// Headers to send to the endpoint
    #[arg(long = "header", action = clap::ArgAction::Append)]
    headers: Vec<String>,

    /// The IP address to bind the SSE server to
    ///
    /// [default: 127.0.0.1]
    #[arg(long)]
    sse_address: Option<IpAddr>,

    /// Start the server using the SSE transport on the given port
    ///
    /// [default: 5000]
    #[arg(long)]
    sse_port: Option<u16>,

    /// Expose the schema to the MCP client through `introspect` and `execute` tools
    #[arg(long, short = 'i')]
    introspection: bool,

    /// Expose a tool that returns the URL to open a GraphQL operation in Apollo Explorer (requires APOLLO_GRAPH_REF)
    #[arg(long, short = 'x', requires = "apollo_graph_ref")]
    explorer: bool,

    /// Enable use of uplink to get the persisted queries (requires APOLLO_KEY and APOLLO_GRAPH_REF)
    #[arg(
        long,
        requires = "apollo_key",
        requires = "apollo_graph_ref",
        conflicts_with_all(["operations", "collection", "manifest"])
    )]
    uplink_manifest: bool,

    /// Deprecated aliases for uplink_manifest to make it backwards compatible
    #[arg(
        hide = true,
        short = 'u',
        long,
        requires = "apollo_key",
        requires = "apollo_graph_ref",
        conflicts_with_all(["uplink_manifest"])
    )]
    uplink: bool,

    /// Operation files to expose as MCP tools
    #[arg(long = "operations", short = 'o', num_args=0..)]
    operations: Vec<PathBuf>,

    /// The path to the persisted query manifest containing operations
    #[arg(long, conflicts_with_all(["operations", "collection", "uplink_manifest"]))]
    manifest: Option<PathBuf>,

    /// collection id to expose as MCP tools, or `default` to expose the default tools for the variant (requires APOLLO_KEY)
    #[arg(
        long,
        conflicts_with_all(["operations", "manifest", "uplink_manifest"]),
        requires = "apollo_key",
        requires_ifs([
            ("default", "apollo_graph_ref"),
            ("Default", "apollo_graph_ref"),
            ("DEFAULT", "apollo_graph_ref")
        ])
    )]
    collection: Option<String>,

    // Configure when to allow mutations
    #[clap(long, short = 'm', default_value_t, value_enum)]
    allow_mutations: MutationMode,

    /// Disable operation root field types in tool description
    #[arg(long)]
    disable_type_description: bool,

    /// Disable schema type definitions referenced by all fields returned by the operation in the tool description
    #[arg(long)]
    disable_schema_description: bool,

    /// The log level for the MCP Server
    #[arg(long = "log", short = 'l', global = true, default_value_t = Level::INFO)]
    log_level: Level,

    /// The IP address to bind the Streamable HTTP server to
    ///
    /// [default: 127.0.0.1]
    #[arg(long, conflicts_with_all(["sse_port", "sse_address"]))]
    http_address: Option<IpAddr>,

    /// Start the server using the Streamable HTTP transport on the given port
    ///
    /// [default: 5000]
    #[arg(long, conflicts_with_all(["sse_port", "sse_address"]))]
    http_port: Option<u16>,

    /// The endpoints (comma separated) polled to fetch the latest supergraph schema.
    #[clap(long, env, action = ArgAction::Append)]
    // Should be a Vec<Url> when https://github.com/clap-rs/clap/discussions/3796 is solved
    apollo_uplink_endpoints: Option<String>,

    #[clap(env)]
    apollo_registry_url: Option<String>,

    /// Your Apollo key.
    #[clap(env = "APOLLO_KEY", long)]
    apollo_key: Option<String>,

    /// Your Apollo graph reference.
    #[clap(env = "APOLLO_GRAPH_REF", long)]
    apollo_graph_ref: Option<String>,
}

impl Args {
    #[allow(clippy::result_large_err)]
    fn uplink_config(&self) -> Result<UplinkConfig, ServerError> {
        Ok(UplinkConfig {
            apollo_key: SecretString::from(
                self.apollo_key
                    .clone()
                    .ok_or(ServerError::EnvironmentVariable(String::from("APOLLO_KEY")))?,
            ),
            apollo_graph_ref: self.apollo_graph_ref.clone().ok_or(
                ServerError::EnvironmentVariable(String::from("APOLLO_GRAPH_REF")),
            )?,
            poll_interval: Duration::from_secs(10),
            timeout: Duration::from_secs(30),
            endpoints: self
                .apollo_uplink_endpoints
                .as_ref()
                .map(|endpoints| self.parse_endpoints(endpoints))
                .transpose()?,
        })
    }

    #[allow(clippy::result_large_err)]
    fn parse_endpoints(&self, endpoints: &str) -> Result<Endpoints, ServerError> {
        Ok(Endpoints::fallback(
            endpoints
                .split(',')
                .map(|endpoint| Url::parse(endpoint.trim()))
                .collect::<Result<Vec<Url>, ParseError>>()
                .map_err(ServerError::UrlParseError)?,
        ))
    }

    #[allow(clippy::result_large_err)]
    fn platform_api_config(&self) -> Result<PlatformApiConfig, ServerError> {
        Ok(PlatformApiConfig::new(
            SecretString::from(
                self.apollo_key
                    .clone()
                    .ok_or(ServerError::EnvironmentVariable(String::from("APOLLO_KEY")))?,
            ),
            Duration::from_secs(30),
            Duration::from_secs(30),
            self.apollo_registry_url
                .as_ref()
                .map(|url| Url::parse(url))
                .transpose()
                .map_err(ServerError::UrlParseError)?,
        ))
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    let transport = if args.http_port.is_some() || args.http_address.is_some() {
        Transport::StreamableHttp {
            address: args.http_address.unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST)),
            port: args.http_port.unwrap_or(5000),
        }
    } else if args.sse_port.is_some() || args.sse_address.is_some() {
        Transport::SSE {
            address: args.sse_address.unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST)),
            port: args.sse_port.unwrap_or(5000),
        }
    } else {
        Transport::Stdio
    };

    // When using the Stdio transport, send output to stderr since stdout is used for MCP messages
    match transport {
        Transport::SSE { .. } | Transport::StreamableHttp { .. } => tracing_subscriber::fmt()
            .with_env_filter(EnvFilter::from_default_env().add_directive(args.log_level.into()))
            .with_ansi(true)
            .with_target(false)
            .init(),
        Transport::Stdio => tracing_subscriber::fmt()
            .with_env_filter(EnvFilter::from_default_env().add_directive(args.log_level.into()))
            .with_writer(std::io::stderr)
            .with_ansi(true)
            .with_target(false)
            .init(),
    };

    info!(
        "Apollo MCP Server v{} // (c) Apollo Graph, Inc. // Licensed under MIT",
        std::env!("CARGO_PKG_VERSION")
    );

    let schema_source = if let Some(path) = &args.schema {
        SchemaSource::File {
            path: path.clone(),
            watch: true,
        }
    } else if args.apollo_graph_ref.is_some() {
        SchemaSource::Registry(args.uplink_config()?)
    } else {
        bail!(ServerError::NoSchema);
    };

    let collection_id = args.collection.as_ref().and_then(|c| {
        if c == "default" || c == "Default" || c == "DEFAULT" {
            None
        } else {
            Some(c.clone())
        }
    });

    let operation_source = if let Some(manifest) = args.manifest {
        OperationSource::from(ManifestSource::LocalHotReload(vec![manifest]))
    } else if !args.operations.is_empty() {
        OperationSource::from(args.operations)
    } else if let Some(collection_id) = collection_id {
        OperationSource::Collection(CollectionSource::Id(
            collection_id,
            args.platform_api_config()?,
        ))
    } else if args.uplink_manifest || (args.uplink && args.collection.is_none()) {
        OperationSource::from(ManifestSource::Uplink(args.uplink_config()?))
    } else if let Some(graph_ref) = &args.apollo_graph_ref {
        OperationSource::Collection(CollectionSource::Default(
            graph_ref.clone(),
            args.platform_api_config()?,
        ))
    } else {
        if !args.introspection {
            bail!(ServerError::NoOperations);
        }
        OperationSource::None
    };

    let default_headers = parse_headers(args.headers)?;

    if let Some(directory) = args.directory {
        env::set_current_dir(directory)?;
    }

    let explorer_graph_ref = if args.explorer {
        Some(
            args.apollo_graph_ref
                .ok_or(ServerError::EnvironmentVariable(String::from(
                    "APOLLO_GRAPH_REF",
                )))?,
        )
    } else {
        None
    };

    Ok(Server::builder()
        .transport(transport)
        .schema_source(schema_source)
        .operation_source(operation_source)
        .endpoint(args.endpoint)
        .maybe_explorer_graph_ref(explorer_graph_ref)
        .headers(default_headers)
        .introspection(args.introspection)
        .mutation_mode(args.allow_mutations)
        .disable_type_description(args.disable_type_description)
        .disable_schema_description(args.disable_schema_description)
        .custom_scalar_map(
            args.custom_scalars_config
                .map(|custom_scalars_config| CustomScalarMap::try_from(&custom_scalars_config))
                .transpose()?,
        )
        .build()
        .start()
        .await?)
}

fn parse_headers(headers: Vec<String>) -> Result<HeaderMap, ServerError> {
    let mut default_headers = HeaderMap::new();
    for header in headers {
        let parts: Vec<&str> = header.splitn(2, ':').map(|s| s.trim()).collect();
        match (parts.first(), parts.get(1)) {
            (Some(key), Some(value)) => {
                default_headers.append(HeaderName::from_str(key)?, HeaderValue::from_str(value)?);
            }
            _ => return Err(ServerError::Header(header)),
        }
    }
    Ok(default_headers)
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::AUTHORIZATION;

    #[test]
    fn test_parse_headers_empty() {
        let headers = vec![];

        let result = parse_headers(headers).unwrap();

        assert_eq!(result.len(), 0)
    }

    #[test]
    fn test_parse_headers_authorization() {
        let headers = vec![
            "Authorization: Bearer 1234567890".to_string(),
            "X-TEST: abcde".to_string(),
        ];

        let result = parse_headers(headers).unwrap();

        assert_eq!(result.len(), 2);
        assert_eq!(
            result.get(AUTHORIZATION),
            Some(&HeaderValue::from_str("Bearer 1234567890").unwrap()),
        );
        assert_eq!(
            result.get("X-TEST"),
            Some(&HeaderValue::from_str("abcde").unwrap()),
        );
    }

    #[test]
    fn test_parse_headers_with_colon_in_value() {
        let headers = vec![
            "X-URL: https://example.com:8080/path".to_string(),
            "X-API-KEY: user::graph::123".to_string(),
        ];

        let result = parse_headers(headers).unwrap();

        assert_eq!(result.len(), 2);
        assert_eq!(
            result.get("X-URL"),
            Some(&HeaderValue::from_str("https://example.com:8080/path").unwrap())
        );
        assert_eq!(
            result.get("X-API-KEY"),
            Some(&HeaderValue::from_str("user::graph::123").unwrap())
        );
    }

    #[test]
    fn test_parse_headers_empty_value() {
        let headers = vec!["Authorization:".to_string()];
        let result = parse_headers(headers).unwrap();

        assert_eq!(result.len(), 1);
        assert_eq!(
            result.get(AUTHORIZATION),
            Some(&HeaderValue::from_str("").unwrap())
        );
    }

    #[test]
    fn test_parse_headers_missing_colon() {
        let headers = vec!["Authorization; Bearer 1234567890".to_string()];
        let result = parse_headers(headers);

        assert!(result.is_err());
        match result.unwrap_err() {
            ServerError::Header(header) => assert_eq!(header, "Authorization; Bearer 1234567890"),
            _ => panic!("Expected ServerError::Header"),
        }
    }
}
