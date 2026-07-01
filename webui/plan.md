# Provider/Model Selector Refactor

## Problem
- `listProviders()` in bridge.mjs uses fragile regex to parse providers.yaml — only extracts `key`, `label`, `model`
- No API to edit provider fields (base_url, api_key, endpoint, handler) from web UI
- No way to add/remove providers from web UI

## Plan

### 1. bridge.mjs — proper provider CRUD endpoints
- Replace regex `listProviders()` with a proper YAML parser that reads the full provider structure (key, label, base_url, model, api_key, endpoint, handler)
- Add `GET /api/providers` → full provider list with all fields
- Add `POST /api/providers` → create a new provider entry
- Add `PATCH /api/providers/:key` → update a specific provider's fields
- Add `DELETE /api/providers/:key` → remove a provider
- Follow the existing `getConfig`/`handleConfigWrite` pattern (read/write YAML files directly)

### 2. app.js — replace popover with full editor
- Replace the simple `provider-row` list with a structured editor view
- Show all 6 fields per provider: label, base_url, model, api_key, endpoint, handler
- Inline edit: clicking a provider row opens a form to edit its fields
- "Add provider" button to create new entries
- "Delete" button per provider (with confirmation)
- "Save" persists via PATCH endpoint, "Cancel" discards

### 3. index.html — update model-pop structure
- Replace the flat `provider-list` div with a structured editor layout
- Add form fields for each provider property

### 4. styles.css — add editor styles
- Styles for the provider editor form (inputs, labels, buttons)
- Styles for add/remove actions
- Responsive layout within the popover

## Files changed
- `webui/bridge.mjs` — provider CRUD endpoints
- `webui/public/app.js` — editor UI logic
- `webui/public/index.html` — editor HTML structure
- `webui/public/styles.css` — editor styles
