# Codex and Grok Login/Usage Support Plan

## Goal

Add daemon-owned `/login`, `/logout`, and `/usage` support for Codex and Grok subscription plans, with behavior parity across the in-process TUI, `bone serve`, and web clients.

Keep provider account quota separate from Bone's existing local token statistics:

- `/usage`: provider subscription limits, remaining quota, and reset times.
- `/stats`: Bone's locally recorded token/request history.

## Current State

- `core/src/llm/providers/codex.rs` uses the Codex subscription endpoint and reads `~/.codex/auth.json`.
- Codex authentication currently reads only `tokens.access_token`; Bone does not refresh it or support Codex keyring-only credentials.
- `core/src/llm/providers/grok_build.rs` reads `~/.grok/auth.json`, refreshes expiring OAuth tokens, and uses the Grok Build subscription proxy.
- `core/src/config/mod.rs` detects cached Codex and Grok credentials for startup warnings.
- `/stats` reports usage stored by Bone, not subscription quota.
- Native slash-command dispatch currently lives partly in the TUI; authentication and account usage must instead be daemon-owned protocol operations.

## User-Facing Commands

```text
/login
/login codex
/login grok
/login codex --device
/login grok --device
/login status

/logout codex
/logout grok

/usage
/usage codex
/usage grok
/usage local
```

Rules:

- Omitted provider defaults to the active provider.
- Device authorization is the portable default for remote/headless daemon use.
- `/login status` shows only provider, account label, authentication state, and expiry. It never exposes tokens.
- `/usage local` shows concise Bone-local totals and points to `/stats` for the full dashboard.

## Architecture

### 1. Protocol

Add daemon commands for provider authentication and account usage, for example:

```rust
RuntimeCommand::ProviderLogin {
    provider_id: String,
    device_auth: bool,
}
RuntimeCommand::ProviderLogout {
    provider_id: String,
}
RuntimeCommand::ProviderAuthStatus {
    provider_id: String,
}
RuntimeCommand::ProviderUsage {
    provider_id: String,
}
```

Add events that contain presentation-safe data only:

```rust
RuntimeEvent::AuthPrompt {
    provider_id: String,
    verification_url: String,
    user_code: Option<String>,
}
RuntimeEvent::AuthChanged {
    provider_id: String,
    authenticated: bool,
    account_label: Option<String>,
}
RuntimeEvent::ProviderUsage {
    usage: ProviderUsageSnapshot,
}
```

Never put an access token, refresh token, authorization header, or raw credential document on the protocol.

### 2. Core authentication service

Move provider credential handling behind a core authentication module rather than leaving it inside provider transports:

```text
core/src/auth/
  mod.rs
  codex.rs
  grok.rs
```

Suggested interface:

```rust
#[async_trait]
pub trait SubscriptionAuth {
    async fn status(&self) -> Result<AuthStatus, AuthError>;
    async fn login(
        &self,
        mode: LoginMode,
        events: AuthEventSink,
    ) -> Result<AuthStatus, AuthError>;
    async fn access_token(&self) -> Result<SecretString, AuthError>;
    async fn logout(&self) -> Result<(), AuthError>;
}
```

Keep this interface minimal. Do not add a generic provider framework beyond what Codex and Grok require.

### 3. Official login brokers

Initially delegate OAuth to the official provider CLIs:

```bash
codex login --device-auth
grok login --device-auth
```

The daemon should:

1. Resolve and validate the executable.
2. Spawn it without invoking a shell.
3. Stream verification URLs, user codes, and safe status text as protocol events.
4. Support cancellation and terminate the child cleanly.
5. Recheck authentication after the process exits.
6. Invalidate cached credentials or rebuild the active provider.

Use the corresponding official logout command for `/logout`; do not partially edit provider credential JSON.

Browser PKCE login can be added later. Device authorization should work first because a browser callback on the daemon host is unsuitable for SSH and `bone serve` deployments.

## Codex Details

### Authentication

The existing direct `~/.codex/auth.json` reader is incomplete:

- It does not refresh expired credentials.
- It cannot read credentials stored only in the OS keyring.
- It assumes one private JSON shape.

Short term:

- Let the official Codex CLI own login and refresh.
- Preserve rereading file credentials before requests so refreshed tokens are observed.
- Detect keyring-only configuration and return a precise unsupported-mode message rather than an API-key warning.
- Do not copy an OAuth client ID and implement refresh requests without confirming the supported provider contract.

Long term, evaluate using `codex app-server` as the Codex auth/account broker so Bone does not depend on private credential storage.

### Subscription usage

Use the official Codex app-server account API:

```text
account/read
account/rateLimits/read
```

Normalize its response into provider-neutral structures:

```rust
pub struct ProviderUsageSnapshot {
    pub provider_id: String,
    pub plan: Option<String>,
    pub windows: Vec<UsageWindow>,
    pub credits: Option<CreditBalance>,
    pub fetched_at: SystemTime,
    pub source: UsageSource,
}

pub struct UsageWindow {
    pub label: String,
    pub used_percent: Option<f64>,
    pub remaining_percent: Option<f64>,
    pub resets_at: Option<SystemTime>,
    pub duration_minutes: Option<u64>,
}
```

Do not use the OpenAI API usage dashboard/API for this command; API-key billing and ChatGPT/Codex subscription quota are separate surfaces.

## Grok Details

### Authentication

Reuse the existing credential parsing and refresh behavior in `grok_build.rs`, moved behind the shared auth boundary. After `/login` or `/logout`, clear the provider's in-memory `credentials` cache so the next request cannot reuse stale credentials.

Keep strict validation for OAuth discovery and token endpoints. Trusted endpoints must use HTTPS and belong to an explicitly allowed xAI host.

### Subscription usage

Do not use xAI's Management API usage endpoint as a consumer subscription meter. It represents metered API/team billing rather than the Grok subscription pool.

Implementation order:

1. Inspect successful Grok Build proxy responses for documented quota/rate-limit headers.
2. If present, capture and normalize those values.
3. Store the latest safe quota snapshot in daemon state.
4. If no stable consumer quota surface is available, return an explicit fallback directing the user to Grok Settings -> Usage.

Never infer subscription percentage from Bone's token totals. Grok may meter the shared subscription pool using provider-specific compute units.

Example fallback:

```text
Grok Build · authenticated

Bone usage this session:
  184,220 input · 8,431 output · 122,004 cached tokens

Grok subscription quota is not exposed by the current Grok Build API.
Open Grok Settings -> Usage to view the shared subscription pool.
```

## Command and Client Integration

### Core command metadata

Add protected built-ins in `core/src/commands.rs`:

- `login`
- `logout`
- `usage`

The daemon remains authoritative for all effects. Do not implement credential mutation directly in `tui/src/ui/app/mod.rs` or the web bridge.

### TUI

- Parse command arguments and send protocol commands.
- Render login URL/code, progress, cancellation, completion, and errors.
- Render normalized usage windows compactly.
- Preserve immediate feedback while the external login process runs.

### Web UI

- Consume the same protocol events as the TUI.
- Present a copyable device code and safe clickable verification URL.
- Do not add bridge endpoints that read credential files directly.

## Security Requirements

- Never log or transmit access tokens, refresh tokens, authorization headers, or raw auth documents.
- Redact provider error bodies before displaying them if they may contain credentials.
- Spawn official CLIs directly, not through shell command strings.
- Ensure any Bone-owned secret file is created with mode `0600`.
- Prefer OS keyring storage if Bone eventually owns OAuth credentials.
- Treat `~/.codex/auth.json` and `~/.grok/auth.json` as passwords.
- Validate device-login URLs before making them clickable.
- Make cancellation terminate child processes and clear pending auth state.
- Do not let Lua commands or `ctx` inspect credential material.

## Implementation Phases

### Phase 1: Protocol and command skeleton

- Add protocol command/event types.
- Add normalized auth and usage data types.
- Add protected slash-command metadata.
- Route commands to daemon handlers.
- Add serialization and command-routing tests.

### Phase 2: Login/logout

- Implement cancellable official-CLI subprocess runner.
- Implement Codex and Grok adapters.
- Add authentication status checks.
- Invalidate provider credential caches after changes.
- Add missing executable, nonzero exit, cancellation, and redaction tests.

### Phase 3: Codex usage

- Add a managed `codex app-server` JSON-RPC client.
- Initialize it and call `account/read` and `account/rateLimits/read`.
- Normalize windows, reset times, plan, and credits.
- Handle unauthenticated, missing-field, and version-skew responses.

### Phase 4: Grok usage

- Verify the current official Grok quota surface.
- Capture documented response headers if available.
- Otherwise implement the explicit settings-page fallback.
- Keep local token totals clearly labeled as Bone-local data.

### Phase 5: Client parity

- Add TUI login and usage rendering.
- Add web UI login and usage rendering over the same protocol.
- Verify in-process, remote daemon, SSH/device-auth, and browser workflows.

## Validation

- Unit-test auth document parsing without real credentials.
- Unit-test normalized usage formatting and reset-time calculations.
- Test protocol serialization for every new command/event.
- Test login process cancellation and child cleanup.
- Test missing `codex`/`grok` executables.
- Test expired, malformed, absent, and refreshed credentials.
- Test that captured logs/events contain no seeded secret values.
- Test provider use immediately after login and immediately after logout.
- Test TUI and web behavior against the same daemon events.
- Run formatting, relevant crate tests, workspace compilation, and final diff review.

## Acceptance Criteria

- `/login codex` and `/login grok` can complete via device authorization without leaving Bone.
- Login works through both an in-process TUI and `bone serve`.
- Cancellation does not leave a running login child or wedged command.
- `/logout` invalidates active credentials and subsequent requests fail as unauthenticated.
- `/usage codex` reports real Codex subscription windows and reset times.
- `/usage grok` reports only verified quota data or an honest unsupported fallback.
- `/stats` remains the local historical usage dashboard.
- No credential material crosses the protocol or appears in logs.
- Existing API-key providers remain unchanged.
