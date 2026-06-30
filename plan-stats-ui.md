# Stats Page UI Improvement Plan

## Problems identified

1. **Monochromatic** — everything uses Indexed grays (16→252). Cards, charts, tables
   all blend together. Zero visual distinction between metric types.
2. **Tab bar ugly** — `[1 Today]  [2 7 days]...` brackets look like debug output.
3. **Cards are bland** — same color, no hierarchy. Can't scan.
4. **Bar chart uses █ full blocks** — creates a solid wall of color, hard to read individual
   values. Looks like a corrupted screen on some terminals.
5. **Hourly chart with `█  █  ·  ·` blocks** — noisy, unreadable at a glance.
6. **Two divergent layouts** — narrow (<110 cols) shows models+hourly+daily stacked
   vertically; wide shows models+heat horizontally. Inconsistent information architecture.
7. **Daily activity grid** — complex week-of-grid layout with month labels, side stats.
   Overengineered for what it shows.
8. **Models table** — columns don't align between header and body.
9. **Cards row uses 5 different labels for 6 cards** — "Cache" is ambiguous (tokens? %?).

## Design goals

- Visual hierarchy: cards → chart → detail tables
- Color-coded metrics (blue=requests, green=prompt, purple=completion, gold=cached, white=total, cyan=cache%)
- Clean tab bar: active tab bold+underline, no brackets
- Bar chart: half-block chars (▌) with gradient, more whitespace
- Hourly chart: gradient mini-bars, show only even hours, summary line
- Single consistent layout: always 2-column (chart left, models+heat right)
- Daily activity: simplified to a compact heat strip + key stats
- Proper table alignment everywhere

## Implementation

Single file: `tui/src/ui/stats.rs`

1. **Color palette** — replace Indexed colors with RGB
2. **Tab bar** — remove brackets, use Style for active state
3. **Cards** — each gets a colored accent border left-marker, centered value
4. **Layout** — single unified 2-column layout, drop the width<110 branch
5. **Bar chart** — gradient colors, half-block or `━` chars, better spacing
6. **Hourly chart** — gradient Unicode blocks (░▒▓█), one compact line
7. **Models table** — fixed column widths, aligned headers
8. **Daily activity** — simplified compact view
9. **Footer** — cleaner key hints

## NOT changing

- Key bindings
- Data loading logic (run_loop, event handling)
- Date picker modal
- Error overlay
- ViewMode enum, compact_number, weekday helpers
- The underlying session_db data structures
