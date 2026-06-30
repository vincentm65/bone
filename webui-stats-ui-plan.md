# Stats Page UI Redesign Plan

## Current Problems

1. **Six equal KPI cards** — no visual hierarchy. Cost, requests, cache% all same weight. Hard to scan.
2. **Chart has no legend** — users must guess which color is prompt vs completion.
3. **Models table is cramped** — 6 columns with narrow model names; no visual distinction between rows.
4. **No breathing room** — sections run together; cards, chart, table feel like a wall of data.
5. **Cost is buried** — most important metric is just another card + a number in summary line.
6. **Loading state** — just opacity fade, no skeleton or spinner.
7. **No empty-state polish** — "No data" is a plain dashed box.

## Proposed Changes

### Layout (2 rows of cards → 1 hero + 3 small + 2 small)
```
┌──────────────────────────────────────────┐
│  Cost          │  Total Tokens           │  ← 2 hero cards (larger, accent border)
├──────────┬──────────┬──────────┬──────────┤
│ Requests │ Prompt   │ Complet. │ Cached   │  ← 4 metric cards
│   143    │  1.2M    │  340k    │  89k 12% │
└──────────┴──────────┴──────────┴──────────┘
```
- Hero cards: cost + total tokens (most important)
- Small cards: requests, prompt, completion, cached (with cache% inline on cached card)
- Remove standalone "Cache" and "Total" cards

### Chart
- Add a compact legend: "■ Completion  ■ Prompt"
- Add subtle horizontal grid lines at 25/50/75%
- Better hover tooltips (already have title attributes, keep those)
- Slightly taller (180px → better visibility)

### Models Table
- Reduce to 4 columns: Provider/Model | Prompt | Completion | Cost
- Merge Cached into a tooltip or show as a small superscript on Prompt
- Actually: keep it simple — show Cached as a small muted number after Prompt
- Better row hover states
- Right-align all number columns consistently

### Visual Polish
- Add section dividers or more padding between sections
- Cards get subtle left-border accent color
- Chart gets a proper container with subtle background
- Loading: add a skeleton shimmer or at least a spinner text
- Empty states: centered icon + text
- Refresh timestamp more prominent

### What stays the same
- Mode tabs (Today, 7d, 4w, Yearly, All time)
- Summary line
- Keyboard shortcuts
- Modal overlay pattern
- Bridge endpoint (no changes needed)
- Hourly chart section (hidden for "today" mode)

## Files Changed
- `webui/public/styles.css` — stats section only
- `webui/public/app.js` — `renderStats()` and related functions
- `webui/public/index.html` — minor: add chart legend element

## Implementation order
1. CSS: card grid, hero cards, chart legend, table columns, spacing
2. HTML: chart legend placeholder
3. JS: card rendering, chart legend fill, model table columns
