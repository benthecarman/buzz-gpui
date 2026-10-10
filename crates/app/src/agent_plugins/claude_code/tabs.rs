//! The type's tabs (buzz-app's `tabs.ts`): Claude, with Claude Code's
//! status on the agent's host, its setup and the agent's running sessions;
//! and Settings, what the agent runs with.

use super::runtime::{ClaudeRuntime, Config};
use super::sessions::SessionView;
use super::setup::{ClaudeSetup, ClaudeStatus, SetupState, Step};
use crate::agents2::{AgentView, Agents2};
use crate::runtime::Runtime;
use crate::ui::overlay::{self, Align};
use buzz_ui::{
    ActiveTheme, Button, Colors, Menu, MenuItem, Select, Surface, TextArea, TextField, TypeStyled,
    tokens::space,
};
use gpui_kit::component::input::{InputEvent, InputState, TextareaState};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::rc::Rc;

const MODELS: [(&str, &str); 4] = [
    ("", "Claude Code default"),
    ("opus", "Opus"),
    ("sonnet", "Sonnet"),
    ("haiku", "Haiku"),
];
const SCOPES: [(&str, &str); 2] = [
    ("thread", "One session per thread"),
    ("channel", "One session per channel"),
];
const RESPOND_TO: [(&str, &str); 2] = [("owner", "Only you"), ("anyone", "Anyone who mentions it")];

/// `buzz-field`: label, control, then the description.
fn field(
    label: &'static str,
    control: impl IntoElement,
    description: Option<SharedString>,
    c: &Colors,
) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(space::S2)
        .min_w_0()
        .child(div().text_label_sm().child(label))
        .child(control)
        .children(description.map(|text| description_text(text, c)))
}

/// `buzz-field-description`.
fn description_text(text: impl Into<SharedString>, c: &Colors) -> Div {
    div()
        .text_body_sm()
        .text_color(c.text_subtle)
        .child(text.into())
}

/// The line that says where Claude Code stands.
pub fn status_line(state: &SetupState) -> String {
    match (&state.status, state.checking) {
        (None, _) | (_, true) => "Checking Claude Code…".into(),
        (Some(ClaudeStatus::Missing), _) => "Claude Code is not installed.".into(),
        (Some(ClaudeStatus::SignedOut { version }), _) => {
            format!("Claude Code {version} needs you to sign in.")
        }
        (Some(ClaudeStatus::Ready { version, account }), _) => match account {
            Some(account) => format!("Claude Code {version} is ready ({account})."),
            None => format!("Claude Code {version} is ready."),
        },
        (Some(ClaudeStatus::Error { message }), _) => message.clone(),
    }
}

/// Claude Code on the agent's host, and what this agent has running.
pub struct ClaudeTab {
    pubkey: String,
    setup: ClaudeSetup,
    state: SetupState,
    runtime: ClaudeRuntime,
    sessions: Vec<SessionView>,
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
            code,
            _tasks: vec![follow_setup, follow_sessions],
            _subscriptions: subscriptions,
        }
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
    let code = |text: &'static str| div().text_mono().child(text);
    div()
        .flex()
        .flex_wrap()
        .text_body_sm()
        .text_color(c.text_subtle)
        .child("Runs the official installer, ")
        .child(code("curl -fsSL https://claude.ai/install.sh | bash"))
        .child(", which puts ")
        .child(code("claude"))
        .child(" in ")
        .child(code("~/.local/bin"))
        .child(".")
}

impl Render for ClaudeTab {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = *cx.colors();
        let state = &self.state;
        let status = state.status.clone();
        let running = state.running;
        let mut buttons = div().flex().flex_wrap().gap(space::S2);
        if matches!(status, Some(ClaudeStatus::Missing)) {
            buttons = buttons.child(
                Button::new("claude-code-install", "Install Claude Code")
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
        let sessions = if self.sessions.is_empty() {
            description_text(
                "None. A conversation's session starts when the agent is mentioned.",
                &c,
            )
        } else {
            div()
                .flex()
                .flex_col()
                .gap(space::S1)
                .children(self.sessions.iter().map(|session| {
                    div()
                        .flex()
                        .justify_between()
                        .gap(space::S2)
                        .child(
                            div()
                                .min_w_0()
                                .overflow_hidden()
                                .text_ellipsis()
                                .whitespace_nowrap()
                                .text_mono()
                                .child(session.key.clone()),
                        )
                        .child(div().flex_none().child(if session.busy {
                            "Working"
                        } else {
                            "Idle"
                        }))
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
                    .child(
                        div()
                            .font_weight(FontWeight::BOLD)
                            .child("Running sessions"),
                    )
                    .child(sessions),
            )
    }
}

/// Which select is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Picker {
    Model,
    Scope,
    RespondTo,
}

/// What the agent runs with. Saving restarts idle sessions with the change.
pub struct SettingsTab {
    view: AgentView,
    /// The choices made with selects; the text fields hold the rest.
    draft: Config,
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
        // The saved settings can change elsewhere; Save compares with them.
        if let Some(agents) = Agents2::global(cx) {
            subscriptions.push(cx.observe(&agents, |_, _, cx| cx.notify()));
        }
        Self {
            view,
            draft: saved,
            workspace,
            instructions,
            open: None,
            saving: false,
            error: None,
            _subscriptions: subscriptions,
        }
    }

    fn saved(&self, cx: &App) -> Config {
        let config = Agents2::global(cx)
            .and_then(|agents| {
                agents
                    .read(cx)
                    .find(&self.view.agent.pubkey)
                    .map(|agent| agent.config.clone())
            })
            .unwrap_or_else(|| self.view.agent.config.clone());
        Config::read(&config)
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

    fn choice(
        &self,
        picker: Picker,
        id: &'static str,
        value: &str,
        options: &'static [(&'static str, &'static str)],
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let open = self.open == Some(picker);
        let label = options
            .iter()
            .find(|(option, _)| *option == value)
            .map_or(options[0].1, |(_, label)| label);
        let mut select = div().relative().w_full().child(
            Select::new(id)
                .value(label)
                .open(open)
                .surface(Surface::Floating)
                .disabled(self.saving)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.open = (this.open != Some(picker)).then_some(picker);
                    cx.notify();
                })),
        );
        if open {
            let items = options.iter().map(|(option, label)| {
                MenuItem::new(SharedString::from(format!("{id}-{option}")), *label)
                    .checked(*option == value)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.open = None;
                        let option = (*option).to_string();
                        match picker {
                            Picker::Model => this.draft.model = option,
                            Picker::Scope => this.draft.scope = option,
                            Picker::RespondTo => this.draft.respond_to = option,
                        }
                        cx.notify();
                    }))
            });
            let page = cx.entity().downgrade();
            select = select.child(overlay::dropdown(
                window,
                Align::Start,
                div().min_w(rems(16.)).child(Menu::new().children(items)),
                Rc::new(move |_, cx| {
                    let _ = page.update(cx, |this, cx| {
                        this.open = None;
                        cx.notify();
                    });
                }),
            ));
        }
        select
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
        let respond_to = if self.draft.respond_to == "anyone" {
            "anyone"
        } else {
            "owner"
        };
        let answers = if respond_to == "anyone" {
            "Claude runs commands on this computer as you, with no approval step. Anyone who can mention it can ask it to."
        } else {
            "Messages from anyone else are ignored."
        };
        div()
            .flex()
            .flex_col()
            .gap(space::S4)
            .child(field(
                "Model",
                self.choice(
                    Picker::Model,
                    "claude-code-model",
                    &model,
                    &MODELS,
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
                Some("The folder Claude Code works in. Use ~/ for your home folder.".into()),
                &c,
            ))
            .child(field(
                "Sessions",
                self.choice(
                    Picker::Scope,
                    "claude-code-scope",
                    scope,
                    &SCOPES,
                    window,
                    cx,
                ),
                None,
                &c,
            ))
            .child(field(
                "Answers",
                self.choice(
                    Picker::RespondTo,
                    "claude-code-respond-to",
                    respond_to,
                    &RESPOND_TO,
                    window,
                    cx,
                ),
                Some(answers.into()),
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
                div().child(
                    Button::new("claude-code-save", "Save")
                        .prominent()
                        .loading(self.saving)
                        .disabled(!changed || self.saving)
                        .on_click(cx.listener(|this, _, _, cx| this.submit(cx))),
                ),
            )
    }
}
