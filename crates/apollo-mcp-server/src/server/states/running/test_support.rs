use axum::body::Body;
use serde_json::Value;

pub(super) struct SseEvent {
    pub(super) id: Option<String>,
    pub(super) message: Value,
}

pub(super) async fn next_message(reader: &mut SseReader) -> Value {
    reader.next_event().await.message
}

// Keep unread bytes across events so a frame containing multiple SSE
// events cannot discard the event following the one a test observes.
pub(super) struct SseReader {
    body: Body,
    buffer: Vec<u8>,
}

impl SseReader {
    pub(super) async fn next_notification(&mut self) -> String {
        let event = self.next_event().await;
        assert_eq!(event.message["method"], "notifications/tools/list_changed");
        event
            .id
            .expect("legacy notifications must have an SSE event ID")
    }

    pub(super) fn new(body: Body) -> Self {
        Self {
            body,
            buffer: Vec::new(),
        }
    }

    pub(super) async fn next_event(&mut self) -> SseEvent {
        use http_body_util::BodyExt as _;

        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let boundary = self
                    .buffer
                    .windows(2)
                    .position(|bytes| bytes == b"\n\n")
                    .map(|position| (position, 2))
                    .into_iter()
                    .chain(
                        self.buffer
                            .windows(4)
                            .position(|bytes| bytes == b"\r\n\r\n")
                            .map(|position| (position, 4)),
                    )
                    .min_by_key(|(position, _)| *position);
                if let Some((position, delimiter_len)) = boundary {
                    let event: Vec<_> = self.buffer.drain(..position + delimiter_len).collect();
                    let event = std::str::from_utf8(&event).unwrap();
                    let mut id = None;
                    let mut data = Vec::new();
                    for line in event.lines() {
                        if let Some(value) = line.strip_prefix("id:") {
                            id = Some(value.strip_prefix(' ').unwrap_or(value).to_owned());
                        } else if let Some(value) = line.strip_prefix("data:") {
                            data.push(value.strip_prefix(' ').unwrap_or(value));
                        }
                    }
                    let data = data.join("\n");
                    if data.is_empty() {
                        continue; // Ignore keep-alive and retry-only events.
                    }
                    let message: Value = serde_json::from_str(&data).unwrap();
                    return SseEvent { id, message };
                }
                let frame = self
                    .body
                    .frame()
                    .await
                    .expect("notification stream closed")
                    .unwrap();
                if let Some(data) = frame.data_ref() {
                    self.buffer.extend_from_slice(data);
                }
            }
        })
        .await
        .expect("expected a complete tool-list notification")
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
