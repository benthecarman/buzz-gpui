//! Settings → Agents → Harnesses (buzz-app's `AgentSettings.tsx` and
//! `ClaudeHarnessSetup.tsx`): what this computer's host can run, the Pi,
//! Claude Code and Codex adapter installers, manual setup steps, and the
//! "Add harness" catalog.

use super::super::{button, form_group, inline_header, preference_row};
use crate::hosts::{HostConnection, Install};
use crate::ui::agents::harness::{self, status_label};
use crate::ui::overlay;
use buzz_host::api::{Harness, HarnessKind, HarnessStatus, InstallReport};
use buzz_host::harness::{HarnessPreset, harness_presets};
use buzz_host_rpc::ConnectionState;
use buzz_ui::{
    ActiveTheme, Brand, BrandMark, Button, ButtonSize, Colors, Dialog, DialogSize, Icon,
    IconButton, IconButtonSize, IconName, NavigationItem, Spinner, Surface, TypeStyled,
    tokens::radius,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::collections::HashSet;
use std::rc::Rc;

const ACP_HINT: &str = "Buzz talks to harnesses through the Agent Client Protocol (ACP). Goose \
                        ships with Buzz. Pi needs the buzz-pi-acp adapter. Hermes Agent uses its \
                        own ACP launcher and sign-in.";
const PI_COMMAND: &str = "npm install -g '@earendil-works/pi-coding-agent@>=0.99.0'";
const ADAPTER_COMMAND: &str = "npm install -g --install-links=true \
                               'git+https://github.com/salman1993/buzz-pi-acp.git#72015de'";
const CLAUDE_CLI_COMMAND: &str = "npm install -g @anthropic-ai/claude-code@2.1.289";
const CLAUDE_ADAPTER_COMMAND: &str = "npm install -g @agentclientprotocol/claude-agent-acp@0.85.1";
const CODEX_ADAPTER_COMMAND: &str = "npm install -g @agentclientprotocol/codex-acp@2.1.1";
/// Manual Pi steps: name, title and command.
const COMMANDS: [(&str, &str, &str); 2] = [
    ("Pi", "Install Pi", PI_COMMAND),
    ("Adapter", "Install the ACP adapter", ADAPTER_COMMAND),
];
const CORE: [HarnessKind; 3] = [HarnessKind::BuzzAgent, HarnessKind::Goose, HarnessKind::Pi];
const CONFIRM_FAILED: &str = "Couldn’t confirm harnesses. Showing the last check; try Check again.";

/// The rows buzz-app lists: Buzz Agent, Goose and Pi always (`None` when
/// the host did not report one), then presets other than Claude Code that
/// are installed, then Codex when the host offers it. Claude Code has its
/// own row.
pub fn rows(list: &[Harness]) -> Vec<(HarnessKind, Option<&Harness>)> {
    let find = |kind| list.iter().find(|harness| harness.kind == kind);
    let mut rows: Vec<_> = CORE.iter().map(|kind| (*kind, find(*kind))).collect();
    rows.extend(
        list.iter()
            .filter(|harness| preset(harness.kind).is_some() && harness.kind != HarnessKind::Claude)
            .filter(|harness| harness.command.is_some())
            .map(|harness| (harness.kind, Some(harness))),
    );
    rows.extend(find(HarnessKind::Codex).map(|codex| (HarnessKind::Codex, Some(codex))));
    rows
}

/// The core harnesses all carry a status: the host is new enough to list.
fn available(list: &[Harness]) -> bool {
    CORE.iter().all(|kind| {
        list.iter()
            .any(|harness| harness.kind == *kind && harness.status.is_some())
    })
}

fn preset_id(kind: HarnessKind) -> Option<&'static str> {
    match kind {
        HarnessKind::Hermes => Some("hermes"),
        HarnessKind::Claude => Some("claude"),
        _ => None,
    }
}

fn preset(kind: HarnessKind) -> Option<&'static HarnessPreset> {
    let id = preset_id(kind)?;
    harness_presets().iter().find(|preset| preset.id == id)
}

/// The presets "Add harness" offers: all but Claude Code.
fn catalog() -> Vec<(HarnessKind, &'static HarnessPreset)> {
    harness_presets()
        .iter()
        .filter(|preset| preset.id != "claude")
        .filter_map(|preset| match preset.id.as_str() {
            "hermes" => Some((HarnessKind::Hermes, preset)),
            _ => None,
        })
        .collect()
}

/// Claude Code's or Codex's sign-in, as `ClaudeHarnessSetup` checks it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Auth {
    Checking,
    /// `Some(false)`: signed out; `None`: could not confirm.
    Known(Option<bool>),
}

/// The Claude Code row's status label.
pub fn claude_label(status: Option<HarnessStatus>, auth: Auth) -> &'static str {
    match status {
        Some(HarnessStatus::Ready) => match auth {
            Auth::Known(Some(true)) => "Ready",
            Auth::Checking => "Checking sign-in…",
            Auth::Known(Some(false)) => "Sign-in needed",
            Auth::Known(None) => "Sign-in unconfirmed",
        },
        Some(HarnessStatus::CliNeeded) => "CLI needed",
        _ => "Adapter needed",
    }
}

/// The Codex row's status label: the tools' status, then its sign-in.
pub fn codex_label(status: Option<HarnessStatus>, auth: Auth) -> &'static str {
    match (status, auth) {
        (Some(HarnessStatus::Ready), Auth::Checking) => "Checking sign-in…",
        (Some(HarnessStatus::Ready), Auth::Known(Some(true))) => "Ready",
        (Some(HarnessStatus::Ready), Auth::Known(Some(false))) => "Sign-in needed",
        (Some(HarnessStatus::Ready), Auth::Known(None)) => "Sign-in unconfirmed",
        (status, _) => status_label(status),
    }
}

/// The hint under the Codex row, when it needs one.
pub fn codex_hint(status: Option<HarnessStatus>, auth: Auth) -> Option<&'static str> {
    match (status, auth) {
        (Some(HarnessStatus::CliNeeded), _) => Some("Install the Codex CLI, then use Check again."),
        (Some(HarnessStatus::Ready), Auth::Known(Some(false) | None)) => {
            Some("Run codex login in your terminal, then use Check again.")
        }
        _ => None,
    }
}

/// The Pi row's result line after an install.
pub fn pi_result(report: &InstallReport) -> String {
    let mut text = format!(
        "Pi and its adapter are up to date. Restart running Pi agents to use them. Restarted {} \
         waiting agents.",
        report.restarted
    );
    if report.restart_failures > 0 {
        text.push_str(&format!(
            " {} agents could not restart; check Agents.",
            report.restart_failures
        ));
    }
    text
}

/// Splits setup prose on backticks, keeping commands as code
/// (`PresetSetupHint`). Returns the plain text and the code ranges.
pub fn hint_spans(hint: &str) -> (String, Vec<std::ops::Range<usize>>) {
    let mut text = String::new();
    let mut code = Vec::new();
    for (index, part) in hint.split('`').enumerate() {
        let start = text.len();
        text.push_str(part);
        // Odd parts sit between a pair of backticks; a lone trailing one
        // stays prose.
        if index % 2 == 1 && hint.split('`').count() > index + 1 {
            code.push(start..text.len());
        }
    }
    (text, code)
}

fn hint_text(hint: &str, c: &Colors) -> StyledText {
    let (text, code) = hint_spans(hint);
    let style = HighlightStyle {
        background_color: Some(c.affordance_subtle),
        ..Default::default()
    };
    StyledText::new(text).with_highlights(code.into_iter().map(|range| (range, style)))
}

/// What the last sign-in check was made for; another check runs when it
/// changes.
#[derive(Clone, Debug, PartialEq)]
struct AuthKey {
    report: Option<InstallReport>,
    checks: u64,
}

/// One harness's sign-in check.
struct SignIn {
    auth: Auth,
    key: Option<AuthKey>,
}

impl Default for SignIn {
    fn default() -> Self {
        Self {
            auth: Auth::Checking,
            key: None,
        }
    }
}

pub struct Harnesses {
    /// This computer's host; `None` when it could not start.
    host: Option<Entity<HostConnection>>,
    checking: bool,
    /// Check again presses, which also check sign-ins again.
    checks: u64,
    claude: SignIn,
    codex: SignIn,
    catalog_open: bool,
    selected: Option<HarnessKind>,
    copy_message: Option<SharedString>,
    /// Expanded disclosures.
    expanded: HashSet<&'static str>,
    _observer: Option<Subscription>,
}

impl Harnesses {
    /// Checks again on open, as buzz-app does whenever the page becomes
    /// active.
    pub fn new(host: Option<Entity<HostConnection>>, cx: &mut Context<Self>) -> Self {
        let observer = host
            .as_ref()
            .map(|host| cx.observe(host, |_, _, cx| cx.notify()));
        let this = Self {
            host,
            checking: false,
            checks: 0,
            claude: SignIn::default(),
            codex: SignIn::default(),
            catalog_open: false,
            selected: None,
            copy_message: None,
            expanded: HashSet::new(),
            _observer: observer,
        };
        if let Some(host) = &this.host {
            host.update(cx, |host, cx| host.refresh(cx).detach());
        }
        this
    }

    fn check_again(&mut self, cx: &mut Context<Self>) {
        let Some(host) = self.host.clone() else {
            return;
        };
        self.checking = true;
        let refresh = host.update(cx, |host, cx| host.refresh(cx));
        cx.spawn(async move |this, cx| {
            refresh.await;
            let _ = this.update(cx, |this, cx| {
                this.checking = false;
                this.checks += 1;
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn sign_in(&mut self, kind: HarnessKind) -> &mut SignIn {
        match kind {
            HarnessKind::Codex => &mut self.codex,
            _ => &mut self.claude,
        }
    }

    /// Checks Claude Code's or Codex's sign-in once its tools are ready and
    /// no install runs, again after each install and each Check again.
    fn check_auth(&mut self, kind: HarnessKind, cx: &mut Context<Self>) {
        let Some(host) = self.host.clone() else {
            return;
        };
        let state = host.read(cx);
        let ready = state
            .harnesses
            .as_ref()
            .and_then(|list| list.iter().find(|h| h.kind == kind))
            .is_some_and(|harness| harness.status == Some(HarnessStatus::Ready));
        let install = match kind {
            HarnessKind::Codex => &state.codex_install,
            _ => &state.claude_install,
        };
        if !ready || install.installing {
            return;
        }
        let key = AuthKey {
            report: install.report.clone(),
            checks: self.checks,
        };
        if self.sign_in(kind).key.as_ref() == Some(&key) {
            return;
        }
        *self.sign_in(kind) = SignIn {
            auth: Auth::Checking,
            key: Some(key.clone()),
        };
        let request = match kind {
            HarnessKind::Codex => buzz_host::api::Request::CodexAuth,
            _ => buzz_host::api::Request::ClaudeAuth,
        };
        let response = host.update(cx, |host, cx| host.request(request, cx));
        cx.spawn(async move |this, cx| {
            let response = response.await;
            let _ = this.update(cx, |this, cx| {
                let sign_in = this.sign_in(kind);
                if sign_in.key.as_ref() != Some(&key) {
                    return;
                }
                sign_in.auth = Auth::Known(match response {
                    Ok(buzz_host::api::Response::ClaudeAuth(signed_in))
                        if kind == HarnessKind::Claude =>
                    {
                        signed_in
                    }
                    Ok(buzz_host::api::Response::CodexAuth(signed_in))
                        if kind == HarnessKind::Codex =>
                    {
                        signed_in
                    }
                    _ => None,
                });
                cx.notify();
            });
        })
        .detach();
    }

    fn toggle(&mut self, id: &'static str, cx: &mut Context<Self>) {
        if !self.expanded.remove(id) {
            self.expanded.insert(id);
        }
        cx.notify();
    }

    /// A `<details>` disclosure: the summary toggles the content.
    fn details(
        &self,
        id: &'static str,
        summary: impl Into<SharedString>,
        content: impl FnOnce() -> AnyElement,
        cx: &mut Context<Self>,
    ) -> Div {
        let open = self.expanded.contains(id);
        let c = *cx.colors();
        div()
            .flex()
            .flex_col()
            .child(
                div()
                    .id(id)
                    .flex()
                    .items_center()
                    .gap(rems(0.25))
                    .cursor_pointer()
                    .child(
                        Icon::new(if open {
                            IconName::CaretDown
                        } else {
                            IconName::CaretRight
                        })
                        .size(12.)
                        .color(c.text_standard),
                    )
                    .child(summary.into())
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle(id, cx))),
            )
            .when(open, |this| this.child(content()))
    }

    fn copy(&mut self, name: &'static str, command: &'static str, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(command.to_string()));
        self.copy_message = Some(format!("{name} command copied.").into());
        cx.notify();
    }

    fn header(&self, cx: &mut Context<Self>) -> Div {
        let state = self.host.as_ref().map(|host| host.read(cx));
        let disabled = state.is_none_or(|state| {
            !matches!(state.state, ConnectionState::Connected { .. }) || state.installing()
        });
        let check: AnyElement = if self.checking {
            div()
                .flex()
                .items_center()
                .justify_center()
                .size(rems(2.))
                .child(Spinner::new().size(16.))
                .into_any_element()
        } else {
            IconButton::new("harnesses-refresh", IconName::ArrowsClockwise)
                .size(IconButtonSize::Sm)
                .disabled(disabled)
                .tooltip("Check again")
                .on_click(cx.listener(|this, _, _, cx| this.check_again(cx)))
                .into_any_element()
        };
        inline_header(
            "Harnesses",
            None,
            vec![
                IconButton::new("harnesses-about-acp", IconName::Question)
                    .size(IconButtonSize::Sm)
                    .tooltip(ACP_HINT)
                    .into_any_element(),
                check,
            ],
            cx,
        )
    }

    fn body(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let c = *cx.colors();
        let secondary = |text: &'static str| {
            div()
                .text_body_sm()
                .text_color(c.text_subtle)
                .child(text)
                .into_any_element()
        };
        let Some(host) = self.host.clone() else {
            return vec![secondary("Harness detection requires the desktop app.")];
        };
        let state = host.read(cx);
        let failed = state.harnesses_failed;
        let list = match &state.harnesses {
            None if !failed => return vec![secondary("Checking harnesses…")],
            list => list.clone().unwrap_or_default(),
        };
        if !available(&list) {
            return vec![secondary(if failed {
                "Couldn’t check harnesses. Select Check again to retry."
            } else {
                "Update the desktop app to check harnesses."
            })];
        }
        let mut parts = Vec::new();
        if failed {
            parts.push(
                div()
                    .text_body_sm()
                    .child(CONFIRM_FAILED)
                    .into_any_element(),
            );
        }
        let mut items: Vec<AnyElement> = rows(&list)
            .into_iter()
            .map(|(kind, harness)| match (kind, harness) {
                (HarnessKind::Codex, Some(codex)) => {
                    self.codex_row(&host, codex, cx).into_any_element()
                }
                _ => self.row(&host, kind, harness, cx).into_any_element(),
            })
            .collect();
        if let Some(claude) = list.iter().find(|h| h.kind == HarnessKind::Claude) {
            items.push(self.claude_row(&host, claude, cx).into_any_element());
        }
        parts.push(
            div()
                .flex()
                .flex_col()
                .my(rems(-0.25))
                .children(items.into_iter().enumerate().map(|(index, item)| {
                    div()
                        .when(index > 0, |this| {
                            this.border_t_1().border_color(c.border_standard)
                        })
                        .child(item)
                }))
                .into_any_element(),
        );
        parts.push(
            div()
                .flex()
                .child(
                    button("add-harness", "Add harness")
                        .outline()
                        .size(ButtonSize::Sm)
                        .icon(IconName::Plus)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.catalog_open = true;
                            cx.notify();
                        })),
                )
                .into_any_element(),
        );
        parts
    }

    /// One `li`: logo and label, status and the Pi install button, then
    /// setup details.
    fn row(
        &self,
        host: &Entity<HostConnection>,
        kind: HarnessKind,
        harness: Option<&Harness>,
        cx: &mut Context<Self>,
    ) -> Div {
        let c = *cx.colors();
        let state = host.read(cx);
        let connected = matches!(state.state, ConnectionState::Connected { .. });
        let pi = state.pi_install.clone();
        let other_installing = state.claude_install.installing || state.codex_install.installing;
        let label: SharedString = harness.map_or_else(
            || match kind {
                HarnessKind::BuzzAgent => "Buzz Agent".into(),
                HarnessKind::Goose => "Goose".into(),
                _ => "Pi".into(),
            },
            |harness| harness.label.clone().into(),
        );
        let status = harness.and_then(|harness| harness.status);
        let ready = status == Some(HarnessStatus::Ready);
        let mut trailing = div()
            .flex()
            .items_center()
            .gap(rems(0.5))
            .child(div().text_color(c.text_subtle).child(status_label(status)));
        if let Some(harness) = harness.filter(|h| h.kind == HarnessKind::Pi)
            && (!ready || harness.update_supported)
            && harness.install_supported
            && !pi.report.as_ref().is_some_and(|report| report.ready)
        {
            let host = host.clone();
            trailing = trailing.child(
                Button::new("install-pi", if ready { "Update Pi" } else { "Install" })
                    .size(ButtonSize::Sm)
                    .loading(pi.installing)
                    .disabled(!connected || other_installing || pi.installing)
                    .on_click(move |_, _, cx| {
                        host.update(cx, |host, cx| host.install(HarnessKind::Pi, cx))
                    }),
            );
        }
        let mut row = div().flex().flex_col().py(rems(0.75)).text_body_sm().child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .justify_between()
                .gap(rems(0.5))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(rems(0.75))
                        .child(harness::mark(kind, 32.))
                        .child(label.clone()),
                )
                .child(trailing),
        );
        if let Some(harness) = harness.filter(|h| !ready && preset(h.kind).is_some()) {
            row = row.child(self.preset_details(harness, cx));
        }
        if kind == HarnessKind::Pi {
            let report = pi.report.as_ref();
            if pi.installing
                || report.is_some_and(|r| r.ready || r.error.is_some())
                || pi.error.is_some()
                || !ready
            {
                row = row.child(self.pi_details(harness, &pi, ready, cx));
            }
        }
        row
    }

    /// buzz-app's `.piSetup` indent: 12px above, a rule on the left.
    fn setup_block(c: &Colors) -> Div {
        div()
            .flex()
            .flex_col()
            .gap(rems(0.75))
            .mt(rems(0.75))
            .ml(rems(0.5))
            .pl(rems(1.))
            .border_l_1()
            .border_color(c.border_standard)
    }

    fn preset_details(&self, harness: &Harness, cx: &mut Context<Self>) -> Div {
        let c = *cx.colors();
        let label = harness.label.clone();
        let setup = preset(harness.kind);
        Self::setup_block(&c)
            .child(div().text_color(c.text_subtle).child(format!(
                "Uses the default model and credentials configured in {label}. Install and \
                 update the harness yourself, then use Check again."
            )))
            .child(self.details(
                "preset-manual-setup",
                format!("Manual {label} setup"),
                || {
                    preset_setup(&label, setup, &c)
                        .mt(rems(0.75))
                        .into_any_element()
                },
                cx,
            ))
    }

    fn install_failure(
        &self,
        id: &'static str,
        summary: &'static str,
        install: &Install,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let error = install
            .report
            .as_ref()
            .and_then(|report| report.error.clone())
            .or_else(|| install.error.as_ref().map(|error| error.to_string()))?;
        let report = install.report.clone();
        Some(
            div()
                .flex()
                .flex_col()
                .gap(rems(0.5))
                .child(error)
                .when_some(report, |this, report| {
                    this.child(self.details(
                        id,
                        summary,
                        move || {
                            div()
                                .flex()
                                .flex_col()
                                .gap(rems(0.5))
                                .child(report.log_path.clone())
                                .child(code_block(if report.output.is_empty() {
                                    "No output was recorded.".to_string()
                                } else {
                                    report.output.clone()
                                }))
                                .into_any_element()
                        },
                        cx,
                    ))
                }),
        )
    }

    fn pi_details(
        &self,
        harness: Option<&Harness>,
        install: &Install,
        ready: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        let c = *cx.colors();
        let mut block = Self::setup_block(&c);
        if install.installing {
            block = block.child("Installing Pi and its ACP adapter…");
        }
        if let Some(report) = install.report.as_ref().filter(|report| report.ready)
            && !install.installing
            && ready
        {
            block = block.child(pi_result(report));
        }
        if !install.installing
            && let Some(failure) =
                self.install_failure("pi-install-log", "Pi install log", install, cx)
        {
            block = block.child(failure);
        }
        if !ready {
            let supported = harness.is_some_and(|harness| harness.install_supported);
            let lead = if supported {
                "Click Install. Buzz installs Node.js, Pi, and its ACP adapter for you."
            } else {
                "Use Manual setup on this device, then click Check again."
            };
            let message = self.copy_message.clone();
            let page = cx.entity().downgrade();
            block = block.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(rems(0.75))
                    .child(div().text_color(c.text_subtle).child(format!(
                        "{lead} Your existing CLI setup and sign-in are left untouched."
                    )))
                    .child(self.details(
                        "pi-manual-setup",
                        "Manual setup",
                        move || manual_steps(page, message, c),
                        cx,
                    )),
            );
        }
        block
    }

    fn claude_row(
        &self,
        host: &Entity<HostConnection>,
        setup: &Harness,
        cx: &mut Context<Self>,
    ) -> Div {
        let c = *cx.colors();
        let state = host.read(cx);
        let connected = matches!(state.state, ConnectionState::Connected { .. });
        let install = state.claude_install.clone();
        let other_installing = state.pi_install.installing || state.codex_install.installing;
        let tools_ready = setup.status == Some(HarnessStatus::Ready);
        let auth = self.claude.auth;
        let needs_guidance =
            !tools_ready || matches!(auth, Auth::Known(Some(false)) | Auth::Known(None));
        let failed = install
            .report
            .as_ref()
            .is_some_and(|report| report.error.is_some())
            || install.error.is_some();
        let show_details = install.installing || failed || needs_guidance;
        let mut trailing = div().flex().items_center().gap(rems(0.5)).child(
            div()
                .text_color(c.text_subtle)
                .child(claude_label(setup.status, auth)),
        );
        if !tools_ready && setup.install_supported {
            let host = host.clone();
            trailing = trailing.child(
                Button::new("install-claude", "Install")
                    .size(ButtonSize::Sm)
                    .loading(install.installing)
                    .disabled(!connected || other_installing || install.installing)
                    .on_click(move |_, _, cx| {
                        host.update(cx, |host, cx| host.install(HarnessKind::Claude, cx))
                    }),
            );
        }
        let mut row = div().flex().flex_col().py(rems(0.75)).text_body_sm().child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .justify_between()
                .gap(rems(0.5))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(rems(0.75))
                        .child(Brand::new(BrandMark::Claude).size(32.))
                        .child("Claude Code"),
                )
                .child(trailing),
        );
        if !show_details {
            return row;
        }
        let mut block = Self::setup_block(&c);
        if install.installing {
            block = block.child("Installing Claude Code and its ACP adapter…");
        } else if let Some(failure) = self.install_failure(
            "claude-install-log",
            "Claude Code install log",
            &install,
            cx,
        ) {
            block = block.child(failure);
        }
        if !tools_ready {
            block = block
                .child(div().text_color(c.text_subtle).child(if setup.install_supported {
                    "Click Install. Buzz installs Node.js, Claude Code, and its ACP adapter for you."
                } else {
                    "Use Manual setup on this device, then click Check again."
                }))
                .child(self.details(
                    "claude-manual-setup",
                    "Manual Claude Code setup",
                    || {
                        div()
                            .flex()
                            .flex_col()
                            .gap(rems(0.75))
                            .mt(rems(0.75))
                            .child("Install Node.js 22 or newer, then run these in your terminal:")
                            .child(code_block(CLAUDE_CLI_COMMAND.to_string()))
                            .child(code_block(CLAUDE_ADAPTER_COMMAND.to_string()))
                            .child("Use Check again to refresh the installation status.")
                            .into_any_element()
                    },
                    cx,
                ));
        }
        if tools_ready && auth == Auth::Known(None) {
            block = block.child(div().text_color(c.text_subtle).child(
                "Couldn’t confirm Claude Code sign-in. Use Check again to retry, or sign in \
                 below.",
            ));
        }
        if needs_guidance && let Some(login) = setup.login_command.clone() {
            block = block.child(self.details(
                "claude-sign-in",
                "Sign in to Claude Code",
                || {
                    div()
                        .flex()
                        .flex_col()
                        .gap(rems(0.75))
                        .mt(rems(0.75))
                        .child("Run this in your terminal and follow Claude’s sign-in steps:")
                        .child(code_block(login))
                        .into_any_element()
                },
                cx,
            ));
        }
        row = row.child(block);
        row
    }

    /// The Codex row: its tools' status, then its sign-in once they are
    /// found; the adapter installer; and how to finish setup.
    fn codex_row(
        &self,
        host: &Entity<HostConnection>,
        codex: &Harness,
        cx: &mut Context<Self>,
    ) -> Div {
        let c = *cx.colors();
        let state = host.read(cx);
        let connected = matches!(state.state, ConnectionState::Connected { .. });
        let install = state.codex_install.clone();
        let other_installing = state.pi_install.installing || state.claude_install.installing;
        let tools_ready = codex.status == Some(HarnessStatus::Ready);
        let auth = self.codex.auth;
        let offer_install =
            codex.install_supported && codex.status == Some(HarnessStatus::AdapterNeeded);
        let mut trailing = div().flex().items_center().gap(rems(0.5)).child(
            div()
                .text_color(c.text_subtle)
                .child(codex_label(codex.status, auth)),
        );
        if offer_install {
            let host = host.clone();
            trailing = trailing.child(
                Button::new("install-codex", "Install")
                    .size(ButtonSize::Sm)
                    .loading(install.installing)
                    .disabled(!connected || other_installing || install.installing)
                    .on_click(move |_, _, cx| {
                        host.update(cx, |host, cx| host.install(HarnessKind::Codex, cx))
                    }),
            );
        }
        let mut row = div().flex().flex_col().py(rems(0.75)).text_body_sm().child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .justify_between()
                .gap(rems(0.5))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(rems(0.75))
                        .child(harness::mark(HarnessKind::Codex, 32.))
                        .child(codex.label.clone()),
                )
                .child(trailing),
        );
        if install.installing {
            row = row.child(
                Self::setup_block(&c).child("Installing Node.js and the Codex ACP adapter…"),
            );
        } else if let Some(failure) = self.install_failure(
            "codex-install-log",
            "Codex ACP adapter install log",
            &install,
            cx,
        ) {
            row = row.child(Self::setup_block(&c).child(failure));
        }
        if offer_install && !install.installing {
            row = row.child(
                Self::setup_block(&c)
                    .child(div().text_color(c.text_subtle).child(
                        "Click Install. Buzz installs Node.js and the Codex ACP adapter for you. \
                         Your Codex CLI and sign-in are left untouched.",
                    ))
                    .child(self.details(
                        "codex-manual-setup",
                        "Manual Codex ACP adapter setup",
                        || {
                            code_block(CODEX_ADAPTER_COMMAND.to_string())
                                .mt(rems(0.75))
                                .into_any_element()
                        },
                        cx,
                    )),
            );
        }
        if let Some(hint) = codex_hint(codex.status, auth) {
            row = row.child(div().mt(rems(0.5)).text_color(c.text_subtle).child(hint));
        }
        // The host may be another machine, so show where to sign in.
        if tools_ready
            && matches!(auth, Auth::Known(Some(false) | None))
            && let Some(login) = codex.login_command.clone()
        {
            row = row.child(Self::setup_block(&c).child(self.details(
                "codex-sign-in",
                "Sign in to Codex",
                || {
                    div()
                        .flex()
                        .flex_col()
                        .gap(rems(0.75))
                        .mt(rems(0.75))
                        .child("Run this in your terminal and follow Codex’s sign-in steps:")
                        .child(code_block(login))
                        .into_any_element()
                },
                cx,
            )));
        }
        row
    }

    /// The "Add harness" dialog: the preset catalog beside the selected
    /// preset's status and setup.
    fn catalog_dialog(&self, cx: &mut Context<Self>) -> AnyElement {
        let c = *cx.colors();
        let presets = catalog();
        let list = self
            .host
            .as_ref()
            .and_then(|host| host.read(cx).harnesses.clone())
            .unwrap_or_default();
        let failed = self
            .host
            .as_ref()
            .is_some_and(|host| host.read(cx).harnesses_failed);
        let (kind, setup) = presets
            .iter()
            .find(|(kind, _)| Some(*kind) == self.selected)
            .or(presets.first())
            .copied()
            .unzip();
        let selected = kind.and_then(|kind| list.iter().find(|h| h.kind == kind));
        let label: SharedString = selected
            .map(|h| h.label.clone())
            .or_else(|| setup.map(|setup| setup.label.clone()))
            .unwrap_or_else(|| "Harness".into())
            .into();
        let installed = selected.is_some_and(|h| h.command.is_some());
        let disabled = self.checking
            || self.host.as_ref().is_none_or(|host| {
                let state = host.read(cx);
                !matches!(state.state, ConnectionState::Connected { .. }) || state.installing()
            });
        let mark = |kind: Option<HarnessKind>, size: f32| match kind {
            Some(HarnessKind::Hermes) => {
                Brand::new(BrandMark::Hermes).size(size).into_any_element()
            }
            _ => Icon::new(IconName::TerminalWindow)
                .size(size)
                .into_any_element(),
        };
        let sidebar = div()
            .flex()
            .flex_col()
            .gap(rems(0.25))
            .w(rems(12.))
            .flex_none()
            .pr(rems(1.))
            .border_r_1()
            .border_color(c.border_standard)
            .child(
                div()
                    .text_label_sm()
                    .text_color(c.text_subtle)
                    .mb(rems(0.5))
                    .child(if installed { "Installed" } else { "Setup" }),
            )
            .children(presets.iter().map(|(preset_kind, preset)| {
                let preset_kind = *preset_kind;
                let name = list
                    .iter()
                    .find(|h| h.kind == preset_kind)
                    .map_or_else(|| preset.label.clone(), |h| h.label.clone());
                NavigationItem::new(SharedString::from(format!("preset-{}", preset.id)), name)
                    .leading(mark(Some(preset_kind), 24.))
                    .surface(Surface::Floating)
                    .selected(Some(preset_kind) == kind)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.selected = Some(preset_kind);
                        cx.notify();
                    }))
            }));
        let hint = setup
            .map(|setup| setup.setup_hint.clone())
            .unwrap_or_default();
        let details = div()
            .flex()
            .flex_col()
            .gap(rems(1.5))
            .flex_1()
            .min_w_0()
            .p(rems(1.5))
            .rounded(radius::CONTROL)
            .bg(c.surface_inset)
            .text_body_sm()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(rems(0.75))
                    .child(mark(kind, 48.))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(div().text_heading().child(label.clone()))
                            .child(
                                div()
                                    .text_color(c.text_subtle)
                                    .child(status_label(selected.and_then(|h| h.status))),
                            ),
                    ),
            )
            .when(failed, |this| this.child(CONFIRM_FAILED))
            .when(selected.is_none(), |this| {
                this.child(format!("Update the desktop app to check {label}."))
            })
            .child(
                div()
                    .text_color(c.text_subtle)
                    .child(format!("Model and sign-in are managed in {label}.")),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(rems(0.75))
                    .child(div().text_label_sm().child("Setup"))
                    .child(format!(
                        "Install {label} with ACP support, then follow the setup guide to \
                         connect it to Buzz."
                    ))
                    .child(self.details(
                        "catalog-terminal-setup",
                        "Terminal setup",
                        move || {
                            div()
                                .mt(rems(0.75))
                                .text_color(c.text_subtle)
                                .child(hint_text(&hint, &c))
                                .into_any_element()
                        },
                        cx,
                    )),
            )
            .when_some(
                selected
                    .and_then(|h| h.command.clone())
                    .filter(|_| installed),
                |this, command| {
                    this.child(preference_row(
                        None,
                        "Executable",
                        Some(div().text_mono().child(command).into_any_element()),
                        Vec::new(),
                        cx,
                    ))
                },
            );
        let url = setup.map(|setup| setup.setup_url.clone());
        Dialog::new("add-harness", "Add harness")
            .size(DialogSize::Wide)
            .header_action(
                IconButton::new("catalog-check-again", IconName::ArrowsClockwise)
                    .size(IconButtonSize::Sm)
                    .surface(Surface::Floating)
                    .disabled(disabled)
                    .tooltip("Check again")
                    .on_click(cx.listener(|this, _, _, cx| this.check_again(cx))),
            )
            .on_close(cx.listener(|this, _, _, cx| {
                this.catalog_open = false;
                cx.notify();
            }))
            .when_some(url, |this, url| {
                this.footer(
                    Button::new("setup-guide", "Setup guide")
                        .prominent()
                        .icon(IconName::ArrowSquareOut)
                        .on_click(move |_, _, cx| cx.open_url(&url)),
                )
            })
            .child(
                div()
                    .flex()
                    .gap(rems(1.5))
                    .min_h(rems(24.))
                    .child(sidebar)
                    .child(details),
            )
            .into_any_element()
    }
}

/// `.command`: a mono command or log, wrapping anywhere.
fn code_block(text: String) -> Div {
    div().w_full().min_w_0().text_mono().child(text)
}

/// `PresetSetup`: the guide link, the hint, and how to recheck.
fn preset_setup(label: &str, setup: Option<&'static HarnessPreset>, c: &Colors) -> Div {
    let Some(setup) = setup else {
        return div();
    };
    let (hint, code) = hint_spans(&setup.setup_hint);
    let lead = "Follow the ";
    let link = format!("{label} setup guide");
    let mut text = format!("{lead}{link} for installation instructions. ");
    let link_range = lead.len()..lead.len() + link.len();
    let offset = text.len();
    text.push_str(&hint);
    let code_style = HighlightStyle {
        background_color: Some(c.affordance_subtle),
        ..Default::default()
    };
    let link_style = HighlightStyle {
        underline: Some(UnderlineStyle {
            thickness: px(1.),
            color: None,
            wavy: false,
        }),
        ..Default::default()
    };
    let highlights = std::iter::once((link_range.clone(), link_style)).chain(
        code.into_iter()
            .map(|range| (range.start + offset..range.end + offset, code_style)),
    );
    let url = setup.setup_url.clone();
    div()
        .flex()
        .flex_col()
        .gap(rems(0.75))
        .child(
            InteractiveText::new(
                SharedString::from(format!("preset-guide-{}", setup.id)),
                StyledText::new(text).with_highlights(highlights),
            )
            .on_click(vec![link_range], move |_, _, cx| cx.open_url(&url)),
        )
        .child("Use Check again to refresh the installation status.")
}

/// The manual Pi steps, with copy buttons for the two commands.
fn manual_steps(
    page: WeakEntity<Harnesses>,
    message: Option<SharedString>,
    c: Colors,
) -> AnyElement {
    let step = |title: &'static str, subtitle: AnyElement, trailing: Option<AnyElement>| {
        div()
            .flex()
            .items_center()
            .gap(rems(1.))
            .py(rems(0.75))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap(rems(0.125))
                    .child(div().text_label_sm().child(title))
                    .child(
                        div()
                            .text_caption()
                            .text_color(c.text_subtle)
                            .child(subtitle),
                    ),
            )
            .children(trailing)
    };
    let mut steps = vec![step(
        "Install Node.js",
        "Use version 22.19 or newer. Run the following commands in your terminal."
            .into_any_element(),
        None,
    )];
    for (name, title, command) in COMMANDS {
        let page = page.clone();
        steps.push(step(
            title,
            div().text_mono().child(command).into_any_element(),
            Some(
                IconButton::new(SharedString::from(format!("copy-{name}")), IconName::Copy)
                    .size(IconButtonSize::Sm)
                    .tooltip(format!("Copy {name} command"))
                    .on_click(move |_, _, cx| {
                        let _ = page.update(cx, |this, cx| this.copy(name, command, cx));
                    })
                    .into_any_element(),
            ),
        ));
    }
    steps.push(step(
        "Check the installation",
        "Use Check again at the top of Harnesses to refresh the status.".into_any_element(),
        None,
    ));
    let count = steps.len();
    div()
        .flex()
        .flex_col()
        .gap(rems(0.75))
        .mt(rems(0.75))
        .child(
            div()
                .flex()
                .flex_col()
                .my(rems(0.75))
                .children(steps.into_iter().enumerate().map(|(index, step)| {
                    step.when(index > 0 && index < count, |this| {
                        this.border_t_1().border_color(c.border_standard)
                    })
                })),
        )
        .children(message)
        .into_any_element()
}

impl Render for Harnesses {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.check_auth(HarnessKind::Claude, cx);
        self.check_auth(HarnessKind::Codex, cx);
        let body = self.body(cx);
        let page = cx.entity().downgrade();
        let close: overlay::Dismiss = Rc::new(move |_, cx| {
            let _ = page.update(cx, |this, cx| {
                this.catalog_open = false;
                cx.notify();
            });
        });
        let dialog = self
            .catalog_open
            .then(|| overlay::modal(window, self.catalog_dialog(cx), Some(close)));
        div()
            .child(self.header(cx))
            .child(form_group(body, cx))
            .children(dialog)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    fn harness(kind: HarnessKind, status: Option<HarnessStatus>, installed: bool) -> Harness {
        Harness {
            kind,
            label: format!("{kind:?}"),
            command: installed.then(|| format!("/bin/{kind:?}")),
            missing: Vec::new(),
            setup_hint: String::new(),
            setup_url: None,
            status,
            install_supported: true,
            update_supported: false,
            login_command: None,
        }
    }

    #[test]
    fn status_labels_match_buzz_app() {
        assert_eq!(status_label(Some(HarnessStatus::Ready)), "Ready");
        assert_eq!(status_label(Some(HarnessStatus::CliNeeded)), "CLI needed");
        assert_eq!(
            status_label(Some(HarnessStatus::AdapterNeeded)),
            "Adapter needed"
        );
        assert_eq!(status_label(None), "Unknown");
    }

    #[test]
    fn core_harnesses_are_always_listed_and_presets_only_when_installed() {
        let ready = Some(HarnessStatus::Ready);
        let list = vec![
            harness(HarnessKind::Claude, ready, true),
            harness(HarnessKind::Goose, ready, true),
            harness(HarnessKind::Hermes, Some(HarnessStatus::CliNeeded), false),
        ];
        let kinds: Vec<_> = rows(&list)
            .into_iter()
            .map(|(kind, harness)| (kind, harness.is_some()))
            .collect();
        assert_eq!(
            kinds,
            [
                (HarnessKind::BuzzAgent, false),
                (HarnessKind::Goose, true),
                (HarnessKind::Pi, false),
            ]
        );
        assert!(!available(&list));
        let list = vec![
            harness(HarnessKind::BuzzAgent, ready, true),
            harness(HarnessKind::Goose, ready, true),
            harness(HarnessKind::Pi, Some(HarnessStatus::AdapterNeeded), false),
            harness(HarnessKind::Hermes, ready, true),
            harness(HarnessKind::Claude, ready, true),
        ];
        assert!(available(&list));
        let kinds: Vec<_> = rows(&list).into_iter().map(|(kind, _)| kind).collect();
        assert_eq!(
            kinds,
            [
                HarnessKind::BuzzAgent,
                HarnessKind::Goose,
                HarnessKind::Pi,
                HarnessKind::Hermes
            ]
        );
    }

    #[test]
    fn claude_needs_tools_then_a_confirmed_sign_in() {
        let ready = Some(HarnessStatus::Ready);
        assert_eq!(claude_label(ready, Auth::Known(Some(true))), "Ready");
        assert_eq!(claude_label(ready, Auth::Checking), "Checking sign-in…");
        assert_eq!(
            claude_label(ready, Auth::Known(Some(false))),
            "Sign-in needed"
        );
        assert_eq!(
            claude_label(ready, Auth::Known(None)),
            "Sign-in unconfirmed"
        );
        assert_eq!(
            claude_label(Some(HarnessStatus::CliNeeded), Auth::Checking),
            "CLI needed"
        );
        assert_eq!(
            claude_label(Some(HarnessStatus::AdapterNeeded), Auth::Checking),
            "Adapter needed"
        );
    }

    #[test]
    fn codex_is_listed_last_when_the_host_offers_it() {
        let ready = Some(HarnessStatus::Ready);
        let list = vec![
            harness(HarnessKind::Codex, Some(HarnessStatus::CliNeeded), false),
            harness(HarnessKind::BuzzAgent, ready, true),
            harness(HarnessKind::Goose, ready, true),
            harness(HarnessKind::Pi, ready, true),
            harness(HarnessKind::Hermes, ready, true),
        ];
        let kinds: Vec<_> = rows(&list).into_iter().map(|(kind, _)| kind).collect();
        assert_eq!(
            kinds,
            [
                HarnessKind::BuzzAgent,
                HarnessKind::Goose,
                HarnessKind::Pi,
                HarnessKind::Hermes,
                HarnessKind::Codex,
            ]
        );
        assert!(
            !rows(&list[1..])
                .iter()
                .any(|(kind, _)| *kind == HarnessKind::Codex)
        );
    }

    #[test]
    fn codex_needs_its_tools_then_a_confirmed_sign_in() {
        let ready = Some(HarnessStatus::Ready);
        assert_eq!(codex_label(ready, Auth::Checking), "Checking sign-in…");
        assert_eq!(codex_label(ready, Auth::Known(Some(true))), "Ready");
        assert_eq!(
            codex_label(ready, Auth::Known(Some(false))),
            "Sign-in needed"
        );
        assert_eq!(codex_label(ready, Auth::Known(None)), "Sign-in unconfirmed");
        let tools =
            |status, label| assert_eq!(codex_label(Some(status), Auth::Known(Some(true))), label);
        tools(HarnessStatus::CliNeeded, "CLI needed");
        tools(HarnessStatus::AdapterNeeded, "Adapter needed");
        tools(HarnessStatus::CheckNeeded, "Check needed");
        tools(HarnessStatus::NotEnabled, "Not enabled");
        assert_eq!(codex_label(None, Auth::Checking), "Unknown");

        assert_eq!(
            codex_hint(Some(HarnessStatus::CliNeeded), Auth::Checking),
            Some("Install the Codex CLI, then use Check again.")
        );
        let login = Some("Run codex login in your terminal, then use Check again.");
        assert_eq!(codex_hint(ready, Auth::Known(Some(false))), login);
        assert_eq!(codex_hint(ready, Auth::Known(None)), login);
        assert_eq!(codex_hint(ready, Auth::Checking), None);
        assert_eq!(codex_hint(ready, Auth::Known(Some(true))), None);
        assert_eq!(
            codex_hint(Some(HarnessStatus::AdapterNeeded), Auth::Known(None)),
            None
        );
    }

    #[test]
    fn pi_results_count_restarts() {
        let report = InstallReport {
            ready: true,
            restarted: 2,
            ..Default::default()
        };
        assert_eq!(
            pi_result(&report),
            "Pi and its adapter are up to date. Restart running Pi agents to use them. \
             Restarted 2 waiting agents."
        );
        let report = InstallReport {
            restart_failures: 1,
            ..report
        };
        assert!(pi_result(&report).ends_with(" 1 agents could not restart; check Agents."));
    }

    #[test]
    fn setup_hints_keep_commands_as_code() {
        let (text, code) = hint_spans("Run `hermes model`, then `hermes acp --check`.");
        assert_eq!(text, "Run hermes model, then hermes acp --check.");
        assert_eq!(&text[code[0].clone()], "hermes model");
        assert_eq!(&text[code[1].clone()], "hermes acp --check");
        let (text, code) = hint_spans("A lone ` stays");
        assert_eq!(text, "A lone  stays");
        assert!(code.is_empty());
    }
}
