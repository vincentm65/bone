# Vision capability for bone

## Context

bone has no way to send images to the model. Every message's `content` is a plain
`String` from the data model (`ChatMessage`) through the provider serializers and
into the session DB. Adding vision means introducing a multimodal content
representation and threading it through three layers: the message model, the
OpenAI-compatible wire format, and persistence.

Scope decided with the user:
- **Entry points:** (1) clipboard image paste, (2) tool-returned images.
  *Not* a `/image <path>` command.
- **Providers:** OpenAI-compatible only (the `openai` handler already powers
  OpenAI, Gemini, OpenRouter, GLM, Kimi, etc.). Codex/native-Anthropic untouched.
- **Persistence:** images survive session resume (DB schema change).

Key wire constraint discovered: the OpenAI chat-completions format only accepts
image parts in **user** messages — `tool`-role messages cannot carry images. So a
tool that returns an image must be relayed to the model as a follow-up synthetic
**user** message containing the image, not inside the tool result itself.

## Data model

**`src/llm/provider.rs`**
- Add `ImageData { media_type: String, data: String /* base64, no data: prefix */ }`
  (`Serialize`/`Deserialize`, `Clone`).
- Add `#[serde(default, skip_serializing_if = "Vec::is_empty")] pub images: Vec<ImageData>`
  to `ChatMessage`. Update the existing constructors (lines 61-90) to default
  `images: Vec::new()`, and add a `user_with_images(text, images)` helper.

**`src/tools/types.rs`**
- Add `pub images: Vec<ImageData>` to `ToolOutput` (line 63) and `ToolResult`
  (line 21). `ToolOutput::text` defaults it empty. Add `ToolOutput::with_images`.

## Provider serialization — `src/llm/providers/openai_compat/mod.rs`

- Replace `OpenAiMessage.content: Option<String>` (line 87) with an untagged enum:
  ```rust
  #[derive(Serialize)]
  #[serde(untagged)]
  enum OaiContent { Text(String), Parts(Vec<OaiPart>) }

  #[derive(Serialize)]
  #[serde(tag = "type")]
  enum OaiPart {
      #[serde(rename = "text")] Text { text: String },
      #[serde(rename = "image_url")] ImageUrl { image_url: OaiImageUrl },
  }
  #[derive(Serialize)] struct OaiImageUrl { url: String }
  ```
- In `openai_messages` (lines 148-187): if `message.images` is non-empty, emit
  `OaiContent::Parts` — a leading `Text` part when content is non-empty, then one
  `ImageUrl { url: format!("data:{};base64,{}", media_type, data) }` per image.
  Otherwise keep current string/None behavior (`Some(OaiContent::Text(..))`).

## Tool-returned images

**`src/tools/read_file.rs`** — detect image extensions (`png jpg jpeg gif webp`);
for those, read bytes, base64-encode, return `ToolOutput::with_images` with a short
text note (e.g. `"[read image <path> (image/png)]"`) plus the `ImageData`. Text
files unchanged.

**`src/tools/registry.rs`** — carry `ToolOutput.images` into `ToolResult.images`
where `ToolOutput` is converted (same place `content`/`pane_page` are mapped).

**`src/runtime/driver.rs`** (tool-result loop, ~lines 583-605) — when
`result.images` is non-empty: push the normal text-only `ChatMessage::tool(..)` as
today, then push an additional synthetic `ChatMessage::user_with_images(note,
images)` so OpenAI receives the image legally (tool role can't hold images).
Persist *both* via `append_message` (the user image-message persists like any user
message, satisfying resume).

**`src/ext/lua_tool.rs`** (optional, same field) — let a Lua tool result table
include `images = { { media_type, data }, ... }` mapped onto `ToolOutput.images`,
so catalogue tools can return images through the same path.

## Clipboard image paste (UI)

**`Cargo.toml`** — add (gated behind/with the `ui` feature where appropriate):
- `arboard` — read image from OS clipboard (`get_image()` → RGBA `width/height/bytes`).
- `png` — encode the RGBA buffer to PNG (arboard gives raw pixels, not PNG).
- `base64` — encode for both clipboard and `read_file`.

**`src/ui/input.rs`** — mirror the existing `PasteBlob` mechanism (lines 13,
56-123): add `pub images: Vec<PendingImage>` to `InputState`, an `insert_image()`
that pushes the `ImageData` and inserts a `[image #N]` placeholder token into
`buffer` (reuse `paste_counter` numbering), include `images` in the `clear()`
resets (lines 272, 286), and a `take_images()` / `has_images()` accessor. Mirror
the placeholder-aware backspace handling so deleting `[image #N]` drops the
attachment.

**`src/ui/app/keymap.rs`** — add a `"paste_image"` action in
`handle_keymap_action` (line 40): call `arboard::Clipboard::get_image()`; on
success PNG-encode + base64 and `self.input.insert_image(..)` then redraw; on
empty/err, no-op (optionally fall back to text paste). Add a default binding (e.g.
`<C-v>`) in the shipped `defaults` keybindings.

**`src/ui/app/stream/mod.rs`** (`send_message`/`submit_user_turn`, lines 221-334)
— drain `self.input.take_images()` and attach to the transcript
`ChatMessage` (use `user_with_images`) and to the visible `Message`; pass image
JSON to `append_message`.

## Persistence — `src/session_db.rs`

- `FULL_SCHEMA` (line 228): add `images TEXT` column to `messages`.
- Migration chain (`setup_schema`, ~lines 411-438): add the next `user_version`
  step `ALTER TABLE messages ADD COLUMN images TEXT;` and bump the version
  constant (current head is 3 / `tool_calls`).
- `append_message` (lines 461-480): add an `images: Option<&str>` (JSON array of
  `{media_type,data}`) parameter; update all call sites (driver, stream).
- History load: deserialize `images` back into `ChatMessage.images`.

## Display

TUI can't render raster images, so show a placeholder. **`src/chat.rs`** `Message`:
track an image count (or reuse the `[image #N]` text). **`src/ui/render/messages.rs`**:
render a `🖼 image (PNG)` style line for attached images so the user sees them in
scrollback. Keep minimal.

## Verification

- **Unit:** add a test in `openai_compat` asserting `openai_messages` emits the
  `content: [{type:text},{type:image_url, image_url:{url:"data:image/png;base64,.."}}]`
  shape when `images` is set, and a plain string when not. Add a `read_file` test
  that a `.png` returns a `ToolOutput` with one `ImageData` and no panic.
- **DB:** test that `append_message` round-trips `images` and that the migration
  applies to a pre-existing v3 database.
- **Manual (`/run`):** launch app, copy a screenshot to clipboard, press the
  paste-image key → see `[image #N]` placeholder, send to a vision model
  (e.g. Gemini/OpenAI), confirm the model describes the image. Then have the model
  `read_file` an image and confirm it can describe it. Quit, reopen the session,
  confirm the image is still in history (persistence).
- `cargo test` and `cargo check --lib --no-default-features` (core compiles
  without `ui`; keep `arboard`/`png` off the no-ui path).

## Notes / out of scope
- No per-model vision-capability gating; images are sent and a non-vision model
  will surface its own error.
- Codex and the stubbed native-Anthropic handler are not modified.
