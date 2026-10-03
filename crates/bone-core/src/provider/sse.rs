//! Minimal server-sent events parser: yields the `data` of each event.

#[derive(Default)]
pub struct SseParser {
    buf: Vec<u8>,
    data: Option<String>,
}

impl SseParser {
    /// Feed bytes; returns the data payloads of every event completed by them.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<String> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        while let Some(nl) = self.buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=nl).collect();
            let line = String::from_utf8_lossy(&line);
            let line = line.trim_end_matches(['\n', '\r']);
            if line.is_empty() {
                out.extend(self.data.take());
            } else if let Some(value) = field(line, "data") {
                match &mut self.data {
                    Some(data) => {
                        data.push('\n');
                        data.push_str(value);
                    }
                    None => self.data = Some(value.to_owned()),
                }
            }
            // Comments (`:`) and other fields (`event`, `id`, `retry`) are unused.
        }
        out
    }

    /// Flush a final event that was not followed by a blank line.
    pub fn finish(&mut self) -> Option<String> {
        if !self.buf.is_empty() {
            let rest = std::mem::take(&mut self.buf);
            let mut out = self.push(&rest);
            out.extend(self.push(b"\n\n"));
            return out.pop();
        }
        self.data.take()
    }
}

fn field<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    let rest = line.strip_prefix(name)?.strip_prefix(':')?;
    Some(rest.strip_prefix(' ').unwrap_or(rest))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_events_across_chunks() {
        let mut p = SseParser::default();
        assert!(p.push(b"data: {\"a\"").is_empty());
        assert!(p.push(b":1}\r\n").is_empty());
        assert_eq!(
            p.push(b"\r\n: keepalive\n\ndata:x\n\n"),
            vec!["{\"a\":1}", "x"]
        );
    }

    #[test]
    fn joins_multiline_data_and_flushes_tail() {
        let mut p = SseParser::default();
        assert_eq!(p.push(b"event: m\ndata: a\ndata: b\n\n"), vec!["a\nb"]);
        assert!(p.push(b"data: [DONE]").is_empty());
        assert_eq!(p.finish().as_deref(), Some("[DONE]"));
    }
}
