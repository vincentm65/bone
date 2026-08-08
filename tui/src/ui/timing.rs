//! Filtering for provider-injected timing metadata that must not reach the TUI.

const OPEN: &str = "<timing>";
const CLOSE: &str = "</timing>";

/// Remove complete timing blocks from an assistant message before rendering it.
pub(crate) fn strip_timing_blocks(input: &str) -> String {
    let mut rest = input;
    let mut output = String::with_capacity(input.len());
    while let Some(start) = rest.find(OPEN) {
        output.push_str(&rest[..start]);
        rest = &rest[start + OPEN.len()..];
        let Some(end) = rest.find(CLOSE) else {
            return output;
        };
        rest = &rest[end + CLOSE.len()..];
    }
    output.push_str(rest);
    output
}

/// Streaming counterpart that handles tags split across arbitrary text deltas.
#[derive(Debug, Default)]
pub(crate) struct TimingBlockFilter {
    pending: String,
    in_block: bool,
}

impl TimingBlockFilter {
    pub(crate) fn push(&mut self, text: &str) -> String {
        self.pending.push_str(text);
        let mut output = String::new();

        loop {
            if self.in_block {
                if let Some(end) = self.pending.find(CLOSE) {
                    self.pending.drain(..end + CLOSE.len());
                    self.in_block = false;
                    continue;
                }
                let keep = partial_suffix_len(&self.pending, CLOSE);
                if self.pending.len() > keep {
                    self.pending.drain(..self.pending.len() - keep);
                }
                break;
            }

            if let Some(start) = self.pending.find(OPEN) {
                output.push_str(&self.pending[..start]);
                self.pending.drain(..start + OPEN.len());
                self.in_block = true;
                continue;
            }

            let keep = partial_suffix_len(&self.pending, OPEN);
            let emit = self.pending.len() - keep;
            output.push_str(&self.pending[..emit]);
            self.pending.drain(..emit);
            break;
        }

        output
    }

    /// Finish a stream, preserving an incomplete ordinary opening marker while
    /// suppressing an unterminated timing block.
    pub(crate) fn finish(&mut self) -> String {
        let output = if self.in_block {
            String::new()
        } else {
            std::mem::take(&mut self.pending)
        };
        self.reset();
        output
    }

    pub(crate) fn reset(&mut self) {
        self.pending.clear();
        self.in_block = false;
    }
}

fn partial_suffix_len(text: &str, marker: &str) -> usize {
    (1..marker.len())
        .rev()
        .find(|&len| text.ends_with(&marker[..len]))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_complete_blocks_and_preserves_surrounding_text() {
        assert_eq!(
            strip_timing_blocks("answer\n\n<timing>Message timestamp: now.</timing>after"),
            "answer\n\nafter"
        );
    }

    #[test]
    fn streaming_filter_strips_tags_split_at_every_byte() {
        let input = "before<timing>Message timestamp: now.</timing>after";
        for split in 0..=input.len() {
            let mut filter = TimingBlockFilter::default();
            let mut output = filter.push(&input[..split]);
            output.push_str(&filter.push(&input[split..]));
            assert_eq!(output, "beforeafter", "split at {split}");
        }
    }

    #[test]
    fn streaming_filter_handles_multiple_blocks() {
        let mut filter = TimingBlockFilter::default();
        let mut output = filter.push("a<timing>one</tim");
        output.push_str(&filter.push("ing>b<timing>two</timing>c"));
        assert_eq!(output, "abc");
    }

    #[test]
    fn finish_preserves_partial_opening_marker_but_drops_unterminated_block() {
        let mut filter = TimingBlockFilter::default();
        assert_eq!(filter.push("answer<tim"), "answer");
        assert_eq!(filter.finish(), "<tim");

        assert_eq!(filter.push("answer<timing>metadata"), "answer");
        assert_eq!(filter.finish(), "");
    }
}
