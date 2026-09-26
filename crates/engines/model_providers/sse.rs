//! SSE（`text/event-stream`）解析 —— M4 新增的 reqwest 流式能力。
//!
//! Python 侧无对应实现（httpx 网关只做一次性请求）；这是 `GOAL_PROMPT`
//! M4 明确要求的 Rust 侧能力，供流式生成与 M7 的 axum SSE 转发复用。
//! 解析器按 WHATWG Server-Sent Events 规范工作在**字节层**——分块边界
//! 可能切断多字节 UTF-8 字符或 `\r\n` 终结符，逐行解码规避两者。
//!
//! 规范要点：
//! - 行终结符 `\n` / `\r\n` / `\r` 三种皆接受；
//! - 空行派发事件；`data` 多行以 `\n` 连接；
//! - `:` 开头为注释行；字段名后冒号至多剥离一个前导空格；
//! - `data` 为空的事件不派发；`id` 含 NUL 时忽略；未知字段忽略。

/// 一条解析完成的 SSE 事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    /// `event:` 字段（未设置为 `None`）。
    pub event: Option<String>,
    /// `data:` 各行以 `\n` 连接的正文。
    pub data: String,
    /// `id:` 字段（未设置或含 NUL 为 `None`）。
    pub id: Option<String>,
}

#[derive(Debug, Default)]
struct PendingEvent {
    event: Option<String>,
    id: Option<String>,
    data_lines: Vec<String>,
}

/// 增量 SSE 解析器：喂入任意切分的字节块，吐出完整事件。
#[derive(Debug, Default)]
pub struct SseParser {
    buffer: Vec<u8>,
    pending: PendingEvent,
}

impl SseParser {
    /// 喂入一块响应字节，返回其中完成的事件。
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        self.buffer.extend_from_slice(chunk);
        let mut events = Vec::new();
        loop {
            // 找最早的 \n 或 \r；末尾孤立 \r 需等待下一块判 \r\n。
            let newline = self.buffer.iter().position(|byte| *byte == b'\n');
            let carriage = self.buffer.iter().position(|byte| *byte == b'\r');
            let terminator = match (newline, carriage) {
                (Some(n), Some(c)) => n.min(c),
                (Some(n), None) => n,
                (None, Some(c)) => c,
                (None, None) => break,
            };
            let is_carriage = self.buffer[terminator] == b'\r';
            if is_carriage && terminator + 1 == self.buffer.len() {
                break;
            }
            let line: Vec<u8> = self.buffer.drain(..terminator).collect();
            let skip = if is_carriage && self.buffer.get(1) == Some(&b'\n') {
                2
            } else {
                1
            };
            self.buffer.drain(..skip);
            let line = String::from_utf8_lossy(&line).into_owned();
            self.process_line(&line, &mut events);
        }
        events
    }

    /// 流结束：把残留缓冲当作最后一行处理，再派发挂起事件。EOF 视作
    /// 行终结——末尾孤立 `\r` 是完整终结符，剥离后再处理最后一行。
    pub fn finish(&mut self) -> Vec<SseEvent> {
        let mut events = Vec::new();
        if self.buffer.last() == Some(&b'\r') {
            self.buffer.pop();
        }
        if !self.buffer.is_empty() {
            let line: Vec<u8> = std::mem::take(&mut self.buffer);
            let line = String::from_utf8_lossy(&line).into_owned();
            self.process_line(&line, &mut events);
        }
        if let Some(event) = self.take_pending() {
            events.push(event);
        }
        events
    }

    fn process_line(&mut self, line: &str, events: &mut Vec<SseEvent>) {
        if line.is_empty() {
            if let Some(event) = self.take_pending() {
                events.push(event);
            }
            return;
        }
        if line.starts_with(':') {
            return;
        }
        let (field, value) = match line.split_once(':') {
            Some((field, rest)) => (field, rest.strip_prefix(' ').unwrap_or(rest)),
            None => (line, ""),
        };
        match field {
            "data" => self.pending.data_lines.push(value.to_string()),
            "event" => self.pending.event = Some(value.to_string()),
            "id" if !value.contains('\u{0}') => self.pending.id = Some(value.to_string()),
            _ => {}
        }
    }

    /// 空事件（无 data 行）不派发（规范行为）；派发后重置挂起状态。
    fn take_pending(&mut self) -> Option<SseEvent> {
        if self.pending.data_lines.is_empty() {
            self.pending.event = None;
            self.pending.id = None;
            return None;
        }
        Some(SseEvent {
            event: self.pending.event.take(),
            id: self.pending.id.take(),
            data: std::mem::take(&mut self.pending.data_lines).join("\n"),
        })
    }
}

/// 从 Chat Completions 流式分块的 `data` JSON 里抽取增量文本
/// （`choices[0].delta.content`）；非 JSON 或无增量返回 `None`。
///
/// keep-alive / 仅含 role 的首块等合法无文本分块都会落到 `None`，
/// 调用方静默跳过——与一次性请求路径的容错语义一致。
#[must_use]
pub fn extract_stream_delta(data: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(data).ok()?;
    let content = value.get("choices")?.as_array()?.first()?.get("delta")?;
    content
        .get("content")
        .and_then(serde_json::Value::as_str)
        .map(String::from)
}

/// 抽取流式分块携带的用量（`GLM` / `DeepSeek` 等会在末块带 `usage`）。
///
/// 返回 `None` = 分块无用量信息；`Some((None, None))` = 有 usage
/// 对象但字段缺失。
#[must_use]
pub fn extract_stream_usage(data: &str) -> Option<(Option<i64>, Option<i64>)> {
    let value: serde_json::Value = serde_json::from_str(data).ok()?;
    let usage = value.get("usage")?;
    if !usage.is_object() {
        return None;
    }
    Some((
        usage
            .get("prompt_tokens")
            .and_then(serde_json::Value::as_i64),
        usage
            .get("completion_tokens")
            .and_then(serde_json::Value::as_i64),
    ))
}

/// 抽取流式分块报告的模型 ID（首块 `model` 字段）；非 JSON 或缺失返回
/// `None`。
#[must_use]
pub fn extract_stream_model(data: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(data).ok()?;
    value
        .get("model")
        .and_then(serde_json::Value::as_str)
        .filter(|model| !model.is_empty())
        .map(String::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_single_event() {
        let mut parser = SseParser::default();
        let events = parser.feed(b"data: hello\n\n");
        assert_eq!(
            events,
            vec![SseEvent {
                event: None,
                data: "hello".to_string(),
                id: None
            }]
        );
    }

    #[test]
    fn joins_multi_line_data_with_newline() {
        let mut parser = SseParser::default();
        let events = parser.feed(b"data: line1\ndata: line2\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "line1\nline2");
    }

    #[test]
    fn accepts_crlf_and_lone_cr_terminators() {
        // \r\n 终结行 + \r\n 终结空行。
        let mut parser = SseParser::default();
        let events = parser.feed(b"data: a\r\n\r\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "a");

        // 块内孤立 \r（后随非 \n 字节）即行终结符。
        let mut parser = SseParser::default();
        let events = parser.feed(b"data: a\r\r\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "a");

        // 块尾孤立 \r 需等下一块判 \r\n；EOF（finish）视作行终结。
        let mut parser = SseParser::default();
        assert!(parser.feed(b"data: a\r\n\r").is_empty());
        let events = parser.finish();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "a");

        // EOF 前的孤立 \r 不混入 data 正文。
        let mut parser = SseParser::default();
        assert!(parser.feed(b"data: tail\r").is_empty());
        let events = parser.finish();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "tail");
    }

    #[test]
    fn waits_for_split_crlf_pair() {
        let mut parser = SseParser::default();
        assert!(parser.feed(b"data: a\r").is_empty());
        let events = parser.feed(b"\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "a");
    }

    #[test]
    fn handles_events_split_across_chunks() {
        let mut parser = SseParser::default();
        assert!(parser.feed(b"dat").is_empty());
        assert!(parser.feed(b"a: hel").is_empty());
        assert!(parser.feed(b"lo\n").is_empty());
        let events = parser.feed(b"\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "hello");
    }

    #[test]
    fn comment_and_unknown_fields_are_ignored() {
        let mut parser = SseParser::default();
        let events = parser.feed(b": keep-alive\nretry: 1000\ndata: x\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "x");
    }

    #[test]
    fn captures_event_and_id_fields() {
        let mut parser = SseParser::default();
        let events = parser.feed(b"event: delta\nid: 42\ndata: payload\n\n");
        assert_eq!(
            events,
            vec![SseEvent {
                event: Some("delta".to_string()),
                data: "payload".to_string(),
                id: Some("42".to_string())
            }]
        );
    }

    #[test]
    fn empty_data_event_is_not_dispatched() {
        let mut parser = SseParser::default();
        let events = parser.feed(b"event: ping\n\n");
        assert!(events.is_empty());
    }

    #[test]
    fn colon_without_space_is_valid_value() {
        let mut parser = SseParser::default();
        let events = parser.feed(b"data:[DONE]\n\n");
        assert_eq!(events[0].data, "[DONE]");
    }

    #[test]
    fn finish_flushes_unterminated_event() {
        let mut parser = SseParser::default();
        assert!(parser.feed(b"data: tail").is_empty());
        let events = parser.finish();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "tail");
    }

    #[test]
    fn extract_stream_delta_reads_choices_delta_content() {
        assert_eq!(
            extract_stream_delta(r#"{"choices":[{"delta":{"content":"he"}}]}"#),
            Some("he".to_string())
        );
        assert_eq!(
            extract_stream_delta(r#"{"choices":[{"delta":{"role":"assistant"}}]}"#),
            None
        );
        assert_eq!(extract_stream_delta("[DONE]"), None);
        assert_eq!(extract_stream_delta("garbage"), None);
    }

    #[test]
    fn extract_stream_usage_reads_usage_object() {
        assert_eq!(
            extract_stream_usage(r#"{"usage":{"prompt_tokens":3,"completion_tokens":1}}"#),
            Some((Some(3), Some(1)))
        );
        assert_eq!(extract_stream_usage(r#"{"choices":[]}"#), None);
    }

    #[test]
    fn extract_stream_model_reads_non_empty_model_field() {
        assert_eq!(
            extract_stream_model(r#"{"model":"glm-4.7","choices":[]}"#),
            Some("glm-4.7".to_string())
        );
        assert_eq!(extract_stream_model(r#"{"model":""}"#), None);
        assert_eq!(extract_stream_model("[DONE]"), None);
    }
}
