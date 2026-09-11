use axum::body::Body;
use futures::StreamExt as _;
use serde_json::Value;
use sse_stream::SseStream;

use super::*;

pub(super) fn create_test_running() -> Running {
    let schema =
        apollo_compiler::Schema::parse_and_validate("type Query { hello: String }", "test")
            .unwrap();
    Running {
        schema: Arc::new(RwLock::new(schema)),
        operations: Arc::new(RwLock::new(vec![])),
        apps: vec![],
        prompts: vec![],
        headers: http::HeaderMap::new(),
        forward_headers: vec![],
        endpoint: url::Url::parse("http://localhost:4000").unwrap(),
        execute_tool: None,
        introspect_tool: None,
        search_tool: None,
        explorer_tool: None,
        validate_tool: None,
        custom_scalar_map: None,
        tool_list_changes: Default::default(),
        cancellation_token: CancellationToken::new(),
        mutation_mode: MutationMode::All,
        disable_type_description: false,
        disable_schema_description: false,
        enable_output_schema: false,
        disable_auth_token_passthrough: false,
        descriptions: HashMap::new(),
        annotations: HashMap::new(),
        health_check: None,
        server_info: Default::default(),
        instructions: None,
        rhai_engine: Arc::new(parking_lot::Mutex::new(RhaiEngine::new("rhai"))),
        caching: Default::default(),
    }
}

pub(super) struct SseEvent {
    pub(super) id: Option<String>,
    pub(super) message: Value,
}

pub(super) async fn next_message(reader: &mut SseReader) -> Value {
    reader.next_event().await.message
}

pub(super) struct SseReader {
    stream: SseStream<Body>,
}

impl SseReader {
    pub(super) fn new(body: Body) -> Self {
        Self {
            stream: SseStream::new(body),
        }
    }

    pub(super) async fn next_notification(&mut self) -> String {
        let event = self.next_event().await;
        assert_eq!(event.message["method"], "notifications/tools/list_changed");
        event
            .id
            .expect("legacy notifications must have an SSE event ID")
    }

    pub(super) async fn next_event(&mut self) -> SseEvent {
        loop {
            let event = self
                .stream
                .next()
                .await
                .expect("SSE stream closed before the expected message")
                .expect("invalid SSE event");
            let Some(data) = event.data.filter(|data| !data.is_empty()) else {
                continue; // Ignore keep-alive and retry-only events.
            };
            return SseEvent {
                id: event.id,
                message: serde_json::from_str(&data).expect("SSE data must contain a JSON message"),
            };
        }
    }
}

proptest::proptest! {
    #[test]
    fn notification_stream_preserves_events_across_frame_boundaries(
        chunk_sizes in proptest::collection::vec(1usize..128, 1..16),
        crlf in proptest::bool::ANY,
    ) {
        // Framing must not affect event identity, including when several
        // events share a frame or an event is split between frames.
        let newline = if crlf { "\r\n" } else { "\n" };
        let payload = format!(
            ": keep-alive{newline}{newline}data:{newline}{newline}id: first{newline}data: {{\"method\":\"notifications/tools/list_changed\"}}{newline}{newline}id: second{newline}data: {{\"method\":\"notifications/tools/list_changed\"}}{newline}{newline}"
        );
        let mut remaining = payload.as_bytes();
        let mut chunks = Vec::new();
        for size in chunk_sizes.into_iter().cycle() {
            if remaining.is_empty() {
                break;
            }
            let (chunk, rest) = remaining.split_at(size.min(remaining.len()));
            chunks.push(Ok::<_, std::convert::Infallible>(chunk.to_vec()));
            remaining = rest;
        }
        let runtime = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
        runtime.block_on(async {
            let body = Body::from_stream(futures::stream::iter(chunks));
            let mut stream = SseReader::new(body);
            assert_eq!(stream.next_notification().await, "first");
            assert_eq!(stream.next_notification().await, "second");
        });
    }
}
