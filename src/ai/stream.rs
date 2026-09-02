use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    Json(Value),
    Done,
}

const MAX_SSE_LINE_BYTES: usize = 1024 * 1024;

#[derive(Debug, Default)]
pub struct SseDecoder {
    buffer: Vec<u8>,
    data_lines: Vec<String>,
}

impl SseDecoder {
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<StreamEvent>, String> {
        self.buffer.extend_from_slice(bytes);
        let mut events = Vec::new();

        while let Some(newline_pos) = self.buffer.iter().position(|byte| *byte == b'\n') {
            if newline_pos > MAX_SSE_LINE_BYTES {
                return Err("provider SSE line exceeded 1 MiB".to_string());
            }
            let mut raw = self.buffer.drain(..=newline_pos).collect::<Vec<_>>();
            raw.pop();
            if raw.last() == Some(&b'\r') {
                raw.pop();
            }
            let line = String::from_utf8(raw)
                .map_err(|_| "provider SSE emitted invalid UTF-8".to_string())?;
            self.process_line(&line, &mut events)?;
        }

        if self.buffer.len() > MAX_SSE_LINE_BYTES {
            return Err("provider SSE line exceeded 1 MiB".to_string());
        }

        Ok(events)
    }

    pub fn finish(&mut self) -> Result<Vec<StreamEvent>, String> {
        let mut events = Vec::new();
        if !self.buffer.is_empty() {
            let mut raw = std::mem::take(&mut self.buffer);
            if raw.last() == Some(&b'\r') {
                raw.pop();
            }
            let line = String::from_utf8(raw)
                .map_err(|_| "provider SSE emitted invalid UTF-8".to_string())?;
            self.process_line(&line, &mut events)?;
        }
        self.flush_event(&mut events)?;
        Ok(events)
    }

    fn process_line(&mut self, line: &str, events: &mut Vec<StreamEvent>) -> Result<(), String> {
        if line.is_empty() {
            return self.flush_event(events);
        }
        if line.starts_with(':') {
            return Ok(());
        }
        if let Some(data) = line.strip_prefix("data:") {
            self.data_lines.push(data.trim_start().to_string());
        }
        Ok(())
    }

    fn flush_event(&mut self, events: &mut Vec<StreamEvent>) -> Result<(), String> {
        if self.data_lines.is_empty() {
            return Ok(());
        }
        let payload = self.data_lines.join("\n");
        self.data_lines.clear();
        let trimmed = payload.trim();
        if trimmed == "[DONE]" {
            events.push(StreamEvent::Done);
            return Ok(());
        }
        let value = serde_json::from_str::<Value>(trimmed)
            .map_err(|error| format!("provider SSE emitted invalid JSON: {error}"))?;
        events.push(StreamEvent::Json(value));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn accepts_data_without_space_and_crlf() {
        let mut decoder = SseDecoder::default();
        let events = decoder.push(b"data:{\"x\":1}\r\n\r\n").unwrap();
        assert_eq!(events, vec![StreamEvent::Json(json!({"x": 1}))]);
    }

    #[test]
    fn handles_split_chunks_and_done() {
        let mut decoder = SseDecoder::default();
        assert!(decoder.push(b"data: {\"x\":").unwrap().is_empty());
        let events = decoder.push(b"2}\n\ndata: [DONE]\n\n").unwrap();
        assert_eq!(
            events,
            vec![StreamEvent::Json(json!({"x": 2})), StreamEvent::Done]
        );
    }

    #[test]
    fn joins_multiline_data_events() {
        let mut decoder = SseDecoder::default();
        let events = decoder.push(b"data: {\"a\":\ndata: 1}\n\n").unwrap();
        assert_eq!(events, vec![StreamEvent::Json(json!({"a": 1}))]);
    }

    #[test]
    fn preserves_multibyte_utf8_across_every_chunk_boundary() {
        let wire = "data: {\"text\":\"Halo 😀 — 中文 — العربية\"}\r\n\r\n";
        let expected = StreamEvent::Json(json!({
            "text": "Halo 😀 — 中文 — العربية"
        }));
        for split in 1..wire.len() {
            let mut decoder = SseDecoder::default();
            let mut events = decoder.push(&wire.as_bytes()[..split]).unwrap();
            events.extend(decoder.push(&wire.as_bytes()[split..]).unwrap());
            assert_eq!(events, vec![expected.clone()], "split at byte {split}");
        }
    }

    #[test]
    fn rejects_malformed_json_events() {
        let mut decoder = SseDecoder::default();
        let error = decoder.push(b"data: {not-json}\n\n").unwrap_err();
        assert!(error.contains("invalid JSON"));
    }

    #[test]
    fn rejects_invalid_utf8_and_oversized_lines() {
        let mut invalid = SseDecoder::default();
        assert!(invalid.push(b"data: \xff\n").is_err());

        let mut oversized = SseDecoder::default();
        assert!(oversized.push(&vec![b'x'; MAX_SSE_LINE_BYTES + 1]).is_err());
    }
}
