use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    Json(Value),
    Done,
}

#[derive(Debug, Default)]
pub struct SseDecoder {
    buffer: String,
    data_lines: Vec<String>,
}

impl SseDecoder {
    pub fn push(&mut self, bytes: &[u8]) -> Vec<StreamEvent> {
        self.buffer.push_str(&String::from_utf8_lossy(bytes));
        let mut events = Vec::new();

        while let Some(newline_pos) = self.buffer.find('\n') {
            let mut line = self.buffer[..newline_pos].to_string();
            self.buffer.drain(..=newline_pos);
            if line.ends_with('\r') {
                line.pop();
            }
            self.process_line(&line, &mut events);
        }

        events
    }

    pub fn finish(&mut self) -> Vec<StreamEvent> {
        let mut events = Vec::new();
        if !self.buffer.is_empty() {
            let line = self.buffer.trim_end_matches('\r').to_string();
            self.buffer.clear();
            self.process_line(&line, &mut events);
        }
        self.flush_event(&mut events);
        events
    }

    fn process_line(&mut self, line: &str, events: &mut Vec<StreamEvent>) {
        if line.is_empty() {
            self.flush_event(events);
            return;
        }
        if line.starts_with(':') {
            return;
        }
        if let Some(data) = line.strip_prefix("data:") {
            self.data_lines.push(data.trim_start().to_string());
        }
    }

    fn flush_event(&mut self, events: &mut Vec<StreamEvent>) {
        if self.data_lines.is_empty() {
            return;
        }
        let payload = self.data_lines.join("\n");
        self.data_lines.clear();
        let trimmed = payload.trim();
        if trimmed == "[DONE]" {
            events.push(StreamEvent::Done);
            return;
        }
        if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
            events.push(StreamEvent::Json(value));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn accepts_data_without_space_and_crlf() {
        let mut decoder = SseDecoder::default();
        let events = decoder.push(b"data:{\"x\":1}\r\n\r\n");
        assert_eq!(events, vec![StreamEvent::Json(json!({"x": 1}))]);
    }

    #[test]
    fn handles_split_chunks_and_done() {
        let mut decoder = SseDecoder::default();
        assert!(decoder.push(b"data: {\"x\":").is_empty());
        let events = decoder.push(b"2}\n\ndata: [DONE]\n\n");
        assert_eq!(
            events,
            vec![StreamEvent::Json(json!({"x": 2})), StreamEvent::Done]
        );
    }

    #[test]
    fn joins_multiline_data_events() {
        let mut decoder = SseDecoder::default();
        let events = decoder.push(b"data: {\"a\":\ndata: 1}\n\n");
        assert_eq!(events, vec![StreamEvent::Json(json!({"a": 1}))]);
    }
}
