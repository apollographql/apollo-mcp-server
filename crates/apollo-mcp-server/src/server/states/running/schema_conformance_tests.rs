//! SEP-2106 coverage of generated GraphQL schemas and their MCP wire representation.
use std::{collections::HashMap, fmt::Write as _, sync::Arc, time::Duration};

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
    interface Named { name: String! }
    type User implements Node & Named { id: ID!, name: String!, friends: [SearchResult!]! }
    type Team implements Node { id: ID!, title: String!, nickname: String }
    union SearchResult = User | Team
    type Query {
        search(filter: Filter!, choice: Choice, count: Int!): [SearchResult!]!
        node: Node!
        user: User!
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
    fixture_with_schema(SCHEMA, query)
}

fn fixture_with_schema(schema_source: &str, query: &str) -> Running {
    let schema =
        apollo_compiler::Schema::parse_and_validate(schema_source, "schema.graphql").unwrap();
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
    assert_eq!(
        tool["inputSchema"]["properties"]["filter"]["$ref"],
        "#/definitions/Filter"
    );
    assert!(
        tool["inputSchema"]["properties"]["choice"]["anyOf"].is_array(),
        "nullable custom scalar needs anyOf"
    );
    let conditional = &tool["inputSchema"]["definitions"]["Choice"]["allOf"][0];
    for keyword in ["if", "then", "else"] {
        assert!(conditional[keyword].is_object(), "missing {keyword}");
    }
    assert!(
        tool["outputSchema"]["properties"]["data"]["properties"]["search"]["items"]["allOf"][1]["anyOf"]
            .is_array(), "union members need alternatives"
    );
    assert!(tool["outputSchema"]["properties"]["errors"]["items"]["properties"]["path"]["items"]["oneOf"].is_array(), "error paths accept string or integer segments");
    let input = validator(&tool["inputSchema"]);
    let output = validator(&tool["outputSchema"]);
    let valid_input = json!({"filter": {"status": "OPEN", "matrix": [[1, 2], []],
        "children": [{"status": "CLOSED", "matrix": []}]},
        "choice": {"kind": "text", "value": "hello"}});
    assert!(input.is_valid(&valid_input), "valid nested input rejected");
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
    assert!(!input.is_valid(&json!({})), "required filter omitted");
    let mut invalid_enum = valid_input.clone();
    invalid_enum["filter"]["status"] = json!("UNKNOWN");
    assert!(!input.is_valid(&invalid_enum), "unknown enum accepted");
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
        assert!(
            input.is_valid(&explicit_default),
            "explicit default rejected at {pointer}"
        );
        *explicit_default.pointer_mut(pointer).unwrap() = Value::Null;
        assert!(
            !input.is_valid(&explicit_default),
            "null accepted at {pointer}"
        );
    }
    let mut recursive = valid_input.clone();
    recursive["filter"]["children"][0]["children"] = json!([{
        "status": "OPEN", "matrix": [], "children": [{"status": "CLOSED", "matrix": []}]
    }]);
    assert!(input.is_valid(&recursive), "valid recursive input rejected");
    recursive["filter"]["children"][0]["children"][0]["children"][0]["status"] = json!("UNKNOWN");
    assert!(!input.is_valid(&recursive), "invalid nested enum accepted");
    let mut nullable = valid_input.clone();
    nullable["filter"]["children"] = Value::Null;
    nullable["choice"] = Value::Null;
    assert!(input.is_valid(&nullable), "nullable fields rejected");
    nullable["filter"]["children"] = json!([null]);
    assert!(!input.is_valid(&nullable), "null list member accepted");
    let mut number_choice = valid_input.clone();
    number_choice["choice"] = json!({"kind": "number", "value": 7});
    assert!(
        input.is_valid(&number_choice),
        "integer scalar variant rejected"
    );
    number_choice["choice"]["value"] = json!("wrong");
    assert!(
        !input.is_valid(&number_choice),
        "wrong scalar variant accepted"
    );
    let response = json!({"data": {"search": [{"name": "Ada"}, {"title": "Team"}],
        "node": {"id": "1", "name": "Ada"},
        "choice": {"kind": "number", "value": 3}}});
    assert!(
        output.is_valid(&response),
        "valid GraphQL response rejected"
    );
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

#[test]
fn union_schema_accepts_members_without_a_matching_fragment() {
    let tool = wire_tool(&fixture_with_query(
        "query PartialUnion { search(filter: {status: OPEN, matrix: []}, count: 1) { ... on User { name } } }",
    ));
    let output = validator(&tool["outputSchema"]);
    assert!(output.is_valid(&json!({"data": {"search": [{"name": "Ada"}]}})));
    // Team has no selected fields, so its valid GraphQL result is an empty object.
    assert!(output.is_valid(&json!({"data": {"search": [{}]}})));
    assert!(!output.is_valid(&json!({"data": {"search": [{"name": false}]}})));
    assert!(!output.is_valid(&json!({"data": {"search": [false]}})));
}

#[test]
fn union_schema_validates_named_fragment_members() {
    let tool = wire_tool(&fixture_with_query(
        r#"
        query NamedUnion {
            search(filter: {status: OPEN, matrix: []}, count: 1) {
                ...UserFields
                ...TeamFields
            }
        }
        fragment UserFields on User { name }
        fragment TeamFields on Team { title }
        "#,
    ));
    let output = validator(&tool["outputSchema"]);
    assert!(output.is_valid(&json!({"data": {"search": [{"name": "Ada"}, {"title": "Team"}]}})));
    assert!(!output.is_valid(&json!({"data": {"search": [{"name": false}]}})));
    assert!(!output.is_valid(&json!({"data": {"search": [{"title": false}]}})));
    assert!(!output.is_valid(&json!({"data": {"search": [{"name": "Ada", "title": "Team"}]}})));
}

#[test]
fn unique_member_key_enforces_its_own_field_schema() {
    let tool = wire_tool(&fixture_with_query(
        "query NullableAlternative { search(filter: {status: OPEN, matrix: []}, count: 1) { ... on User { name } ... on Team { nickname } } }",
    ));
    let output = validator(&tool["outputSchema"]);
    assert!(
        output.is_valid(&json!({"data": {"search": [{"name": "Ada"}, {"nickname": "T"}, {}]}}))
    );
    assert!(!output.is_valid(&json!({"data": {"search": [{"name": false}]}})));
    assert!(!output.is_valid(&json!({"data": {"search": [{"nickname": false}]}})));
}

#[test]
fn union_schema_applies_interface_fragment_to_all_members() {
    let tool = wire_tool(&fixture_with_query(
        "query InterfaceUnion { search(filter: {status: OPEN, matrix: []}, count: 1) { ... on Node { id } } }",
    ));
    let output = validator(&tool["outputSchema"]);
    assert!(output.is_valid(&json!({"data": {"search": [{"id": "user"}, {"id": "team"}]}})));
    assert!(!output.is_valid(&json!({"data": {"search": [{}]}})));
}

#[test]
fn union_interface_fragment_applies_to_only_matching_members() {
    let tool = wire_tool(&fixture_with_query(
        "query NamedSubset { search(filter: {status: OPEN, matrix: []}, count: 1) { ... on Named { name } } }",
    ));
    let output = validator(&tool["outputSchema"]);
    assert!(output.is_valid(&json!({"data": {"search": [{"name": "Ada"}, {}]}})));
    assert!(!output.is_valid(&json!({"data": {"search": [{"name": false}]}})));
}

#[test]
fn multiple_fragments_on_one_member_constrain_every_selected_field() {
    let tool = wire_tool(&fixture_with_query(
        r#"
        query Combined {
            search(filter: {status: OPEN, matrix: []}, count: 1) {
                ...UserId
                ... on User { name }
            }
        }
        fragment UserId on User { id }
        "#,
    ));
    let output = validator(&tool["outputSchema"]);
    assert!(output.is_valid(&json!({"data": {"search": [{"id": "1", "name": "Ada"}, {}]}})));
    for invalid in [
        json!({"name": "Ada"}),
        json!({"id": "1"}),
        json!({"id": false, "name": "Ada"}),
        json!({"id": "1", "name": false}),
    ] {
        assert!(
            !output.is_valid(&json!({"data": {"search": [invalid.clone()]}})),
            "accepted {invalid}"
        );
    }
}

#[test]
fn overlapping_member_keys_constrain_every_present_field() {
    let tool = wire_tool(&fixture_with_schema(
        r#"
        type First { a: String!, b: String! }
        type Second { a: String!, c: String! }
        type Third { b: String!, c: String! }
        union Result = First | Second | Third
        type Query { result: Result! }
        "#,
        r#"
        query Overlap {
            result {
                ... on First { a b }
                ... on Second { a c }
                ... on Third { b c }
            }
        }
        "#,
    ));
    let output = validator(&tool["outputSchema"]);
    for response in [
        json!({"a": "ok", "b": "ok"}),
        json!({"a": "ok", "c": "ok"}),
        json!({"b": "ok", "c": "ok"}),
        json!({"a": "ok", "b": "ok", "extra": true}),
    ] {
        assert!(output.is_valid(&json!({"data": {"result": response}})));
    }
    assert!(!output.is_valid(&json!({"data": {"result": {
        "a": false, "b": "ok", "c": "ok"
    }}})));
    assert!(!output.is_valid(&json!({"data": {"result": {
        "a": "ok", "b": "ok", "c": "ok"
    }}})));
}

#[test]
fn type_less_inline_fragment_reaches_nested_named_spread() {
    let tool = wire_tool(&fixture_with_query(
        r#"
        query Nested {
            search(filter: {status: OPEN, matrix: []}, count: 1) {
                ... { ...UserFields }
            }
        }
        fragment UserFields on User { name }
        "#,
    ));
    let output = validator(&tool["outputSchema"]);
    assert!(output.is_valid(&json!({"data": {"search": [{"name": "Ada"}, {}]}})));
    assert!(!output.is_valid(&json!({"data": {"search": [{"name": false}]}})));
}

#[test]
fn nested_spreads_preserve_non_null_required_fields() {
    let tool = wire_tool(&fixture_with_query(
        r#"
        query NestedObject { user { ...UserFields } }
        fragment UserFields on User { name ...UserId }
        fragment UserId on User { id }
        "#,
    ));
    let output = validator(&tool["outputSchema"]);
    assert!(output.is_valid(&json!({"data": {"user": {"name": "Ada", "id": "1"}}})));
    assert!(!output.is_valid(&json!({"data": {"user": {"name": "Ada"}}})));
    assert!(!output.is_valid(&json!({"data": {"user": {"id": "1"}}})));
}

#[test]
fn interface_schema_applies_member_fragments_and_direct_fields() {
    let tool = wire_tool(&fixture_with_query(
        r#"
        query InterfaceMembers {
            node { id ... on User { name } ... on Team { title } }
        }
        "#,
    ));
    let output = validator(&tool["outputSchema"]);
    assert!(output.is_valid(&json!({"data": {"node": {"id": "1", "name": "Ada"}}})));
    assert!(output.is_valid(&json!({"data": {"node": {"id": "2", "title": "Team"}}})));
    assert!(!output.is_valid(&json!({"data": {"node": {"id": false, "name": "Ada"}}})));
    assert!(!output.is_valid(&json!({"data": {"node": {"id": "1", "name": false}}})));
    assert!(!output.is_valid(&json!({"data": {"node": {"id": "2", "title": false}}})));
}

#[test]
fn union_common_typename_is_validated_for_uncovered_members() {
    let tool = wire_tool(&fixture_with_query(
        "query Common { search(filter: {status: OPEN, matrix: []}, count: 1) { __typename ... on User { name } } }",
    ));
    let output = validator(&tool["outputSchema"]);
    assert!(output.is_valid(&json!({"data": {"search": [{"__typename": "Team"}]}})));
    assert!(!output.is_valid(&json!({"data": {"search": [{"__typename": false}]}})));
}

#[test]
fn large_interface_groups_identical_fragment_patterns() {
    fn generated_schema(member_count: usize) -> Value {
        let mut source = String::from(
            "interface Node { id: ID! } interface Special { special: String! } type Query { node: Node! }\n",
        );
        for index in 0..member_count {
            let implements = if index == 0 { "Node" } else { "Node & Special" };
            writeln!(
                source,
                "type Member{index} implements {implements} {{ id: ID!, special: String! }}"
            )
            .unwrap();
        }
        let query = "query Scale { node { id ... on Special { special } } }";
        wire_tool(&fixture_with_schema(&source, query))["outputSchema"].clone()
    }

    let small = generated_schema(20);
    let large = generated_schema(1_000);
    let small_size = serde_json::to_vec(&small).unwrap().len();
    let large_size = serde_json::to_vec(&large).unwrap().len();
    assert_eq!(
        large_size, small_size,
        "schema size grew with identical implementers"
    );
    let output = validator(&large);
    assert!(output.is_valid(&json!({"data": {"node": {"id": "2"}}})));
    assert!(!output.is_valid(&json!({"data": {"node": {"id": "1", "special": false}}})));
}

#[test]
fn distinct_member_fragments_scale_with_selected_fields() {
    fn generated_schema(member_count: usize) -> Value {
        let mut source = String::from("interface Node { id: ID! } type Query { node: Node! }\n");
        let mut query = String::from("query Scale { node { id ");
        for index in 0..member_count {
            writeln!(
                source,
                "type Member{index} implements Node {{ id: ID!, field{index}: String! }}"
            )
            .unwrap();
            write!(query, "... on Member{index} {{ field{index} }} ").unwrap();
        }
        query.push_str("} }");
        wire_tool(&fixture_with_schema(&source, &query))["outputSchema"].clone()
    }

    let small = generated_schema(20);
    let large = generated_schema(200);
    let small_size = serde_json::to_vec(&small).unwrap().len();
    let large_size = serde_json::to_vec(&large).unwrap().len();
    assert!(
        large_size < small_size * 15,
        "output grew faster than selected fields: {small_size} -> {large_size}"
    );
    jsonschema::draft202012::meta::validate(&large).unwrap();
}

#[test]
fn nested_union_schema_size_grows_with_depth() {
    fn selection(depth: usize) -> String {
        if depth == 0 {
            "... on User { name } ... on Team { title }".to_string()
        } else {
            format!(
                "... on User {{ name friends {{ {} }} }} ... on Team {{ title }}",
                selection(depth - 1)
            )
        }
    }
    fn output_schema(depth: usize) -> Value {
        let query = format!(
            "query Deep {{ search(filter: {{status: OPEN, matrix: []}}, count: 1) {{ {} }} }}",
            selection(depth)
        );
        wire_tool(&fixture_with_query(&query))["outputSchema"].clone()
    }

    let sizes: Vec<_> = (1..=4)
        .map(|depth| serde_json::to_vec(&output_schema(depth)).unwrap().len())
        .collect();
    let first_growth = sizes[1] - sizes[0];
    let last_growth = sizes[3] - sizes[2];
    assert!(
        last_growth < first_growth * 2,
        "nested schema grew exponentially: {sizes:?}"
    );
    let deep = output_schema(4);
    let output = validator(&deep);
    assert!(output.is_valid(
        &json!({"data": {"search": [{"name": "Ada", "friends": []}, {"title": "Team"}]}})
    ));
}

#[test]
fn nested_field_shared_by_member_patterns_grows_with_depth() {
    const SCHEMA: &str = r#"
        interface Node { id: ID! }
        interface Shared { children: [Node!]! }
        type A implements Node & Shared { id: ID!, children: [Node!]!, a: String! }
        type B implements Node & Shared { id: ID!, children: [Node!]! }
        type C implements Node { id: ID! }
        type Query { node: Node! }
    "#;

    fn selection(depth: usize) -> String {
        if depth == 0 {
            "... on A { a }".to_string()
        } else {
            format!(
                "... on Shared {{ children {{ {} }} }} ... on A {{ a }}",
                selection(depth - 1)
            )
        }
    }

    fn output_schema(depth: usize) -> Value {
        let query = format!("query DeepShared {{ node {{ {} }} }}", selection(depth));
        wire_tool(&fixture_with_schema(SCHEMA, &query))["outputSchema"].clone()
    }

    let sizes: Vec<_> = (1..=4)
        .map(|depth| serde_json::to_vec(&output_schema(depth)).unwrap().len())
        .collect();
    let first_growth = sizes[1] - sizes[0];
    let last_growth = sizes[3] - sizes[2];
    assert!(
        last_growth < first_growth * 2,
        "shared nested field grew exponentially: {sizes:?}"
    );
    let output = validator(&output_schema(4));
    assert!(output.is_valid(&json!({"data": {"node": {"children": [], "a": "ok"}}})));
    assert!(output.is_valid(&json!({"data": {"node": {"children": []}}})));
    assert!(output.is_valid(&json!({"data": {"node": {}}})));
    assert!(!output.is_valid(&json!({"data": {"node": {"children": false}}})));
}
