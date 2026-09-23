//! SEP-2106 coverage of generated GraphQL schemas and their MCP wire representation.
use std::{collections::HashMap, sync::Arc, time::Duration};

use proptest::prelude::*;
use rmcp::{
    ServiceExt as _,
    model::{CallToolResult, Tool},
};
use serde_json::{Value, json};
use tokio_util::task::AbortOnDropHandle;

use super::{Running, test_support::create_test_running};
use crate::{
    custom_scalar_map::CustomScalarMap,
    operations::{MutationMode, RawOperation},
};

const SCHEMA: &str = r#"
    enum Status { OPEN CLOSED }
    input Filter { status: Status!, children: [Filter!], matrix: [[Int!]!]!, limit: Int! = 10 }
    scalar Choice
    interface Node { id: ID! }
    type User implements Node { id: ID!, name: String! }
    type Team implements Node { id: ID!, title: String! }
    union SearchResult = User | Team
    type Query {
        search(filter: Filter!, choice: Choice, count: Int!): [SearchResult!]!
        node: Node!
        choice: Choice
    }
"#;
const QUERY: &str = r#"
    query Search($filter: Filter!, $choice: Choice, $count: Int! = 1) {
        search(filter: $filter, choice: $choice, count: $count) {
            ... on User { name }
            ... on Team { title }
        }
        node { id ... on User { name } }
        choice
    }
"#;

fn fixture() -> Running {
    fixture_with_query(QUERY)
}

fn fixture_with_query(query: &str) -> Running {
    let schema = apollo_compiler::Schema::parse_and_validate(SCHEMA, "schema.graphql").unwrap();
    // Validate the fixture's operation independently of our schema generator.
    apollo_compiler::ExecutableDocument::parse_and_validate(&schema, query, "query.graphql")
        .unwrap();
    let scalars: CustomScalarMap = json!({"Choice": {
        "type": "object",
        "properties": {"kind": {"enum": ["text", "number"]}},
        "required": ["kind", "value"],
        "allOf": [{
            "if": {"properties": {"kind": {"const": "text"}}},
            "then": {"properties": {"value": {"type": "string"}}},
            "else": {"properties": {"value": {"type": "integer"}}}
        }]
    }})
    .to_string()
    .parse()
    .unwrap();
    let operation = RawOperation::from((query.to_owned(), None))
        .into_operation(
            &schema,
            Some(&scalars),
            MutationMode::None,
            false,
            false,
            true,
            &HashMap::new(),
            &HashMap::new(),
        )
        .unwrap()
        .unwrap();
    let mut running = create_test_running();
    running.schema = Arc::new(tokio::sync::RwLock::new(schema));
    running.operations = Arc::new(tokio::sync::RwLock::new(vec![operation]));
    running.custom_scalar_map = Some(scalars);
    running.enable_output_schema = true;
    running
}

fn wire_tool(running: &Running) -> Value {
    let operations = running.operations.try_read().unwrap();
    serde_json::to_value(AsRef::<Tool>::as_ref(&operations[0])).unwrap()
}

fn validator(schema: &Value) -> jsonschema::Validator {
    jsonschema::draft202012::meta::validate(schema).unwrap();
    jsonschema::draft202012::new(schema).unwrap()
}

fn check_tool(tool: &Value) {
    assert_eq!(tool["inputSchema"]["type"], "object");
    // GraphQL returns an object response envelope even when selected fields are lists/scalars.
    assert_eq!(tool["outputSchema"]["type"], "object");
    let input = validator(&tool["inputSchema"]);
    let output = validator(&tool["outputSchema"]);
    let valid_input = json!({"filter": {"status": "OPEN", "matrix": [[1, 2], []],
        "children": [{"status": "CLOSED", "matrix": []}]},
        "choice": {"kind": "text", "value": "hello"}});
    assert!(input.is_valid(&valid_input));
    for pointer in [
        "/filter/status",
        "/filter/matrix/0/0",
        "/filter/children/0/status",
        "/choice/value",
    ] {
        let mut invalid = valid_input.clone();
        *invalid.pointer_mut(pointer).unwrap() = json!(false);
        assert!(!input.is_valid(&invalid), "accepted invalid {pointer}");
    }
    assert!(!input.is_valid(&json!({})));
    let mut invalid_enum = valid_input.clone();
    invalid_enum["filter"]["status"] = json!("UNKNOWN");
    assert!(!input.is_valid(&invalid_enum));
    // Defaults allow omission, but explicit null still violates the non-null type.
    assert_eq!(tool["inputSchema"]["properties"]["count"]["default"], 1);
    assert_eq!(
        tool["inputSchema"]["definitions"]["Filter"]["properties"]["limit"]["default"],
        10
    );
    for pointer in ["/count", "/filter/limit"] {
        let mut explicit_default = valid_input.clone();
        if pointer == "/count" {
            explicit_default["count"] = json!(1);
        } else {
            explicit_default["filter"]["limit"] = json!(10);
        }
        assert!(input.is_valid(&explicit_default));
        *explicit_default.pointer_mut(pointer).unwrap() = Value::Null;
        assert!(!input.is_valid(&explicit_default));
    }
    let mut recursive = valid_input.clone();
    recursive["filter"]["children"][0]["children"] = json!([{
        "status": "OPEN", "matrix": [], "children": [{"status": "CLOSED", "matrix": []}]
    }]);
    assert!(input.is_valid(&recursive));
    recursive["filter"]["children"][0]["children"][0]["children"][0]["status"] = json!("UNKNOWN");
    assert!(!input.is_valid(&recursive));
    let mut nullable = valid_input.clone();
    nullable["filter"]["children"] = Value::Null;
    nullable["choice"] = Value::Null;
    assert!(input.is_valid(&nullable));
    nullable["filter"]["children"] = json!([null]);
    assert!(!input.is_valid(&nullable));
    let mut number_choice = valid_input.clone();
    number_choice["choice"] = json!({"kind": "number", "value": 7});
    assert!(input.is_valid(&number_choice));
    number_choice["choice"]["value"] = json!("wrong");
    assert!(!input.is_valid(&number_choice));
    let response = json!({"data": {"search": [{"name": "Ada"}, {"title": "Team"},
        {"name": "Ada", "title": "Team"}], "node": {"id": "1", "name": "Ada"},
        "choice": {"kind": "number", "value": 3}}});
    assert!(output.is_valid(&response));
    for pointer in ["/data/search/0/name", "/data/node/id", "/data/choice/value"] {
        let mut invalid = response.clone();
        *invalid.pointer_mut(pointer).unwrap() = json!(false);
        assert!(!output.is_valid(&invalid), "accepted invalid {pointer}");
    }
    assert!(output.is_valid(&json!({"errors": [{"message": "failed", "path": ["search", 0]}]})));
    assert!(!output.is_valid(&json!({"errors": [{"message": "failed", "path": [false]}]})));
}

#[test]
fn generated_schemas_validate_complex_inputs_and_outputs() {
    check_tool(&wire_tool(&fixture()));
}

#[tokio::test]
async fn tools_list_preserves_generated_2020_12_schemas() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let running = fixture();
        let expected = wire_tool(&running);
        let (server_io, client_io) = tokio::io::duplex(4096);
        let server = AbortOnDropHandle::new(tokio::spawn(async move {
            running
                .for_service()
                .serve(server_io)
                .await
                .unwrap()
                .waiting()
                .await
                .unwrap();
        }));
        let client = ().serve(client_io).await.unwrap();
        let tools = client.list_all_tools().await.unwrap();
        assert_eq!(tools.len(), 1);
        let actual = serde_json::to_value(&tools[0]).unwrap();
        assert_eq!(actual["inputSchema"], expected["inputSchema"]);
        assert_eq!(actual["outputSchema"], expected["outputSchema"]);
        check_tool(&actual);
        client.cancel().await.unwrap();
        server.await.unwrap();
    })
    .await
    .expect("MCP lifecycle timed out");
}

proptest! {
    #[test]
    fn nested_non_null_integer_lists_match_their_value_domain(
        rows in prop::collection::vec(prop::collection::vec(prop::option::of(any::<i32>()), 0..5), 0..5),
        children_null in any::<bool>(),
    ) {
        let tool = wire_tool(&fixture());
        let validator = validator(&tool["inputSchema"]);
        let children = if children_null { Value::Null } else { json!([]) };
        let value = json!({"filter": {"status": "OPEN", "matrix": rows, "children": children}});
        // [[Int!]!]! admits empty lists but never a null integer element.
        let expected = rows.iter().flatten().all(Option::is_some);
        prop_assert_eq!(validator.is_valid(&value), expected);
    }
}

#[rstest::rstest]
#[case(json!({"type": "array", "items": {"type": "integer"}}), json!([1, 2]), json!(["wrong"]))]
#[case(json!({"type": "integer"}), json!(42), json!("wrong"))]
fn sdk_preserves_non_object_output_contracts(
    #[case] schema: Value,
    #[case] value: Value,
    #[case] invalid: Value,
) {
    // SDK compatibility only: Apollo's generated GraphQL output keeps its response envelope.
    let tool = Tool::new(
        "example",
        "example",
        json!({"type": "object"}).as_object().unwrap().clone(),
    )
    .with_raw_output_schema(Arc::new(schema.as_object().unwrap().clone()));
    let decoded: Tool = serde_json::from_value(serde_json::to_value(&tool).unwrap()).unwrap();
    let decoded_schema = serde_json::to_value(decoded.output_schema.unwrap()).unwrap();
    assert_eq!(decoded_schema, schema);
    let result = CallToolResult::structured(value.clone());
    let decoded: CallToolResult =
        serde_json::from_value(serde_json::to_value(result).unwrap()).unwrap();
    assert_eq!(decoded.structured_content.as_ref(), Some(&value));
    let validator = validator(&decoded_schema);
    assert!(validator.is_valid(decoded.structured_content.as_ref().unwrap()));
    assert!(!validator.is_valid(&invalid));
}

#[test]
fn repeated_non_null_selections_produce_valid_required_keywords() {
    let tool = wire_tool(&fixture_with_query("query Repeated { node { id id } }"));
    let output = validator(&tool["outputSchema"]);
    assert!(output.is_valid(&json!({"data": {"node": {"id": "1"}}})));
    assert!(!output.is_valid(&json!({"data": {"node": {}}})));
}
