//! The type's tabs (buzz-app's `tabs.ts`): Claude, with Claude's
//! status on the agent's host, its setup and the agent's conversations,
//! each with its session's transcript; and Settings, what the agent runs
//! with.

use super::runtime::{ClaudeRuntime, Config, SavedConversation};
use super::sessions::SessionView;
use super::setup::{ClaudeSetup, ClaudeStatus, SetupState, Step};
use super::transcript::{TURN_LIMIT, read_transcript};
use crate::agent_plugins::tabs::{Picking, choice, description_text, field, options, saved_config};
use crate::agent_plugins::transcript_view::{self, Back, Loaded, Reader, Texts, TranscriptView};
use crate::agents2::{AgentView, Agents2};
use crate::runtime::Runtime;
use crate::ui::agent_activity::panel::date_time;
use buzz_ui::{
    ActiveTheme, Button, Colors, Surface, TextArea, TextField, TypeStyled,
    tokens::{font, space},
};
use gpui_kit::component::input::{InputEvent, InputState, TextareaState};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::rc::Rc;
use std::sync::Arc;

const MODELS: [(&str, &str); 4] = [
    ("", "Claude default"),
    ("opus", "Opus"),
    ("sonnet", "Sonnet"),
    ("haiku", "Haiku"),
];
const SCOPES: [(&str, &str); 2] = [
    ("thread", "One session per thread"),
    ("channel", "One session per channel"),
];

/// The line that says where Claude stands.
pub fn status_line(state: &SetupState) -> String {
    match (&state.status, state.checking) {
        (None, _) | (_, true) => "Checking Claude…".into(),
        (Some(ClaudeStatus::Missing), _) => "Claude is not installed.".into(),
        (Some(ClaudeStatus::SignedOut { version }), _) => {
            format!("Claude {version} needs you to sign in.")
        }
        (Some(ClaudeStatus::Ready { version, account }), _) => match account {
            Some(account) => format!("Claude {version} is ready ({account})."),
            None => format!("Claude {version} is ready."),
        },
        (Some(ClaudeStatus::Error { message }), _) => message.clone(),
    }
}

const TEXTS: Texts = Texts {
    harness: "Claude",
    missing: "Claude has no transcript for this conversation on this computer. It may have been deleted, or the session ran in a different workspace.",
    turn_limit: TURN_LIMIT,
};

fn label(conversation: &SavedConversation) -> String {
    transcript_view::label(
        &conversation.channel_id,
        conversation.name.as_deref(),
        conversation.root.as_deref(),
    )
}

/// Claude on the agent's host, and this agent's conversations.
pub struct ClaudeTab {
    pubkey: String,
    setup: ClaudeSetup,
    state: SetupState,
    runtime: ClaudeRuntime,
    sessions: Vec<SessionView>,
    /// The conversation whose transcript is shown, by key.
    open: Option<(String, Entity<TranscriptView>)>,
    code: Entity<InputState>,
    _tasks: Vec<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl ClaudeTab {
    pub fn new(
        pubkey: String,
        setup: ClaudeSetup,
        runtime: ClaudeRuntime,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let code = cx.new(|cx| InputState::new(window, cx));
        let mut setup_changes = setup.subscribe();
        let follow_setup = cx.spawn(async move |this, cx| {
            while setup_changes.changed().await.is_ok() {
                let state = setup_changes.borrow_and_update().clone();
                if this
                    .update(cx, |this, cx| {
                        this.state = state;
                        cx.notify();
                    })
                    .is_err()
                {
                    return;
                }
            }
        });
        let mut runtime_changes = runtime.subscribe();
        let follow_sessions = cx.spawn(async move |this, cx| {
            while runtime_changes.changed().await.is_ok() {
                if this
                    .update(cx, |this, cx| {
                        this.sessions = this.runtime.sessions(&this.pubkey);
                        if let Some((key, view)) = &this.open {
                            let working = this.busy(key).unwrap_or(false);
                            view.update(cx, |view, cx| view.set_working(working, cx));
                        }
                        cx.notify();
                    })
                    .is_err()
                {
                    return;
                }
            }
        });
        let subscriptions = vec![cx.subscribe(&code, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        })];
        let check = setup.clone();
        Runtime::handle(cx).spawn(async move { check.check().await });
        Self {
            sessions: runtime.sessions(&pubkey),
            state: setup.snapshot(),
            pubkey,
            setup,
            runtime,
            open: None,
            code,
            _tasks: vec![follow_setup, follow_sessions],
            _subscriptions: subscriptions,
        }
    }

    /// Whether the conversation's session is working; `None` when none
    /// is running.
    fn busy(&self, key: &str) -> Option<bool> {
        self.sessions
            .iter()
            .find(|session| session.key == key)
            .map(|session| session.busy)
    }

    /// Shows the conversation's transcript in place of the tab.
    fn show(&mut self, conversation: SavedConversation, cx: &mut Context<Self>) {
        let agents = Agents2::global(cx);
        let spawn = agents.as_ref().and_then(|agents| {
            let host = agents.read(cx).host_of(&self.pubkey)?;
            Some(host.read(cx).spawner(super::PROCESS_OWNER))
        });
        let workspace = agents
            .and_then(|agents| {
                let agent = agents.read(cx).find(&self.pubkey)?;
                Some(Config::read(&agent.config).workspace)
            })
            .unwrap_or_else(|| Config::default().workspace);
        let session = conversation.conversation();
        let reader: Reader = Arc::new(move |working, signal| {
            let (spawn, workspace, session) = (spawn.clone(), workspace.clone(), session.clone());
            Box::pin(async move {
                let Some(spawn) = spawn else {
                    return Err("The agent's host is not connected".into());
                };
                let read = read_transcript(&spawn, &workspace, &session, working, &signal).await?;
                Ok(read.map(|read| Loaded {
                    transcript: read.transcript,
                    more: read.more,
                    working: None,
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
        let working = self.busy(&conversation.key).unwrap_or(false);
        let pubkey = self.pubkey.clone();
        let title = label(&conversation).into();
        let view =
            cx.new(|cx| TranscriptView::new(pubkey, title, TEXTS, reader, working, back, cx));
        self.open = Some((conversation.key, view));
        cx.notify();
    }

    fn run(&self, step: Step, cx: &mut Context<Self>) {
        let setup = self.setup.clone();
        Runtime::handle(cx).spawn(async move { setup.run(step).await });
    }

    fn send_code(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let code = self.code.read(cx).value().to_string();
        if code.trim().is_empty() {
            return;
        }
        let setup = self.setup.clone();
        Runtime::handle(cx).spawn(async move { setup.answer(&code).await });
        self.code
            .update(cx, |state, cx| state.set_value("", window, cx));
    }
}

/// `curl … | bash`, `claude` and `~/.local/bin` in code type.
fn installer_note(c: &Colors) -> Div {
    // Words wrap one at a time; punctuation stays with the word before it.
    // `true` marks code.
    const WORDS: &[&[(&str, bool)]] = &[
        &[("Runs", false)],
        &[("the", false)],
        &[("official", false)],
        &[("installer,", false)],
        &[("curl", true)],
        &[("-fsSL", true)],
        &[("https://claude.ai/install.sh", true)],
        &[("|", true)],
        &[("bash", true), (",", false)],
        &[("which", false)],
        &[("puts", false)],
        &[("claude", true)],
        &[("in", false)],
        &[("~/.local/bin", true), (".", false)],
    ];
    div()
        .flex()
        .flex_wrap()
        .gap_x(rems(0.25))
        .text_body_sm()
        .text_color(c.text_subtle)
        .children(WORDS.iter().map(|word| {
            div().flex().children(word.iter().map(|(text, code)| {
                div()
                    .when(*code, |this| this.font_family(font::MONO))
                    .child(*text)
            }))
        }))
}

impl Render for ClaudeTab {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some((_, view)) = &self.open {
            return div().child(view.clone());
        }
        let c = *cx.colors();
        let state = &self.state;
        let status = state.status.clone();
        let running = state.running;
        let mut buttons = div().flex().flex_wrap().gap(space::S2);
        if matches!(status, Some(ClaudeStatus::Missing)) {
            buttons = buttons.child(
                Button::new("claude-code-install", "Install Claude")
                    .prominent()
                    .loading(running == Some(Step::Install))
                    .disabled(running.is_some())
                    .on_click(cx.listener(|this, _, _, cx| this.run(Step::Install, cx))),
            );
        }
        if matches!(status, Some(ClaudeStatus::SignedOut { .. })) {
            buttons = buttons.child(
                Button::new("claude-code-sign-in", "Sign in")
                    .prominent()
                    .loading(running == Some(Step::Login))
                    .disabled(running.is_some())
                    .on_click(cx.listener(|this, _, _, cx| this.run(Step::Login, cx))),
            );
        }
        buttons = buttons.child(if running.is_some() {
            Button::new("claude-code-cancel", "Cancel")
                .ghost()
                .on_click(cx.listener(|this, _, _, cx| {
                    let setup = this.setup.clone();
                    Runtime::handle(cx).spawn(async move { setup.cancel().await });
                }))
        } else {
            Button::new("claude-code-check", "Check again")
                .ghost()
                .disabled(state.checking)
                .on_click(cx.listener(|this, _, _, cx| {
                    let setup = this.setup.clone();
                    Runtime::handle(cx).spawn(async move { setup.check().await });
                }))
        });
        let has_code = !self.code.read(cx).value().trim().is_empty();
        let setup = div()
            .flex()
            .flex_col()
            .gap(space::S2)
            .child(div().text_body().child(status_line(state)))
            .child(buttons)
            .when(matches!(status, Some(ClaudeStatus::Missing)), |this| {
                this.child(installer_note(&c))
            })
            .when(!state.output.is_empty(), |this| {
                this.child(
                    div()
                        .id("claude-code-setup-output")
                        .max_h(rems(15.))
                        .overflow_y_scroll()
                        .text_caption()
                        .text_mono()
                        .whitespace_normal()
                        .child(state.output.clone()),
                )
            })
            .when(running == Some(Step::Login), |this| {
                this.child(
                    div()
                        .flex()
                        .items_end()
                        .gap(space::S2)
                        .child(
                            field(
                                "Sign-in code",
                                TextField::new(&self.code).surface(Surface::Floating),
                                Some("If the browser shows a code to paste, enter it here.".into()),
                                &c,
                            )
                            .flex_1(),
                        )
                        .child(
                            Button::new("claude-code-send-code", "Send")
                                .disabled(!has_code)
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.send_code(window, cx)),
                                ),
                        ),
                )
            });
        // Read on each render: bindings change as turns start, which also
        // changes the session list this tab follows.
        let conversations = self.runtime.conversations(&self.pubkey);
        let list = if conversations.is_empty() {
            description_text(
                "None. A conversation's session starts when the agent is mentioned.",
                &c,
            )
        } else {
            div()
                .flex()
                .flex_col()
                .gap(space::S2)
                .children(conversations.into_iter().map(|conversation| {
                    let activity = self
                        .busy(&conversation.key)
                        .map(|busy| if busy { "Working" } else { "Idle" }.to_string());
                    let detail: Vec<String> = activity
                        .into_iter()
                        .chain([date_time(conversation.at)])
                        .collect();
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap(space::S2)
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .min_w_0()
                                .child(div().truncate().child(label(&conversation)))
                                .child(
                                    div()
                                        .text_body_sm()
                                        .text_color(c.text_subtle)
                                        .child(detail.join(" · ")),
                                ),
                        )
                        .child(
                            Button::new(
                                SharedString::from(format!(
                                    "claude-transcript-{}",
                                    conversation.key
                                )),
                                "View transcript",
                            )
                            .ghost()
                            .on_click(cx.listener(
                                move |this, _, _, cx| this.show(conversation.clone(), cx),
                            )),
                        )
                }))
        };
        let _ = window;
        div()
            .flex()
            .flex_col()
            .gap(space::S4)
            .text_body()
            .child(setup)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(space::S1)
                    .child(div().font_weight(FontWeight::BOLD).child("Conversations"))
                    .child(list),
            )
    }
}

/// Which select is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Picker {
    Model,
    Scope,
}

/// What the agent runs with. Saving restarts idle sessions with the change.
pub struct SettingsTab {
    view: AgentView,
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
    pub fn new(view: AgentView, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let saved = Config::read(&view.agent.config);
        let workspace =
            cx.new(|cx| InputState::new(window, cx).default_value(saved.workspace.clone()));
        let instructions = cx.new(|cx| TextareaState::new(window, cx));
        instructions.update(cx, |state, cx| {
            state.set_value(saved.instructions.clone(), window, cx)
        });
        let mut subscriptions = vec![
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
        let mut config = self.current(cx);
        if config.workspace.trim().is_empty() {
            config.workspace = "~/.buzz".into();
        } else {
            config.workspace = config.workspace.trim().to_string();
        }
        self.saving = true;
        self.error = None;
        let save = self.view.save(config.value(), cx);
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

    fn pick(&mut self, picker: Picker, value: String, _: &mut Context<Self>) {
        match picker {
            Picker::Model => self.draft.model = value,
            Picker::Scope => self.draft.scope = value,
        }
    }
}

impl Render for SettingsTab {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = *cx.colors();
        let current = self.current(cx);
        let changed = current != self.saved(cx);
        // A model not offered here shows as the default.
        let model = if MODELS.iter().any(|(value, _)| *value == self.draft.model) {
            self.draft.model.clone()
        } else {
            String::new()
        };
        let scope = if self.draft.scope == "channel" {
            "channel"
        } else {
            "thread"
        };
        div()
            .flex()
            .flex_col()
            .gap(space::S4)
            .child(field(
                "Model",
                choice(
                    self.open,
                    Picker::Model,
                    "claude-code-model",
                    &model,
                    options(&MODELS),
                    self.saving,
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
                    .disabled(self.saving),
                Some("The folder Claude works in. Use ~/ for your home folder.".into()),
                &c,
            ))
            .child(field(
                "Sessions",
                choice(
                    self.open,
                    Picker::Scope,
                    "claude-code-scope",
                    scope,
                    options(&SCOPES),
                    self.saving,
                    window,
                    cx,
                ),
                None,
                &c,
            ))
            .child(field(
                "Instructions",
                TextArea::new(&self.instructions)
                    .rows(6)
                    .surface(Surface::Floating)
                    .disabled(self.saving),
                Some("Added to every session's system prompt, after Buzz's own.".into()),
                &c,
            ))
            .children(
                self.error
                    .clone()
                    .map(|error| div().text_body_sm().text_color(c.text_danger).child(error)),
            )
            .child(
                div().flex().child(
                    Button::new("claude-code-save", "Save")
                        .prominent()
                        .loading(self.saving)
                        .disabled(!changed || self.saving)
                        .on_click(cx.listener(|this, _, _, cx| this.submit(cx))),
                ),
            )
    }
}
