//! The type's tabs (buzz-app's `tabs.ts`): Codex, with Codex's status on
//! the agent's host and the agent's conversations; and Settings, what the
//! agent runs with. Both read the account and models through a short-lived
//! `codex app-server` on that host.

use super::config::{Config, DEFAULT_WORKSPACE, Model, effort_name, valid_workspace};
use super::rpc::{AppServer, list_models};
use super::runtime::{CodexRuntime, SavedConversation, SessionView};
use super::transcript::{Place, TURN_LIMIT, read_transcript};
use crate::agent_plugins::process::Spawner;
use crate::agent_plugins::tabs::{Options, Picking, choice, description_text, field, saved_config};
use crate::agent_plugins::transcript_view::{self, Back, Loaded, Reader, Texts, TranscriptView};
use crate::agents2::{AgentView, Agents2};
use crate::buzz_mcp::app_client::Signal;
use crate::runtime::Runtime;
use crate::ui::agent_activity::panel::date_time;
use buzz_ui::{
    ActiveTheme, Button, Colors, Surface, TextArea, TextField, TypeStyled, tokens::space,
};
use gpui_kit::component::input::{InputEvent, InputState, TextareaState};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use serde_json::{Value, json};
use std::rc::Rc;
use std::sync::Arc;

/// What `account/read` says when nobody is signed in.
pub const SIGN_IN: &str =
    "Codex needs you to sign in. Run codex login in a terminal, then check again.";

/// The account Codex uses and the models it offers.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Catalog {
    pub models: Vec<Model>,
    pub account: String,
}

/// Reads the account and models with a server of its own, closed when
/// done or when `signal` aborts. Must run inside tokio.
pub async fn catalog(spawn: Spawner, signal: Signal) -> Result<Catalog, String> {
    let rpc = AppServer::new(None);
    let closer = {
        let (rpc, signal) = (rpc.clone(), signal.clone());
        tokio::spawn(async move {
            signal.wait().await;
            rpc.close().await;
        })
    };
    let result = async {
        signal.check()?;
        rpc.open(&spawn, None).await?;
        signal.check()?;
        let read = rpc.request("account/read", json!({})).await?;
        let Some(account) = read.get("account").filter(|account| account.is_object()) else {
            return Err(SIGN_IN.to_string());
        };
        let models = list_models(&rpc).await?;
        let text = |name: &str| {
            account
                .get(name)
                .and_then(Value::as_str)
                .map(str::to_string)
        };
        Ok(Catalog {
            models,
            account: text("email").or_else(|| text("type")).unwrap_or_default(),
        })
    }
    .await;
    closer.abort();
    rpc.close().await;
    result
}

/// Where the agent's processes run, and how its person signs Codex in
/// there.
#[derive(Clone)]
pub struct Host {
    pub spawn: Option<Spawner>,
    pub label: SharedString,
    pub remote: bool,
}

/// One check of Codex on the agent's host (buzz-app's `useCatalog`): a
/// tab's own, stopped when the tab goes away.
pub struct CatalogCheck {
    host: Host,
    pub data: Option<Catalog>,
    pub error: String,
    pub checking: bool,
    signal: Signal,
    _task: Option<Task<()>>,
}

impl CatalogCheck {
    pub fn new(host: Host, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            host,
            data: None,
            error: String::new(),
            checking: true,
            signal: Signal::default(),
            _task: None,
        };
        this.retry(cx);
        this
    }

    /// Checks again, abandoning a check in flight.
    pub fn retry(&mut self, cx: &mut Context<Self>) {
        self.signal.abort("Codex check replaced");
        self.signal = Signal::default();
        self.checking = true;
        self.error.clear();
        cx.notify();
        let Some(spawn) = self.host.spawn.clone() else {
            self.checking = false;
            self.data = None;
            self.error = "The agent's host is not connected".into();
            return;
        };
        let signal = self.signal.clone();
        let check = Runtime::handle(cx).spawn(catalog(spawn, signal.clone()));
        self._task = Some(cx.spawn(async move |this, cx| {
            let result = check
                .await
                .unwrap_or_else(|_| Err("Codex check stopped".into()));
            let _ = this.update(cx, |this, cx| {
                if signal.aborted() {
                    return;
                }
                match result {
                    Ok(catalog) => this.data = Some(catalog),
                    Err(error) => {
                        this.data = None;
                        this.error = error;
                    }
                }
                this.checking = false;
                cx.notify();
            });
        }));
    }

    /// The host's terminal sign-in, shown when Codex is signed out: a
    /// remote host has no browser for the usual sign-in, so it signs in
    /// with a device code instead.
    fn sign_in(&self, c: &Colors) -> Option<Div> {
        if self.error != SIGN_IN {
            return None;
        }
        let (lead, command) = if self.host.remote {
            (
                format!(
                    "Run this in a terminal on {} and follow Codex’s sign-in steps:",
                    self.host.label
                ),
                "codex login --device-auth",
            )
        } else {
            (
                "Run this in your terminal and follow Codex’s sign-in steps:".to_string(),
                "codex login",
            )
        };
        Some(
            div()
                .flex()
                .flex_col()
                .gap(space::S2)
                .text_body_sm()
                .text_color(c.text_subtle)
                .child(lead)
                .child(div().w_full().min_w_0().text_mono().child(command)),
        )
    }
}

impl Drop for CatalogCheck {
    fn drop(&mut self) {
        self.signal.abort("Codex check closed");
    }
}

/// The line that says where Codex stands.
pub fn status_line(check: &CatalogCheck) -> String {
    if check.checking {
        "Checking Codex…".into()
    } else if let Some(data) = &check.data {
        format!("Codex is ready ({}).", data.account)
    } else {
        "Codex needs attention.".into()
    }
}

const TEXTS: Texts = Texts {
    harness: "Codex",
    missing: "Codex has no history for this conversation on this computer. Its session may have been deleted.",
    turn_limit: TURN_LIMIT,
};

fn label(conversation: &SavedConversation) -> String {
    transcript_view::label(
        &conversation.channel_id,
        conversation.name.as_deref(),
        conversation.root.as_deref(),
    )
}

/// Codex on the agent's host, and this agent's conversations.
pub struct CodexTab {
    pubkey: String,
    check: Entity<CatalogCheck>,
    runtime: CodexRuntime,
    sessions: Vec<SessionView>,
    spawn: Option<Spawner>,
    /// The conversation whose transcript is shown.
    open: Option<Entity<TranscriptView>>,
    _task: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl CodexTab {
    pub fn new(pubkey: String, host: Host, runtime: CodexRuntime, cx: &mut Context<Self>) -> Self {
        let spawn = host.spawn.clone();
        let check = cx.new(|cx| CatalogCheck::new(host, cx));
        let mut changes = runtime.subscribe();
        let follow = cx.spawn(async move |this, cx| {
            while changes.changed().await.is_ok() {
                if this
                    .update(cx, |this, cx| {
                        this.sessions = this.runtime.sessions(&this.pubkey);
                        cx.notify();
                    })
                    .is_err()
                {
                    return;
                }
            }
        });
        Self {
            sessions: runtime.sessions(&pubkey),
            pubkey,
            _subscriptions: vec![cx.observe(&check, |_, _, cx| cx.notify())],
            check,
            runtime,
            spawn,
            open: None,
            _task: follow,
        }
    }

    /// Shows the conversation's transcript in place of the tab.
    fn show(&mut self, conversation: SavedConversation, cx: &mut Context<Self>) {
        let spawn = self.spawn.clone();
        let (thread, place) = (
            conversation.thread_id.clone(),
            Place {
                channel_id: conversation.channel_id.clone(),
                root: conversation.root.clone(),
            },
        );
        let reader: Reader = Arc::new(move |_, signal| {
            let (spawn, thread, place) = (spawn.clone(), thread.clone(), place.clone());
            Box::pin(async move {
                let Some(spawn) = spawn else {
                    return Err("The agent's host is not connected".into());
                };
                let read = read_transcript(&spawn, &thread, &place, &signal).await?;
                Ok(read.map(|read| Loaded {
                    transcript: read.transcript,
                    more: read.more,
                    working: Some(read.working),
                }))
            })
        });
        let weak = cx.entity().downgrade();
        let back: Back = Rc::new(move |_, cx| {
            let _ = weak.update(cx, |this, cx| {
                this.open = None;
                cx.notify();
            });
        });
        let (pubkey, title) = (self.pubkey.clone(), label(&conversation).into());
        self.open =
            Some(cx.new(|cx| TranscriptView::new(pubkey, title, TEXTS, reader, false, back, cx)));
        cx.notify();
    }
}

/// A conversation's status detail, scrolled when long.
fn detail(key: &str, text: &str) -> Stateful<Div> {
    div()
        .id(SharedString::from(format!("codex-detail-{key}")))
        .max_h(rems(12.))
        .overflow_y_scroll()
        .text_caption()
        .text_mono()
        .whitespace_normal()
        .child(text.to_string())
}

impl Render for CodexTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(view) = &self.open {
            return div().child(view.clone());
        }
        let c = *cx.colors();
        // Read on each render: bindings change as turns start, which also
        // changes the session list this tab follows.
        let saved = self.runtime.conversations(&self.pubkey);
        let check = self.check.read(cx);
        let checking = check.checking;
        // Work that failed before Codex started a thread has no binding.
        let unbound: Vec<&SessionView> = self
            .sessions
            .iter()
            .filter(|view| !saved.iter().any(|row| row.key == view.key))
            .collect();
        let conversations = if saved.is_empty() && unbound.is_empty() {
            description_text(
                "Mention this agent in a channel to start a conversation.",
                &c,
            )
        } else {
            let unbound = unbound.into_iter().map(|session| {
                div()
                    .flex()
                    .flex_col()
                    .gap(space::S1)
                    .min_w_0()
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .child(div().text_mono().child(session.key.clone()))
                            .child(format!(" · {}", session.status)),
                    )
                    .when(!session.detail.is_empty(), |this| {
                        this.child(detail(&session.key, &session.detail))
                    })
            });
            let bound = saved.into_iter().map(|row| {
                let view = self.sessions.iter().find(|view| view.key == row.key);
                let line: String = std::iter::once(label(&row))
                    .chain(view.map(|view| view.status.clone()))
                    .chain(row.at.map(date_time))
                    .collect::<Vec<_>>()
                    .join(" · ");
                let id = SharedString::from(format!("codex-transcript-{}", row.key));
                div()
                    .flex()
                    .flex_col()
                    .gap(space::S2)
                    .min_w_0()
                    .child(div().child(line))
                    .when_some(view.filter(|view| !view.detail.is_empty()), |this, view| {
                        this.child(detail(&view.key, &view.detail))
                    })
                    .child(div().flex().child(
                        Button::new(id, "View transcript").ghost().on_click(
                            cx.listener(move |this, _, _, cx| this.show(row.clone(), cx)),
                        ),
                    ))
            });
            div()
                .flex()
                .flex_col()
                .gap(space::S4)
                .children(unbound.map(IntoElement::into_any_element))
                .children(bound.map(IntoElement::into_any_element))
        };
        div()
            .flex()
            .flex_col()
            .gap(space::S4)
            .text_body()
            .child(div().child(status_line(check)))
            .when(!check.error.is_empty(), |this| {
                this.child(div().child(check.error.clone()))
            })
            .children(check.sign_in(&c))
            .child(
                div().flex().child(
                    Button::new("codex-check", "Check again")
                        .disabled(checking)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.check.update(cx, |check, cx| check.retry(cx));
                        })),
                ),
            )
            .child(description_text(
                "Uses your existing Codex sign-in. Install Codex and run codex login in a terminal if needed.",
                &c,
            ))
            .child(
                div()
                    .font_weight(FontWeight::BOLD)
                    .child("Conversations"),
            )
            .child(conversations)
            .child(description_text(
                "Only your messages trigger this agent. New mentions steer active work and continue the same session when idle. Event watches work for your messages; timers are not supported yet.",
                &c,
            ))
    }
}

/// Which select is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Picker {
    Model,
    Effort,
    Scope,
}

/// The Model select: Codex's default, a saved model Codex no longer
/// offers, then every model.
pub fn model_options(draft: &Config, models: Option<&[Model]>) -> Options {
    let models = models.unwrap_or_default();
    let selected = selected_model(draft, models);
    let default = match selected {
        Some(model) if draft.model.is_empty() => format!("Codex default · {}", model.display_name),
        _ => "Codex default".into(),
    };
    let mut options = vec![(String::new(), default)];
    if !draft.model.is_empty() && !models.iter().any(|model| model.model == draft.model) {
        options.push((
            draft.model.clone(),
            format!("{} (unavailable)", draft.model),
        ));
    }
    options.extend(
        models
            .iter()
            .map(|model| (model.model.clone(), model.display_name.clone())),
    );
    options
}

/// The Thinking select for the selected model.
pub fn effort_options(draft: &Config, selected: Option<&Model>) -> Options {
    let efforts = selected.map_or(&[][..], |model| &model.supported_reasoning_efforts);
    let mut options = vec![(String::new(), "Model default".to_string())];
    if !draft.effort.is_empty()
        && !efforts
            .iter()
            .any(|effort| effort.reasoning_effort == draft.effort)
    {
        options.push((
            draft.effort.clone(),
            format!("{} (unavailable)", effort_name(&draft.effort)),
        ));
    }
    options.extend(efforts.iter().map(|effort| {
        (
            effort.reasoning_effort.clone(),
            effort_name(&effort.reasoning_effort),
        )
    }));
    options
}

/// The model the draft runs: its own, or Codex's default.
pub fn selected_model<'a>(draft: &Config, models: &'a [Model]) -> Option<&'a Model> {
    models
        .iter()
        .find(|model| model.model == draft.model)
        .or_else(|| {
            draft
                .model
                .is_empty()
                .then(|| models.iter().find(|model| model.is_default))
                .flatten()
        })
}

/// Checks a draft before it is saved.
pub fn validate(draft: &Config) -> Result<Config, String> {
    let settings = Config::read(&draft.value());
    if !valid_workspace(&settings.workspace) {
        return Err("Enter an absolute workspace path or ~/.buzz.".into());
    }
    Ok(settings)
}

/// What the agent runs with.
pub struct SettingsTab {
    view: AgentView,
    check: Entity<CatalogCheck>,
    /// The choices made with selects; the text fields hold the rest.
    draft: Config,
    /// The saved settings the draft started from.
    saved: Config,
    workspace: Entity<InputState>,
    instructions: Entity<TextareaState>,
    open: Option<Picker>,
    saving: bool,
    error: Option<String>,
    _subscriptions: Vec<Subscription>,
}

impl SettingsTab {
    pub fn new(view: AgentView, host: Host, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let saved = Config::read(&view.agent.config);
        let check = cx.new(|cx| CatalogCheck::new(host, cx));
        let workspace = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(DEFAULT_WORKSPACE)
                .default_value(saved.workspace.clone())
        });
        let instructions = cx.new(|cx| TextareaState::new(window, cx));
        instructions.update(cx, |state, cx| {
            state.set_value(saved.instructions.clone(), window, cx)
        });
        let mut subscriptions = vec![
            cx.observe(&check, |_, _, cx| cx.notify()),
            cx.subscribe(&workspace, |_, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            }),
            cx.subscribe(&instructions, |_, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            }),
        ];
        // Saved settings that change, here or elsewhere, replace the draft.
        if let Some(agents) = Agents2::global(cx) {
            subscriptions.push(cx.observe_in(&agents, window, |this, _, window, cx| {
                let saved = this.saved(cx);
                if saved != this.saved {
                    this.reset(saved, window, cx);
                }
            }));
        }
        Self {
            view,
            check,
            draft: saved.clone(),
            saved,
            workspace,
            instructions,
            open: None,
            saving: false,
            error: None,
            _subscriptions: subscriptions,
        }
    }

    fn saved(&self, cx: &App) -> Config {
        Config::read(&saved_config(&self.view, cx))
    }

    fn reset(&mut self, saved: Config, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace.update(cx, |state, cx| {
            state.set_value(saved.workspace.clone(), window, cx)
        });
        self.instructions.update(cx, |state, cx| {
            state.set_value(saved.instructions.clone(), window, cx)
        });
        self.draft = saved.clone();
        self.saved = saved;
        cx.notify();
    }

    fn current(&self, cx: &App) -> Config {
        Config {
            workspace: self.workspace.read(cx).value().to_string(),
            instructions: self.instructions.read(cx).value().to_string(),
            ..self.draft.clone()
        }
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        self.error = None;
        let settings = match validate(&self.current(cx)) {
            Ok(settings) => settings,
            Err(error) => {
                self.error = Some(error);
                cx.notify();
                return;
            }
        };
        self.saving = true;
        let save = self.view.save(settings.value(), cx);
        cx.spawn(async move |this, cx| {
            let result = save.await;
            let _ = this.update(cx, |this, cx| {
                this.saving = false;
                this.error = result.err();
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}

impl Picking for SettingsTab {
    type Picker = Picker;

    fn open(&mut self) -> &mut Option<Picker> {
        &mut self.open
    }

    fn pick(&mut self, picker: Picker, value: String, cx: &mut Context<Self>) {
        match picker {
            Picker::Model => {
                let effort = self
                    .check
                    .read(cx)
                    .data
                    .as_ref()
                    .and_then(|data| data.models.iter().find(|model| model.model == value))
                    .map(|model| model.default_reasoning_effort.clone())
                    .unwrap_or_default();
                self.draft.model = value;
                self.draft.effort = effort;
            }
            Picker::Effort => self.draft.effort = value,
            Picker::Scope => self.draft.scope = value,
        }
    }
}

impl Render for SettingsTab {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = *cx.colors();
        let current = self.current(cx);
        let changed = current != self.saved(cx);
        let check = self.check.read(cx);
        let (checking, catalog_error) = (check.checking, check.error.clone());
        let sign_in = check.sign_in(&c);
        let models = check.data.as_ref().map(|data| data.models.clone());
        let selected = models
            .as_deref()
            .and_then(|models| selected_model(&self.draft, models))
            .cloned();
        let model_choices = model_options(&self.draft, models.as_deref());
        let effort_choices = effort_options(&self.draft, selected.as_ref());
        let scope = if self.draft.scope == "channel" {
            "channel"
        } else {
            "thread"
        };
        let scopes = vec![
            ("thread".to_string(), "One session per thread".to_string()),
            ("channel".to_string(), "One session per channel".to_string()),
        ];
        let saving = self.saving;
        div()
            .flex()
            .flex_col()
            .gap(space::S4)
            .when(!catalog_error.is_empty(), |this| {
                this.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(space::S2)
                        .child(catalog_error)
                        .children(sign_in)
                        .child(
                            div().flex().child(
                                Button::new("codex-settings-check", "Check again")
                                    .disabled(checking)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.check.update(cx, |check, cx| check.retry(cx));
                                    })),
                            ),
                        ),
                )
            })
            .child(field(
                "Model",
                choice(
                    self.open,
                    Picker::Model,
                    "codex-model",
                    &self.draft.model,
                    model_choices,
                    checking || saving,
                    window,
                    cx,
                ),
                selected
                    .as_ref()
                    .map(|model| model.description.clone())
                    .filter(|description| !description.is_empty())
                    .map(SharedString::from),
                &c,
            ))
            .child(field(
                "Thinking",
                choice(
                    self.open,
                    Picker::Effort,
                    "codex-effort",
                    &self.draft.effort,
                    effort_choices,
                    checking || saving || selected.is_none(),
                    window,
                    cx,
                ),
                None,
                &c,
            ))
            .child(field(
                "Workspace",
                TextField::new(&self.workspace)
                    .surface(Surface::Floating)
                    .disabled(saving),
                Some(
                    "Codex starts here with full access to your files and network. Buzz tools run inside the app. Inherited MCP/Apps/plugin tools are disabled."
                        .into(),
                ),
                &c,
            ))
            .child(field(
                "Sessions",
                choice(
                    self.open,
                    Picker::Scope,
                    "codex-scope",
                    scope,
                    scopes,
                    saving,
                    window,
                    cx,
                ),
                Some("DMs always share one conversation.".into()),
                &c,
            ))
            .child(field(
                "Instructions",
                TextArea::new(&self.instructions)
                    .rows(6)
                    .surface(Surface::Floating)
                    .disabled(saving),
                Some(
                    "Buzz guidance supplements Codex’s native instructions. Saved changes apply to the next turn. A workspace change starts a fresh session."
                        .into(),
                ),
                &c,
            ))
            .children(
                self.error
                    .clone()
                    .map(|error| div().text_body_sm().text_color(c.text_danger).child(error)),
            )
            .child(
                div().flex().child(
                    Button::new("codex-save", if saving { "Saving…" } else { "Save" })
                        .prominent()
                        .disabled(saving || !changed)
                        .on_click(cx.listener(|this, _, _, cx| this.submit(cx))),
                ),
            )
    }
}

#[cfg(test)]
mod tests;
