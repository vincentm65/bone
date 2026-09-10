//! Provider onboarding helper for the desktop frontend.
//!
//! A small, renderer-independent state machine driving the daemon-host
//! onboarding round trip over the existing host protocol:
//!
//! * [`HostRequest::Setup`] fetches a [`SetupSnapshot`] (providers with
//!   credential flags, active provider, and the config/catalog revisions the
//!   daemon currently expects);
//! * [`HostRequest::SetupApply`] persists a plan built from that snapshot.
//!
//! All authoritative data comes from the daemon — this module never reads or
//! writes local config files. The parent app's `ConfigSnapshot` provider/model
//! picker (the `GetConfig`-based state in `main.rs`) stays unchanged: a
//! successful `SetupApply` makes the daemon broadcast a fresh `ConfigChanged`
//! snapshot, so the picker refreshes itself.
//!
//! The plan is deliberately minimal: it selects/updates a provider (optionally
//! with an API key) and never touches `init.lua` (`InitChoice::Keep`) or the
//! catalog (empty actions). The host protocol has no provider "test" API, so
//! success means the daemon persisted the plan (`SetupApplied`); this module
//! never claims a key was verified and reports `restart_required` as-is.
//!
//! Secret handling: the typed key lives in memory only, is displayed solely
//! through the masked field (or [`SetupUi::masked_key`]), never appears in any
//! status string, and is wiped on submit and on close. `SetupUi` deliberately
//! does not derive `Debug` so the key can never be printed accidentally.
//!
//! Parent (`DesktopApp`) integration:
//!
//! 1. Declare `mod setup;` in `main.rs`; keep one `setup::SetupUi` plus an
//!    in-flight slot `Option<(tab_id, request_id)>` on `DesktopApp` (same
//!    pattern as `conversations_request`).
//! 2. Each frame (e.g. in `drain_all`), while the slot is `None`, take
//!    `setup_ui.poll()`; if it returns a request, send it through the first
//!    connected tab as `RuntimeCommand::HostRequest { request_id, request }`
//!    (like `request_conversations`) and record the slot.
//! 3. In the host-response drain loop, when a response's `(tab_id,
//!    request_id)` matches the slot, clear the slot and call
//!    `setup_ui.handle_response(response)`; use the returned [`Outcome`] for
//!    app-level notices.
//! 4. Render the dialog (an `egui::Window` around `setup_ui.render(ui)`).
//!    If the returned [`RenderOutcome::request`] is a `HostRequest` (the
//!    `SetupApply` plan from Save), send it as in step 2 and record the slot.
//!    When [`RenderOutcome::close`] is set — or the window's own close button,
//!    the sending tab loses its socket, or the user cancels — hide the window,
//!    call `setup_ui.close()` (wipes the key), and clear the slot; call
//!    [`SetupUi::abort`] if a send attempt failed.
//! 5. Stale recovery is automatic: a `HostErrorCode::Stale` error arms the
//!    machine so the next `poll()` re-requests `HostRequest::Setup`, and the
//!    fresh snapshot rebases the plan (the key must be re-entered).

use std::collections::HashMap;

use bone_protocol::{
    CatalogAction, CatalogActionKind, CatalogItem, HostErrorCode, HostRequest, HostResponse,
    InitChoice, ProviderChoice, SetupApplyResult, SetupSnapshot,
};
use eframe::egui;

/// The wizard pages, mirroring the TUI's five-step onboarding flow.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Step {
    Welcome,
    Provider,
    Catalog,
    Init,
    Confirm,
}

impl Step {
    fn number(self) -> usize {
        match self {
            Step::Welcome => 1,
            Step::Provider => 2,
            Step::Catalog => 3,
            Step::Init => 4,
            Step::Confirm => 5,
        }
    }

    fn next(self) -> Step {
        match self {
            Step::Welcome => Step::Provider,
            Step::Provider => Step::Catalog,
            Step::Catalog => Step::Init,
            Step::Init => Step::Confirm,
            Step::Confirm => Step::Confirm,
        }
    }

    fn prev(self) -> Step {
        match self {
            Step::Welcome => Step::Welcome,
            Step::Provider => Step::Welcome,
            Step::Catalog => Step::Provider,
            Step::Init => Step::Catalog,
            Step::Confirm => Step::Init,
        }
    }
}

/// One `init.lua` choice offered on the Init step.
#[derive(Clone, Copy)]
struct InitOption {
    choice: InitChoice,
    label: &'static str,
    detail: &'static str,
}

/// The init options the wizard offers: always Populated/Blank, plus Keep when the
/// daemon reports an existing `init.lua` (mirrors the TUI's option list).
fn init_options(init_exists: bool) -> Vec<InitOption> {
    let mut options = vec![
        InitOption {
            choice: InitChoice::Populated,
            label: "Auto-populated",
            detail: "Banner wiring plus a researcher in subagents.yaml, ready to dispatch.",
        },
        InitOption {
            choice: InitChoice::Blank,
            label: "Blank",
            detail: "A minimal placeholder you fill in yourself.",
        },
    ];
    if init_exists {
        options.push(InitOption {
            choice: InitChoice::Keep,
            label: "Keep current",
            detail: "Leave my existing init.lua untouched.",
        });
    }
    options
}

/// Result of feeding one correlated response into [`SetupUi::handle_response`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// `HostResponse::Setup` stored; the form is (re)armed.
    Snapshot,
    /// `SetupApplied`: the daemon persisted the plan.
    Applied(SetupApplyResult),
    /// `HostResponse::Error`. `stale` is true for `HostErrorCode::Stale`, in
    /// which case the next [`SetupUi::poll`] re-requests a fresh snapshot.
    Failed {
        code: HostErrorCode,
        message: String,
        stale: bool,
    },
    /// Not a setup response; state is unchanged.
    Ignored,
}

/// What the parent should do after one [`SetupUi::render`] frame.
#[derive(Debug, Default)]
pub struct RenderOutcome {
    /// A `SetupApply` plan produced by Save, to send and correlate.
    pub request: Option<HostRequest>,
    /// The user dismissed the dialog (Done/Cancel/Close): the parent must hide
    /// the window and clear its in-flight slot.
    pub close: bool,
}

impl RenderOutcome {
    /// A pure close request (no outgoing plan).
    fn close() -> Self {
        Self {
            request: None,
            close: true,
        }
    }
}

/// Provider onboarding state machine (see the module docs for integration).
///
/// The parent owns correlation: it sends whatever `poll`/`submit`/`render`
/// hand back and routes the matching `RuntimeEvent::HostResponse` back into
/// [`Self::handle_response`].
pub struct SetupUi {
    /// Stored daemon snapshot; the plan's expected revisions come from here.
    snapshot: Option<SetupSnapshot>,
    /// Provider selected in the form (defaults to the snapshot's active one).
    provider: Option<String>,
    /// API key typed by the user. In memory only: masked in the UI, never
    /// logged, wiped on submit and on close.
    api_key: String,
    /// A `Setup`/`SetupApply` request is in flight.
    pending: bool,
    /// A `Stale` error was seen; the next `poll` re-requests the snapshot.
    stale: bool,
    /// Latest successful apply, shown in the success panel.
    applied: Option<SetupApplyResult>,
    /// Latest host error, shown in the error panel.
    error: Option<(HostErrorCode, String)>,
    /// Current wizard page.
    step: Step,
    /// Catalog entries from the snapshot; checked/touched state lives in the
    /// maps below so the entries stay immutable.
    catalog: Vec<CatalogItem>,
    /// Checked state per catalog item name (seeded from installed/update).
    cat_checked: HashMap<String, bool>,
    /// `init.lua` choices offered (Populated/Blank, plus Keep when applicable).
    init_options: Vec<InitOption>,
    /// Selected `init.lua` option.
    init_cursor: usize,
}

impl Default for SetupUi {
    fn default() -> Self {
        Self::new()
    }
}

// The GUI drives this machine through `poll`, `submit`, `render`,
// `handle_response`, `show` and the lifecycle methods. The read-only accessors
// (`selected_provider_id`, `masked_key`, `key_len`, `set_api_key`, `providers`,
// `status_line`) are part of the module's documented contract and are exercised
// by the headless unit tests and reserved for status surfaces; in a binary crate
// `pub` does not exempt them from `dead_code`, so the warning is suppressed here.
#[allow(dead_code)]
impl SetupUi {
    pub fn new() -> Self {
        Self {
            snapshot: None,
            provider: None,
            api_key: String::new(),
            pending: false,
            stale: false,
            applied: None,
            error: None,
            step: Step::Welcome,
            catalog: Vec::new(),
            cat_checked: HashMap::new(),
            init_options: init_options(false),
            init_cursor: 0,
        }
    }

    // ---- requests ---------------------------------------------------------

    /// The next request the parent should send, or `None` when none is due.
    ///
    /// Returns `HostRequest::Setup` once until a snapshot is stored (rearmed
    /// after a stale error or `close`). While a request is in flight, or once
    /// the plan has been applied, returns `None`.
    pub fn poll(&mut self) -> Option<HostRequest> {
        if self.pending || self.applied.is_some() {
            return None;
        }
        if self.stale {
            self.stale = false;
            self.snapshot = None;
            self.provider = None;
            self.api_key.clear();
            self.pending = true;
            return Some(HostRequest::Setup);
        }
        if self.snapshot.is_none() {
            self.pending = true;
            Some(HostRequest::Setup)
        } else {
            None
        }
    }

    /// The parent failed to send the request returned by `poll`/`submit`
    /// (e.g. the tab's socket dropped): drop the in-flight mark so `poll`
    /// can reissue. Any stored snapshot is kept.
    pub fn abort(&mut self) {
        self.pending = false;
    }

    /// Store a `SetupSnapshot` (normally via [`Self::handle_response`]).
    /// Rearms the form: selects the snapshot's active provider (falling back
    /// to the first) and clears any stale error.
    pub fn show(&mut self, snapshot: SetupSnapshot) {
        let active = snapshot.active_provider.clone();
        self.provider = if snapshot.providers.iter().any(|p| p.id == active) {
            Some(active)
        } else {
            snapshot.providers.first().map(|p| p.id.clone())
        };
        self.error = None;
        self.stale = false;
        self.pending = false;
        // (Re)seed the wizard from the fresh snapshot.
        self.step = Step::Welcome;
        self.catalog = snapshot.catalog.items.clone();
        self.cat_checked.clear();
        for item in &self.catalog {
            self.cat_checked
                .insert(item.name.clone(), item.installed || item.update_available);
        }
        self.init_options = init_options(snapshot.init_exists);
        self.init_cursor = 0;
        self.snapshot = Some(snapshot);
    }

    // ---- form -------------------------------------------------------------

    /// The providers reported by the daemon (empty before a snapshot).
    pub fn providers(&self) -> &[ProviderChoice] {
        self.snapshot
            .as_ref()
            .map(|s| s.providers.as_slice())
            .unwrap_or_default()
    }

    /// The provider selected in the form, if any.
    pub fn selected_provider_id(&self) -> Option<&str> {
        self.provider.as_deref()
    }

    /// Select a provider known to the stored snapshot; unknown ids are
    /// rejected so the plan can never name a provider the daemon doesn't have.
    pub fn select_provider(&mut self, id: &str) {
        if self.providers().iter().any(|p| p.id == id) {
            self.provider = Some(id.to_owned());
        }
    }

    /// The provider currently selected, if the snapshot knows it.
    fn selected(&self) -> Option<&ProviderChoice> {
        self.providers()
            .iter()
            .find(|p| Some(p.id.as_str()) == self.provider.as_deref())
    }

    /// The typed key, masked for display: one "•" per character, never the key.
    pub fn masked_key(&self) -> String {
        "•".repeat(self.api_key.chars().count())
    }

    /// Character count of the typed key (0 = blank).
    pub fn key_len(&self) -> usize {
        self.api_key.chars().count()
    }

    /// Replace the typed key (bound to the dialog's masked field).
    pub fn set_api_key(&mut self, key: &str) {
        self.api_key = key.to_owned();
    }

    /// True when the selected provider still has no key on the daemon — the
    /// actionable onboarding state this dialog exists to fix.
    pub fn needs_key(&self) -> bool {
        self.selected().is_some_and(|p| !p.api_key_configured)
    }

    /// Client-side credential sanity check for the typed key.
    ///
    /// The host protocol has no provider "test" call and this crate never links
    /// `bone-core`, so a typed key cannot be authenticated here — the daemon
    /// persists it as-is. We do reject obvious paste errors (embedded
    /// whitespace or control characters) before sending. An empty field means
    /// "skip / keep the existing key" and is always allowed.
    pub fn key_error(&self) -> Option<&'static str> {
        let key = self.api_key.trim();
        if key.is_empty() {
            return None;
        }
        if key.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Some(
                "The API key contains spaces or control characters — check for a bad paste.",
            );
        }
        None
    }

    /// True when the plan can be applied: a provider is selected and the typed
    /// key (if any) passes the client-side sanity check.
    pub fn can_apply(&self) -> bool {
        self.selected().is_some() && self.key_error().is_none()
    }

    /// True when the selected provider has no configured key and none was typed,
    /// so applying would leave it unusable (mirrors the TUI's no-key warning).
    pub fn missing_key_warning(&self) -> bool {
        self.selected().is_some_and(|p| !p.api_key_configured) && self.api_key.trim().is_empty()
    }

    /// Auto-offer setup only when the daemon reports first-launch onboarding.
    /// An unconfigured selection is still editable through explicit setup, but
    /// must not interrupt users who already have another working provider.
    pub fn actionable(&self) -> bool {
        self.snapshot.as_ref().is_some_and(|s| s.needs_onboarding)
    }

    /// True once a `SetupApply` has been accepted by the daemon (terminal
    /// success state; the parent can show a "done" notice on close).
    pub fn completed(&self) -> bool {
        self.applied.is_some()
    }

    /// The catalog entries offered on the Catalog step.
    pub fn catalog(&self) -> &[CatalogItem] {
        &self.catalog
    }

    /// The checked state for one catalog item (seeded installed/update-checked).
    fn cat_checked(&self, item: &CatalogItem) -> bool {
        *self
            .cat_checked
            .get(&item.name)
            .unwrap_or(&(item.installed || item.update_available))
    }

    /// Toggle one catalog item's checked state.
    fn toggle_catalog(&mut self, name: &str) {
        let current = self.cat_checked.get(name).copied().unwrap_or(false);
        self.cat_checked.insert(name.to_owned(), !current);
    }

    /// The catalog actions the plan will send: only items whose checked state
    /// differs from the snapshot, plus any pending update. Mirrors the TUI's
    /// `catalog::actions(..., touched_only = false)`.
    fn catalog_actions(&self) -> Vec<CatalogAction> {
        self.catalog
            .iter()
            .filter_map(|item| {
                let checked = self.cat_checked(item);
                if checked != item.installed || (checked && item.update_available) {
                    Some(CatalogAction {
                        name: item.name.clone(),
                        action: if checked {
                            CatalogActionKind::Install
                        } else {
                            CatalogActionKind::Remove
                        },
                    })
                } else {
                    None
                }
            })
            .collect()
    }

    /// The currently selected `init.lua` choice.
    fn init_choice(&self) -> InitChoice {
        self.init_options
            .get(self.init_cursor)
            .map(|o| o.choice)
            .unwrap_or(InitChoice::Keep)
    }

    /// The label of the selected `init.lua` choice (for the confirm summary).
    fn init_label(&self) -> &'static str {
        self.init_options
            .get(self.init_cursor)
            .map(|o| o.label)
            .unwrap_or("Keep current")
    }

    // ---- plan and lifecycle ------------------------------------------------

    /// Build the `SetupApply` plan from the stored snapshot, wipe the local
    /// key copy (the wire request is the only copy that remains), and mark
    /// the request in flight.
    ///
    /// The plan uses the snapshot's config and catalog revisions, the selected
    /// `init.lua` choice, and the catalog actions collected on the Catalog
    /// step. Returns `None` — with no side effects — when no provider is
    /// selectable.
    pub fn submit(&mut self) -> Option<HostRequest> {
        let (config_revision, catalog_revision) = {
            let snapshot = self.snapshot.as_ref()?;
            (snapshot.config_revision, snapshot.catalog.revision.clone())
        };
        let provider_id = self.provider.clone()?;
        let api_key = (!self.api_key.trim().is_empty()).then(|| self.api_key.trim().to_string());
        let catalog = self.catalog_actions();
        let init = self.init_choice();
        self.api_key.clear();
        self.error = None;
        self.pending = true;
        Some(HostRequest::SetupApply {
            expected_config_revision: config_revision,
            expected_catalog_revision: catalog_revision,
            provider_id: Some(provider_id),
            api_key,
            catalog,
            init,
        })
    }

    /// Close the dialog: wipe the key and rearm for a fresh snapshot on the
    /// next `poll`. The parent calls this on cancel, on the window's close
    /// button, or when the sending tab loses its socket.
    pub fn close(&mut self) {
        *self = Self::new();
    }

    /// Feed one correlated response for an in-flight setup request.
    ///
    /// Consumes `HostResponse::Setup` / `SetupApplied` / `Error`; any other
    /// variant is ignored (the parent should only route setup responses here).
    /// The key is wiped on every response: it was already cleared on submit,
    /// so this is belt-and-braces for paths where a key was never sent.
    pub fn handle_response(&mut self, response: HostResponse) -> Outcome {
        match response {
            HostResponse::Setup(snapshot) => {
                self.show(snapshot);
                Outcome::Snapshot
            }
            HostResponse::SetupApplied(result) => {
                self.pending = false;
                self.api_key.clear();
                self.error = None;
                self.applied = Some(result.clone());
                Outcome::Applied(result)
            }
            HostResponse::Error { code, message } => {
                self.pending = false;
                self.api_key.clear();
                let stale = code == HostErrorCode::Stale;
                if stale {
                    // The daemon's revisions moved: drop the local base and
                    // re-request; the fresh snapshot rebases the plan.
                    self.stale = true;
                    self.snapshot = None;
                    self.provider = None;
                }
                self.error = Some((code, message.clone()));
                Outcome::Failed {
                    code,
                    message,
                    stale,
                }
            }
            _ => Outcome::Ignored,
        }
    }

    /// Short, non-secret line for the parent's status bar. The API key is
    /// never part of it.
    pub fn status_line(&self) -> String {
        if let Some(result) = &self.applied {
            return result.message.clone();
        }
        if let Some((code, message)) = &self.error {
            return format!("{code:?}: {message}");
        }
        if self.pending {
            return "Contacting daemon…".into();
        }
        if let Some(snapshot) = &self.snapshot {
            if self.needs_key() {
                return format!(
                    "Provider {} has no API key",
                    self.provider
                        .as_deref()
                        .unwrap_or(&snapshot.active_provider)
                );
            }
            return "All providers have keys configured".into();
        }
        "Waiting for setup snapshot…".into()
    }

    // ---- rendering ----------------------------------------------------------

    /// Render the dialog contents into `ui` and report the parent's next
    /// action: a `SetupApply` plan to send when Save is clicked, and/or a close
    /// request when the user dismisses the dialog (Done/Cancel/Close).
    pub fn render(&mut self, ui: &mut egui::Ui) -> RenderOutcome {
        if let Some(result) = &self.applied {
            if result.message.is_empty() {
                ui.label(egui::RichText::new("Setup saved.").strong());
            } else {
                ui.label(egui::RichText::new(&result.message).strong());
            }
            if result.restart_required {
                ui.colored_label(
                    egui::Color32::from_rgb(235, 190, 80),
                    "The daemon flagged this change as restart-required; \
                     restart the daemon to pick it up.",
                );
            }
            ui.add_space(4.0);
            if ui.button("Done").clicked() {
                return RenderOutcome::close();
            }
            return RenderOutcome::default();
        }
        if let Some((code, message)) = &self.error {
            ui.colored_label(
                egui::Color32::from_rgb(235, 90, 90),
                egui::RichText::new(format!("Setup failed ({code:?})")).strong(),
            );
            ui.label(message);
            if self.stale {
                ui.weak(
                    "The daemon's setup revision changed; a fresh snapshot is \
                     being requested. Re-enter the key and save again.",
                );
            } else {
                ui.weak("Your typed API key was discarded; re-enter it and save again.");
            }
            ui.add_space(4.0);
            let mut close = false;
            ui.horizontal(|ui| {
                if !self.stale && ui.button("Retry").clicked() {
                    // Keep the stored snapshot; the user re-saves the same plan.
                    self.error = None;
                }
                if ui.button("Close").clicked() {
                    close = true;
                }
            });
            if close {
                return RenderOutcome::close();
            }
            return RenderOutcome::default();
        }
        if self.snapshot.is_none() {
            ui.weak(if self.pending {
                "Loading daemon setup state…"
            } else {
                "Waiting for a daemon connection…"
            });
            ui.add_space(4.0);
            if ui.button("Close").clicked() {
                return RenderOutcome::close();
            }
            return RenderOutcome::default();
        }

        // Header: title + step counter.
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("bone setup").strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.weak(format!(
                    "step {} of {}",
                    self.step.number(),
                    Step::Confirm.number()
                ));
            });
        });
        ui.separator();
        match self.step {
            Step::Welcome => self.render_welcome(ui),
            Step::Provider => self.render_provider(ui),
            Step::Catalog => self.render_catalog(ui),
            Step::Init => self.render_init(ui),
            Step::Confirm => self.render_confirm(ui),
        }

        ui.add_space(4.0);
        ui.separator();
        let mut outcome = RenderOutcome::default();
        ui.horizontal(|ui| {
            if self.step != Step::Welcome && ui.button("← Back").clicked() {
                self.step = self.step.prev();
            }
            let forward = match self.step {
                Step::Welcome => "Start",
                Step::Confirm => "Apply",
                _ => "Next →",
            };
            let ready = self.step != Step::Confirm || self.can_apply();
            if ui.add_enabled(ready, egui::Button::new(forward)).clicked() {
                if self.step == Step::Confirm {
                    outcome.request = self.submit();
                } else {
                    self.step = self.step.next();
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Cancel").clicked() {
                    outcome.close = true;
                }
            });
        });
        outcome
    }

    // ---- wizard pages -------------------------------------------------------

    fn render_welcome(&self, ui: &mut egui::Ui) {
        ui.label(egui::RichText::new("Welcome to Bone").strong());
        ui.add_space(4.0);
        ui.label("This quick setup configures the daemon host. You'll set:");
        ui.add_space(2.0);
        ui.label("• Provider — pick one and drop in an API key (optional).");
        ui.label("• Catalog — optional tools & commands, downloaded on demand.");
        ui.label("• init.lua — startup script, banner, and advanced hooks.");
        ui.add_space(4.0);
        ui.weak(
            "Everything is editable later — just ask bone, or run /setup again. \
             Nothing is written locally; the daemon persists the plan on Apply.",
        );
    }

    fn render_provider(&mut self, ui: &mut egui::Ui) {
        // Clone so the loop can mutate `self` while reading the list.
        let providers = self.providers().to_vec();
        ui.label(egui::RichText::new("Pick a provider and add a key").strong());
        if self.snapshot.as_ref().is_some_and(|s| s.needs_onboarding) {
            ui.weak("First launch: pick a provider and add its API key to get started.");
        }
        ui.add_space(4.0);
        if providers.is_empty() {
            ui.label("No providers are configured on this daemon.");
            ui.weak("Add one on the daemon (e.g. its providers config) and reopen setup.");
        } else {
            let active = self
                .snapshot
                .as_ref()
                .map(|s| s.active_provider.clone())
                .unwrap_or_default();
            for provider in &providers {
                let is_active = provider.id == active;
                let marker = if is_active { "●" } else { "○" };
                let label = if provider.label.is_empty() {
                    provider.id.clone()
                } else {
                    provider.label.clone()
                };
                let suffix = if provider.api_key_configured {
                    String::new()
                } else {
                    " (no API key)".to_string()
                };
                let selected = self.provider.as_deref() == Some(provider.id.as_str());
                if ui
                    .selectable_label(
                        selected,
                        egui::RichText::new(format!("{marker} {label}{suffix}")),
                    )
                    .on_hover_text(if is_active {
                        "Active provider"
                    } else {
                        "Select to activate on apply"
                    })
                    .clicked()
                {
                    self.select_provider(&provider.id);
                }
            }
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label("API key");
            ui.add(
                egui::TextEdit::singleline(&mut self.api_key)
                    .password(true)
                    .desired_width(220.0)
                    .hint_text("leave blank to skip"),
            );
        });
        ui.weak(
            "Sent to the daemon on Apply. The app never logs it and forgets it \
             as soon as the dialog closes.",
        );
        if let Some(error) = self.key_error() {
            ui.colored_label(egui::Color32::from_rgb(235, 90, 90), error);
        }
    }

    fn render_catalog(&mut self, ui: &mut egui::Ui) {
        ui.label(egui::RichText::new("Pick optional tools & commands").strong());
        if self.catalog.is_empty() {
            ui.add_space(4.0);
            ui.weak(
                "bone couldn't reach the catalog (you may be offline). Skip for \
                 now and add tools later with /catalog.",
            );
            return;
        }
        ui.weak("They download once selected. Uncheck to remove an installed item.");
        ui.add_space(4.0);
        let items = self.catalog.clone();
        egui::ScrollArea::vertical()
            .id_salt("setup_catalog")
            .max_height(220.0)
            .show(ui, |ui| {
                for item in &items {
                    let mut checked = self.cat_checked(item);
                    let mut label = item.name.clone();
                    if item.update_available {
                        label.push_str("  (update available)");
                    } else if item.installed {
                        label.push_str("  (installed)");
                    }
                    if ui.checkbox(&mut checked, label).changed() {
                        self.toggle_catalog(&item.name);
                    }
                }
            });
    }

    fn render_init(&mut self, ui: &mut egui::Ui) {
        ui.label(egui::RichText::new("How should your init.lua start?").strong());
        ui.weak("init.lua runs once at launch.");
        ui.add_space(4.0);
        let options = self.init_options.clone();
        for (i, option) in options.iter().enumerate() {
            let selected = i == self.init_cursor;
            if ui.selectable_label(selected, option.label).clicked() {
                self.init_cursor = i;
            }
        }
        if let Some(option) = options.get(self.init_cursor) {
            ui.add_space(4.0);
            ui.weak(option.detail);
        }
    }

    fn render_confirm(&self, ui: &mut egui::Ui) {
        ui.label(egui::RichText::new("Ready to set up bone.").strong());
        ui.add_space(4.0);
        let provider = self
            .selected()
            .map(|p| {
                if p.label.is_empty() {
                    p.id.clone()
                } else {
                    p.label.clone()
                }
            })
            .unwrap_or_else(|| "skipped".to_string());
        let key = if self.api_key.trim().is_empty() {
            if self.selected().is_some_and(|p| p.api_key_configured) {
                "keep current".to_string()
            } else {
                "skipped".to_string()
            }
        } else {
            format!("{} characters", self.key_len())
        };
        ui.label(format!("Provider:   {provider}"));
        ui.label(format!("API key:    {key}"));
        ui.label(format!(
            "Catalog:    {} change(s)",
            self.catalog_actions().len()
        ));
        ui.label(format!("init.lua:   {}", self.init_label()));
        ui.add_space(4.0);
        if let Some(error) = self.key_error() {
            ui.colored_label(egui::Color32::from_rgb(235, 90, 90), error);
        } else if self.missing_key_warning() {
            ui.colored_label(
                egui::Color32::from_rgb(235, 190, 80),
                "No API key for this provider yet — it will stay unusable until you \
                 add one (via /config or /setup).",
            );
        }
        ui.weak("Apply sends this plan to the daemon host; Cancel leaves config unchanged.");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bone_protocol::{CatalogApplyResult, CatalogSnapshot};

    fn provider(id: &str, key: bool) -> ProviderChoice {
        ProviderChoice {
            id: id.to_string(),
            label: id.to_string(),
            api_key_configured: key,
        }
    }

    fn snapshot(
        revision: u64,
        catalog: &str,
        providers: Vec<ProviderChoice>,
        active: &str,
    ) -> SetupSnapshot {
        SetupSnapshot {
            config_revision: revision,
            providers,
            active_provider: active.to_string(),
            init_exists: false,
            needs_onboarding: true,
            catalog: CatalogSnapshot {
                revision: catalog.to_string(),
                items: Vec::new(),
            },
        }
    }

    fn applied(revision: u64, message: &str) -> SetupApplyResult {
        SetupApplyResult {
            config_revision: revision,
            catalog: CatalogApplyResult {
                snapshot: CatalogSnapshot {
                    revision: "c2".into(),
                    items: Vec::new(),
                },
                results: Vec::new(),
                changed: false,
                extensions_reloaded: false,
            },
            restart_required: true,
            message: message.to_string(),
        }
    }

    fn cat_item(name: &str, installed: bool, update: bool) -> CatalogItem {
        CatalogItem {
            name: name.to_string(),
            installed,
            update_available: update,
            ..CatalogItem::default()
        }
    }

    fn snapshot_with_catalog(
        revision: u64,
        items: Vec<CatalogItem>,
        init_exists: bool,
    ) -> SetupSnapshot {
        SetupSnapshot {
            config_revision: revision,
            providers: vec![provider("local", true)],
            active_provider: "local".to_string(),
            init_exists,
            needs_onboarding: false,
            catalog: CatalogSnapshot {
                revision: "cat-1".to_string(),
                items,
            },
        }
    }

    /// Drive one headless egui frame over `render` (no window, no input).
    fn render_once(state: &mut SetupUi) -> RenderOutcome {
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::Vec2::splat(420.0),
            )),
            ..Default::default()
        };
        let mut outcome = RenderOutcome::default();
        let output = ctx.run_ui(input, |ui| {
            outcome = state.render(ui);
        });
        output.drop_without_applying_deltas();
        outcome
    }

    #[test]
    fn poll_requests_setup_once_until_a_snapshot_arrives() {
        let mut ui = SetupUi::new();
        assert!(matches!(ui.poll(), Some(HostRequest::Setup)));
        // In flight: no second request.
        assert!(ui.poll().is_none());
        // A send failure re-arms via abort.
        ui.abort();
        assert!(matches!(ui.poll(), Some(HostRequest::Setup)));
        ui.show(snapshot(4, "c1", vec![provider("local", false)], "local"));
        assert!(ui.poll().is_none(), "snapshot stored: nothing due");
    }

    #[test]
    fn plan_uses_snapshot_revisions_and_defaults() {
        let mut ui = SetupUi::new();
        ui.show(snapshot(
            7,
            "cat-1",
            vec![provider("local", false), provider("remote", true)],
            "local",
        ));
        assert_eq!(ui.selected_provider_id(), Some("local"));
        ui.set_api_key("sk-test-123");
        let Some(HostRequest::SetupApply {
            expected_config_revision,
            expected_catalog_revision,
            provider_id,
            api_key,
            catalog,
            init,
        }) = ui.submit()
        else {
            panic!("submit must produce a SetupApply plan");
        };
        assert_eq!(expected_config_revision, 7);
        assert_eq!(expected_catalog_revision, "cat-1");
        assert_eq!(provider_id.as_deref(), Some("local"));
        assert_eq!(api_key.as_deref(), Some("sk-test-123"));
        assert!(catalog.is_empty(), "no catalog items: no actions");
        assert_eq!(
            init,
            InitChoice::Populated,
            "default init choice is auto-populated"
        );
    }

    #[test]
    fn catalog_is_seeded_and_actions_reflect_checked_state() {
        let mut ui = SetupUi::new();
        ui.show(snapshot_with_catalog(
            1,
            vec![
                cat_item("alpha", true, false), // installed -> checked
                cat_item("beta", false, false), // not installed -> unchecked
                cat_item("gamma", true, true),  // installed + update -> checked
            ],
            false,
        ));
        assert_eq!(ui.catalog().len(), 3);
        assert!(ui.cat_checked(&ui.catalog()[0]));
        assert!(!ui.cat_checked(&ui.catalog()[1]));
        assert!(ui.cat_checked(&ui.catalog()[2]));
        // No user changes: only the pending update is actionable.
        let actions = ui.catalog_actions();
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].name, "gamma");
        assert_eq!(actions[0].action, CatalogActionKind::Install);

        // Toggle beta on (install) and alpha off (remove).
        ui.toggle_catalog("beta");
        ui.toggle_catalog("alpha");
        let actions = ui.catalog_actions();
        assert_eq!(actions.len(), 3);
        assert_eq!(actions[0].name, "alpha");
        assert_eq!(actions[0].action, CatalogActionKind::Remove);
        assert_eq!(actions[1].name, "beta");
        assert_eq!(actions[1].action, CatalogActionKind::Install);
        assert_eq!(actions[2].name, "gamma");
        assert_eq!(actions[2].action, CatalogActionKind::Install);
    }

    #[test]
    fn init_options_depend_on_existing_init_lua() {
        let mut ui = SetupUi::new();
        ui.show(snapshot_with_catalog(1, Vec::new(), false));
        assert_eq!(ui.init_choice(), InitChoice::Populated);
        assert_eq!(ui.init_label(), "Auto-populated");
        assert!(!ui.init_options.iter().any(|o| o.choice == InitChoice::Keep));

        let mut ui = SetupUi::new();
        ui.show(snapshot_with_catalog(1, Vec::new(), true));
        assert!(ui.init_options.iter().any(|o| o.choice == InitChoice::Keep));
        // Select Keep (last option) and confirm it flows into the plan.
        ui.init_cursor = ui.init_options.len() - 1;
        assert_eq!(ui.init_choice(), InitChoice::Keep);
        let HostRequest::SetupApply { init, .. } = ui.submit().expect("plan") else {
            panic!("expected SetupApply");
        };
        assert_eq!(init, InitChoice::Keep);
    }

    #[test]
    fn submit_includes_catalog_and_selected_init_choice() {
        let mut ui = SetupUi::new();
        ui.show(snapshot_with_catalog(
            3,
            vec![cat_item("alpha", false, false)],
            true,
        ));
        ui.toggle_catalog("alpha"); // install alpha
        ui.init_cursor = 0; // Auto-populated
        let HostRequest::SetupApply { catalog, init, .. } = ui.submit().expect("plan") else {
            panic!("expected SetupApply");
        };
        assert_eq!(catalog.len(), 1);
        assert_eq!(catalog[0].name, "alpha");
        assert_eq!(catalog[0].action, CatalogActionKind::Install);
        assert_eq!(init, InitChoice::Populated);
    }

    #[test]
    fn step_navigation_wraps_at_the_ends() {
        assert_eq!(Step::Welcome.number(), 1);
        assert_eq!(Step::Confirm.number(), 5);
        assert_eq!(Step::Welcome.prev(), Step::Welcome);
        assert_eq!(Step::Confirm.next(), Step::Confirm);
        assert_eq!(Step::Welcome.next(), Step::Provider);
        assert_eq!(Step::Confirm.prev(), Step::Init);
    }

    #[test]
    fn submit_wipes_key_and_trims_whitespace() {
        let mut ui = SetupUi::new();
        ui.show(snapshot(1, "c", vec![provider("local", false)], "local"));
        ui.set_api_key("  sk-padded  ");
        let plan = ui.submit().expect("plan");
        let HostRequest::SetupApply { api_key, .. } = plan else {
            panic!("expected SetupApply");
        };
        assert_eq!(api_key.as_deref(), Some("sk-padded"));
        assert_eq!(ui.key_len(), 0, "local key copy wiped on submit");
        assert_eq!(ui.masked_key(), "");

        // A blank/whitespace key is omitted: the daemon keeps the existing one.
        let mut ui = SetupUi::new();
        ui.show(snapshot(1, "c", vec![provider("local", true)], "local"));
        ui.set_api_key("   ");
        let HostRequest::SetupApply { api_key, .. } = ui.submit().expect("plan") else {
            panic!("expected SetupApply");
        };
        assert_eq!(api_key, None);
    }

    #[test]
    fn close_wipes_key_and_rearms_a_fresh_snapshot() {
        let mut ui = SetupUi::new();
        ui.show(snapshot(2, "c", vec![provider("local", false)], "local"));
        ui.set_api_key("secret");
        ui.close();
        assert_eq!(ui.key_len(), 0, "key wiped on close");
        assert!(matches!(ui.poll(), Some(HostRequest::Setup)));
    }

    #[test]
    fn stale_error_rebases_the_plan_via_repoll() {
        let mut ui = SetupUi::new();
        ui.show(snapshot(
            5,
            "c-old",
            vec![provider("local", false)],
            "local",
        ));
        ui.set_api_key("sk-a");
        let plan = ui.submit().expect("plan");
        let HostRequest::SetupApply {
            expected_config_revision,
            ..
        } = plan
        else {
            panic!("expected SetupApply");
        };
        assert_eq!(expected_config_revision, 5);

        let outcome = ui.handle_response(HostResponse::Error {
            code: HostErrorCode::Stale,
            message: "config changed; expected 5, current 6".into(),
        });
        assert!(
            matches!(outcome, Outcome::Failed { stale: true, .. }),
            "stale must be flagged"
        );
        assert_eq!(ui.key_len(), 0, "key wiped with the stale plan");
        assert!(matches!(ui.poll(), Some(HostRequest::Setup)));
        ui.show(snapshot(
            9,
            "c-new",
            vec![provider("local", false)],
            "local",
        ));
        ui.set_api_key("sk-b");
        let HostRequest::SetupApply {
            expected_config_revision,
            expected_catalog_revision,
            ..
        } = ui.submit().expect("plan")
        else {
            panic!("expected rebased SetupApply");
        };
        assert_eq!(expected_config_revision, 9, "rebased on the fresh revision");
        assert_eq!(expected_catalog_revision, "c-new");
    }

    #[test]
    fn non_stale_error_keeps_the_snapshot_for_retry() {
        let mut ui = SetupUi::new();
        ui.show(snapshot(3, "c", vec![provider("local", false)], "local"));
        ui.set_api_key("sk-c");
        let _ = ui.submit();
        let outcome = ui.handle_response(HostResponse::Error {
            code: HostErrorCode::Busy,
            message: "daemon is busy".into(),
        });
        assert!(matches!(outcome, Outcome::Failed { stale: false, .. }));
        assert!(
            ui.providers().iter().any(|p| p.id == "local"),
            "snapshot retained for a retry"
        );
        assert_eq!(ui.status_line(), "Busy: daemon is busy");
    }

    #[test]
    fn applied_response_stops_polling_and_reports_the_message() {
        let mut ui = SetupUi::new();
        ui.show(snapshot(4, "c", vec![provider("local", false)], "local"));
        let _ = ui.submit();
        let outcome = ui.handle_response(HostResponse::SetupApplied(applied(6, "Setup saved.")));
        assert_eq!(outcome, Outcome::Applied(applied(6, "Setup saved.")));
        assert!(ui.poll().is_none(), "no re-request after success");
        assert_eq!(ui.status_line(), "Setup saved.");
    }

    #[test]
    fn key_is_never_exposed_outside_the_masked_field() {
        let mut ui = SetupUi::new();
        ui.show(snapshot(1, "c", vec![provider("local", false)], "local"));
        ui.set_api_key("hunter2-secret-key");
        assert_eq!(ui.masked_key(), "•".repeat(18));
        assert!(!ui.masked_key().contains("hunter2"));
        assert!(!ui.status_line().contains("hunter2"));
        assert!(!ui.status_line().contains("sk"));
    }

    #[test]
    fn key_error_flags_bad_pastes_but_allows_blank_and_clean_keys() {
        let mut ui = SetupUi::new();
        ui.show(snapshot(1, "c", vec![provider("local", false)], "local"));
        assert_eq!(ui.key_error(), None, "blank key is a valid skip");
        ui.set_api_key("sk-abc 123");
        assert!(ui.key_error().is_some(), "embedded space is a paste error");
        ui.set_api_key("sk-abc\n123");
        assert!(
            ui.key_error().is_some(),
            "embedded newline is a paste error"
        );
        ui.set_api_key("  sk-abc-123  ");
        assert_eq!(ui.key_error(), None, "surrounding whitespace is trimmed");
    }

    #[test]
    fn can_apply_requires_a_provider_and_a_sane_key() {
        let mut ui = SetupUi::new();
        ui.show(snapshot(1, "c", vec![provider("local", false)], "local"));
        assert!(ui.can_apply(), "blank key with a provider is applicable");
        ui.set_api_key("sk a");
        assert!(!ui.can_apply(), "malformed key blocks apply");
        // No providers: nothing to apply.
        let mut ui = SetupUi::new();
        ui.show(snapshot(1, "c", Vec::new(), ""));
        assert!(!ui.can_apply(), "no provider selected");
    }

    #[test]
    fn missing_key_warning_tracks_provider_and_field() {
        let mut ui = SetupUi::new();
        ui.show(snapshot(1, "c", vec![provider("local", false)], "local"));
        assert!(ui.missing_key_warning());
        ui.set_api_key("sk-123");
        assert!(!ui.missing_key_warning());
        // Provider already has a key: no warning even with an empty field.
        let mut ui = SetupUi::new();
        ui.show(snapshot(1, "c", vec![provider("local", true)], "local"));
        assert!(!ui.missing_key_warning());
    }

    #[test]
    fn non_setup_responses_are_ignored() {
        let mut ui = SetupUi::new();
        ui.show(snapshot(1, "c", vec![provider("local", false)], "local"));
        assert_eq!(
            ui.handle_response(HostResponse::Conversations(Vec::new())),
            Outcome::Ignored
        );
        assert!(matches!(
            ui.handle_response(HostResponse::Catalog(CatalogSnapshot {
                revision: "x".into(),
                items: Vec::new(),
            })),
            Outcome::Ignored
        ));
        // State untouched: still armed with the original snapshot.
        assert!(ui.needs_key());
        assert_eq!(ui.selected_provider_id(), Some("local"));
    }

    #[test]
    fn unknown_provider_selections_are_rejected_and_active_falls_back() {
        let mut ui = SetupUi::new();
        ui.show(snapshot(1, "c", vec![provider("local", false)], "local"));
        ui.select_provider("nope");
        assert_eq!(ui.selected_provider_id(), Some("local"));
        // Active id not in the list: the first provider is selected.
        let mut ui = SetupUi::new();
        ui.show(snapshot(1, "c", vec![provider("local", false)], "missing"));
        assert_eq!(ui.selected_provider_id(), Some("local"));
        assert!(ui.actionable(), "missing key keeps the dialog actionable");
        // Configured provider: not actionable on its own.
        let mut ui = SetupUi::new();
        let mut s = snapshot(1, "c", vec![provider("local", true)], "local");
        s.needs_onboarding = false;
        ui.show(s);
        assert!(!ui.actionable());
        assert!(!ui.needs_key());
    }

    #[test]
    fn configured_daemon_does_not_offer_setup_for_an_unconfigured_selection() {
        let mut ui = SetupUi::new();
        let mut s = snapshot(
            1,
            "c",
            vec![provider("local", false), provider("working", true)],
            "local",
        );
        s.needs_onboarding = false;
        ui.show(s);
        assert!(ui.needs_key());
        assert!(!ui.actionable());
        assert!(ui.can_apply(), "explicit setup remains usable");
    }

    #[test]
    fn render_smoke_is_headless_and_idle() {
        // Loading state.
        let mut ui = SetupUi::new();
        ui.poll();
        assert!(render_once(&mut ui).request.is_none());

        // Actionable form (missing key): renders without a click request.
        let mut ui = SetupUi::new();
        ui.show(snapshot(1, "c", vec![provider("local", false)], "local"));
        assert!(ui.actionable());
        assert!(render_once(&mut ui).request.is_none());

        // Error state.
        let mut ui = SetupUi::new();
        ui.show(snapshot(1, "c", vec![provider("local", false)], "local"));
        ui.handle_response(HostResponse::Error {
            code: HostErrorCode::Unavailable,
            message: "offline".into(),
        });
        assert!(render_once(&mut ui).request.is_none());

        // Success state.
        let mut ui = SetupUi::new();
        ui.handle_response(HostResponse::SetupApplied(applied(2, "Setup saved.")));
        assert!(render_once(&mut ui).request.is_none());
    }
}
