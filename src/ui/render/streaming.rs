use std::io;

use crate::ui::input::InputState;
use super::messages;
use super::{BoneTerminal, Renderer, StatusInfo};

/// During streaming: flush only complete lines of the assistant message.
///
/// The viewport is fixed-size and never resized — just flush new lines
/// above it and redraw the bottom pane.
pub fn redraw(
    renderer: &mut Renderer,
    content: &str,
    term: &mut BoneTerminal,
    input: &InputState,
    status_info: &StatusInfo,
) -> io::Result<()> {
    // Flush new complete lines into scrollback.
    let all_lines: Vec<&str> = content.lines().collect();

    let complete = if content.ends_with('\n') {
        all_lines.len()
    } else {
        all_lines.len().saturating_sub(1)
    };

    if complete > renderer.streaming_lines_flushed {
        let new_lines = &all_lines[renderer.streaming_lines_flushed..complete];
        let visual_lines = messages::assistant_raw_lines_to_lines(new_lines, term.size()?.width);
        messages::insert_lines(term, &visual_lines)?;
        renderer.streaming_lines_flushed = complete;
    }

    // Redraw bottom pane (shows current input so user can type ahead).
    term.draw(|frame| renderer.draw_bottom_pane(frame, input, status_info, None))?;
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
        let visual_lines = messages::assistant_raw_lines_to_lines(remaining, term.size()?.width);
        messages::insert_lines(term, &visual_lines)?;
        renderer.streaming_lines_flushed = all_lines.len();
    }

    messages::insert_lines(term, &[ratatui::text::Line::raw("")])?;
    Ok(())
}
