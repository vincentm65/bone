use std::io;

use super::messages;
use super::{BoneTerminal, InputState, Renderer, StatusInfo};

/// During streaming: flush only complete lines of the assistant message.
pub fn redraw(
    renderer: &mut Renderer,
    content: &str,
    term: &mut BoneTerminal,
    input: &InputState,
    status_info: &StatusInfo,
) -> io::Result<()> {
    let all_lines: Vec<&str> = content.lines().collect();

    let complete = if content.ends_with('\n') {
        all_lines.len()
    } else {
        all_lines.len().saturating_sub(1)
    };

    if complete > renderer.streaming_lines_flushed {
        let new_lines = &all_lines[renderer.streaming_lines_flushed..complete];
        messages::insert_raw_lines(term, new_lines)?;
        renderer.streaming_lines_flushed = complete;
    }

    term.draw(|frame| renderer.draw_bottom_pane(frame, input, status_info))?;
    Ok(())
}

/// Flush all remaining lines from the streaming message (including the
/// final partial line that `redraw` skips).
pub fn finalize(
    renderer: &mut Renderer,
    content: &str,
    term: &mut BoneTerminal,
) -> io::Result<()> {
    let all_lines: Vec<&str> = content.lines().collect();

    if all_lines.len() > renderer.streaming_lines_flushed {
        let remaining = &all_lines[renderer.streaming_lines_flushed..];
        messages::insert_raw_lines(term, remaining)?;
        renderer.streaming_lines_flushed = all_lines.len();
    }

    messages::insert_raw_lines(term, &[""])?;
    Ok(())
}
