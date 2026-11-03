//! Minimal Server-Sent Events decoder over a byte stream.

use futures::{Stream, StreamExt};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    pub event: Option<String>,
    pub data: String,
}

/// Incremental SSE parser. Feed bytes, take complete events.
#[derive(Default)]
pub struct SseParser {
    buf: Vec<u8>,
    event: Option<String>,
    data: Vec<String>,
}

impl SseParser {
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        while let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
            let mut line: Vec<u8> = self.buf.drain(..=pos).collect();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            let line = String::from_utf8_lossy(&line);
            if line.is_empty() {
                if !self.data.is_empty() || self.event.is_some() {
                    out.push(SseEvent {
                        event: self.event.take(),
                        data: std::mem::take(&mut self.data).join("\n"),
                    });
                }
                continue;
            }
            if line.starts_with(':') {
                continue;
            }
            let (field, value) = match line.split_once(':') {
                Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
                None => (line.as_ref(), ""),
            };
            match field {
                "event" => self.event = Some(value.to_owned()),
                "data" => self.data.push(value.to_owned()),
                _ => {}
            }
        }
        out
    }
}

/// Decode a response body into SSE events.
pub fn events(
    resp: reqwest::Response,
) -> impl Stream<Item = Result<SseEvent, reqwest::Error>> + Send {
    let mut parser = SseParser::default();
    resp.bytes_stream()
        .map(move |chunk| chunk.map(|bytes| parser.feed(&bytes)))
        .flat_map(|res| {
            futures::stream::iter(match res {
                Ok(evs) => evs.into_iter().map(Ok).collect::<Vec<_>>(),
                Err(e) => vec![Err(e)],
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_across_chunk_boundaries() {
        let mut p = SseParser::default();
        assert!(p.feed(b"event: ping\nda").is_empty());
        let evs = p.feed(b"ta: {\"a\":1}\r\n\r\n: comment\ndata: x\ndata: y\n\n");
        assert_eq!(
            evs,
            [
                SseEvent {
                    event: Some("ping".into()),
                    data: "{\"a\":1}".into()
                },
                SseEvent {
                    event: None,
                    data: "x\ny".into()
                },
            ]
        );
    }
}
