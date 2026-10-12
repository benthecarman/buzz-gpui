//! "Share {name}" (buzz-app `SnapshotShareDialog.tsx`, agent part of
//! `DirectShare.tsx`): send a copy of an agent or team in a direct
//! message, copy a link to it, or export it.
//!
//! The dialog is generic over what it shares ([`ShareSource`]): this module
//! wires agents ([`open_agent`]); teams build their own source and call
//! [`open`]. A send journals its intent in the session's outbox, so one
//! interrupted by a restart comes back as "Retry send".

use super::editor::session_policy;
use super::inventory::in_community;
use super::snapshots::{
    self, LEVELS, LayerDialog, WindowAction, alert, checked, inline_select, level_label, on_tokio,
    paragraph, pre, read_memories, summary,
};
use crate::community::{AppState, Community};
use crate::hosts::{HostConnection, Hosts};
use crate::snapshot::Kind;
use crate::snapshot::manifest::{self, MemoryLevel, build_agent_snapshot};
use crate::snapshot::png::{self, Format};
use crate::snapshot::send::{self, Phase, Receipt};
use crate::snapshot::share::{self, EncodedSnapshot, Recipient};
use crate::ui::notice;
use crate::ui::overlay::{self, Dismiss};
use buzz_host::api::Agent;
use buzz_relay_client::{Delivery, Outbox, PeoplePage, Recovery, Session};
use buzz_ui::{
    ActiveTheme, AlertDialog, Avatar, AvatarSize, Button, Dialog, Icon, IconName, SearchField,
    Surface, TextRole, TypeStyled,
};
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

/// The portable configuration to review, or why there is none.
pub type Review = Rc<dyn Fn(&App) -> Result<String, String>>;

/// Encodes the shared file at a memory level.
pub type Encode = Rc<dyn Fn(MemoryLevel, &mut App) -> Task<Result<EncodedSnapshot, String>>>;

/// What a share dialog shares; its owner keeps encoding, export and
/// catalog policy.
pub struct ShareSource {
    pub kind: Kind,
    pub display_name: SharedString,
    /// Stable across restarts: it keys the send's journal entry.
    pub source_id: String,
    pub has_memory_options: bool,
    /// People who may not receive it (the agent itself, team members).
    pub excluded: Vec<String>,
    pub encode: Encode,
    /// "Export {kind}".
    pub on_export: WindowAction,
    /// The portable configuration to review before sharing, or why it
    /// cannot be shared. Agents have one; teams do not.
    pub review: Option<Review>,
    /// The "Share to catalog" controls, from the community catalog.
    pub catalog: Option<AnyView>,
}

/// Builds an agent's "Share to catalog" controls. The community catalog
/// registers one with [`set_agent_catalog_controls`]; until then the
/// dialog has no catalog section.
pub type AgentCatalogControls =
    Rc<dyn Fn(&Entity<HostConnection>, &Agent, &mut Window, &mut App) -> AnyView>;

#[derive(Default)]
struct Hooks {
    agent_catalog: Option<AgentCatalogControls>,
}

impl Global for Hooks {}

/// Hook for the community catalog: the controls the agent share dialog
/// shows under "Share settings".
pub(crate) fn set_agent_catalog_controls(controls: AgentCatalogControls, cx: &mut App) {
    cx.default_global::<Hooks>().agent_catalog = Some(controls);
}

/// Opens "Share {name}" for an agent, or straight to its export without a
/// community to send in (`AgentDirectShare`).
pub(crate) fn open_agent(
    host: Entity<HostConnection>,
    id: String,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(agent) = host.read(cx).agent(&id).cloned() else {
        return;
    };
    let Some(community) = AppState::current(cx) else {
        snapshots::open_export(host, id, cx);
        return;
    };
    host.update(cx, |host, cx| host.load_defaults(cx));
    let same_community = in_community(&agent.relay_url, &community.read(cx).origin);
    let review = {
        let (host, id) = (host.clone(), id.clone());
        Rc::new(move |cx: &App| {
            let agent = host
                .read(cx)
                .agent(&id)
                .cloned()
                .ok_or("This agent no longer exists.")?;
            snapshots::review(&agent, &host, cx).map(|review| review.pretty())
        }) as Review
    };
    let encode: Encode = {
        let (host, id) = (host.clone(), id.clone());
        Rc::new(move |level, cx: &mut App| {
            let Some(agent) = host.read(cx).agent(&id).cloned() else {
                return Task::ready(Err("This agent no longer exists.".into()));
            };
            let policy = session_policy(&host, cx);
            if level != MemoryLevel::None && !same_community {
                return Task::ready(Err(
                    "Memory reads require the source agent's community.".into()
                ));
            }
            let memories = read_memories(&agent, level, cx);
            cx.background_spawn(async move {
                let memories = memories.await?;
                let snapshot = build_agent_snapshot(&agent, level, &memories, policy)?;
                Ok(EncodedSnapshot {
                    bytes: png::encode(&snapshot, Format::Png, None)?,
                    file_name: manifest::file_name(&agent.name, "png"),
                })
            })
        })
    };
    let on_export = {
        let (host, id) = (host.clone(), id.clone());
        Rc::new(move |_: &mut Window, cx: &mut App| {
            snapshots::open_export(host.clone(), id.clone(), cx)
        })
    };
    let catalog = cx
        .try_global::<Hooks>()
        .and_then(|hooks| hooks.agent_catalog.clone())
        .map(|controls| controls(&host, &agent, window, cx));
    let source = ShareSource {
        kind: Kind::Agent,
        display_name: agent.name.clone().into(),
        source_id: agent.id.clone(),
        has_memory_options: same_community,
        excluded: vec![agent.pubkey.clone()],
        encode,
        on_export,
        review: Some(review),
        catalog,
    };
    open(community, source, window, cx);
}

/// Opens the share dialog for `source` in `community`.
pub(crate) fn open(
    community: Entity<Community>,
    source: ShareSource,
    window: &mut Window,
    cx: &mut App,
) {
    let dialog = cx.new(|cx| ShareDialog::new(community, source, window, cx));
    snapshots::show(dialog, cx);
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    Send,
    Copy,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Copy {
    Idle,
    Copying,
    Copied,
}

/// Other people a DM can hold (`direct-message-policy.ts`).
const MAX_RECIPIENTS: usize = 8;
const DEBOUNCE: Duration = Duration::from_millis(150);

pub struct ShareDialog {
    session: Arc<Session>,
    source: ShareSource,
    recovery_key: Result<String, String>,
    outbox: Option<Outbox>,
    ready: bool,
    recipients: Vec<Recipient>,
    input: Entity<InputState>,
    picker_open: bool,
    found: Option<(String, Result<PeoplePage, String>)>,
    searching: Option<Task<()>>,
    level: MemoryLevel,
    level_open: bool,
    phase: Phase,
    receipt: Option<Receipt>,
    copy: Copy,
    pending: bool,
    error: Option<SharedString>,
    confirmation: Option<(Action, MemoryLevel)>,
    reviewed: bool,
    review_open: bool,
    /// The review the approval covers; a change asks again.
    reviewed_json: Option<String>,
    encoded: HashMap<MemoryLevel, EncodedSnapshot>,
    operation: Option<Task<()>>,
    _watch: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl LayerDialog for ShareDialog {
    fn closable(&self) -> bool {
        !self.busy()
    }
}

impl ShareDialog {
    fn new(
        community: Entity<Community>,
        source: ShareSource,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let session = community.read(cx).session.clone();
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Find a person…"));
        let subscriptions =
            vec![
                cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::Change) {
                        this.picker_open = true;
                        this.search(window, cx);
                    }
                }),
            ];
        let recovery_key = share::recovery_key(source.kind, &source.source_id);
        let mut dialog = Self {
            session,
            source,
            recovery_key,
            outbox: None,
            ready: false,
            recipients: Vec::new(),
            input,
            picker_open: false,
            found: None,
            searching: None,
            level: MemoryLevel::None,
            level_open: false,
            phase: Phase::Idle,
            receipt: None,
            copy: Copy::Idle,
            pending: false,
            error: None,
            confirmation: None,
            reviewed: false,
            review_open: false,
            reviewed_json: None,
            encoded: HashMap::new(),
            operation: None,
            _watch: None,
            _subscriptions: subscriptions,
        };
        dialog.recover(cx);
        dialog
    }

    fn busy(&self) -> bool {
        self.phase.sending() || self.copy == Copy::Copying || self.pending
    }

    fn delivery(&self) -> Option<Delivery> {
        let (outbox, receipt) = (self.outbox.as_ref()?, self.receipt.as_ref()?);
        send::delivery(outbox, receipt)
    }

    fn failed(&self) -> bool {
        self.receipt.is_some() && self.delivery() == Some(Delivery::Failed)
    }

    /// Recipients and level stay put while an earlier send is unconfirmed.
    fn locked(&self) -> bool {
        !self.ready || self.busy() || (self.receipt.is_some() && !self.failed())
    }

    fn actions_disabled(&self, cx: &App) -> bool {
        match &self.source.review {
            Some(review) => !self.reviewed || review(cx).is_err(),
            None => false,
        }
    }

    /// Picks up a send journaled before (`recoverSnapshot`), confirming it
    /// when it was delivered meanwhile.
    fn recover(&mut self, cx: &mut Context<Self>) {
        let session = self.session.clone();
        let key = self.recovery_key.clone();
        self._watch = Some(cx.spawn(async move |this, cx| {
            let outbox = match session.outbox().await {
                Some(outbox) => outbox,
                None => {
                    let _ = this.update(cx, |this, cx| {
                        this.error = Some("Sharing recovery is unavailable.".into());
                        cx.notify();
                    });
                    return;
                }
            };
            // The outbox journals on tokio.
            let journal = outbox.clone();
            let found = cx
                .update(|cx| {
                    on_tokio(cx, async move {
                        journal.ready().await.map_err(|error| error.to_string())?;
                        let Some(saved) = share::recover(&journal.pending(), &key?)? else {
                            return Ok(None);
                        };
                        let delivered =
                            matches!(saved.delivery, Delivery::Accepted | Delivery::Seen);
                        let acknowledged =
                            delivered && journal.acknowledge(&saved.event_id).await.is_ok();
                        Ok::<_, String>(Some((saved, delivered, acknowledged)))
                    })
                })
                .await;
            let restored: Result<(), String> = match found {
                Ok(Some((saved, delivered, acknowledged))) if !acknowledged => {
                    let receipt = Receipt {
                        channel: saved.channel.clone(),
                        event_id: saved.event_id,
                    };
                    let _ = this.update(cx, |this, _| {
                        this.restore(saved.people, saved.level, receipt)
                    });
                    if delivered {
                        Err(String::new())
                    } else {
                        Ok(())
                    }
                }
                Ok(_) => Ok(()),
                Err(error) => Err(error),
            };
            let mut changes = outbox.changes();
            let _ = this.update(cx, |this, cx| {
                this.outbox = Some(outbox);
                match restored {
                    Ok(()) => this.ready = true,
                    Err(error) if error.is_empty() => {
                        this.ready = true;
                        this.error = Some(
                            format!("Couldn’t send {}. Try again.", this.source.kind.name()).into(),
                        );
                    }
                    Err(error) => this.error = Some(error.into()),
                }
                cx.notify();
            });
            // Delivery changes redraw the send state.
            while changes.changed().await.is_ok() {
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        }));
    }

    fn restore(&mut self, people: Vec<Recipient>, level: MemoryLevel, receipt: Receipt) {
        self.recipients = people;
        self.level = level;
        self.phase = Phase::Error;
        self.receipt = Some(receipt);
    }

    fn query(&self, cx: &App) -> String {
        self.input.read(cx).value().trim().to_string()
    }

    fn search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let query = self.query(cx);
        let session = self.session.clone();
        self.searching = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(DEBOUNCE).await;
            let page = session.people(&query, 1).await;
            let _ = this.update(cx, |this, cx| {
                this.found = Some((query, page));
                this.searching = None;
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Pubkeys of agents this app manages, on any host.
    fn managed(cx: &App) -> Vec<String> {
        cx.try_global::<Hosts>()
            .map(|hosts| {
                hosts
                    .hosts
                    .iter()
                    .flat_map(|host| host.read(cx).agents.agents.iter().map(|a| a.pubkey.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn excluded(&self) -> Vec<String> {
        let mut excluded = self.source.excluded.clone();
        excluded.push(self.session.viewer().to_owned());
        if let Some(relay) = &self.session.snapshot().borrow().relay {
            excluded.push(relay.authority.clone());
        }
        excluded
    }

    fn candidates(&self, cx: &App) -> Vec<buzz_relay_client::members::Person> {
        let excluded = self.excluded();
        let managed = Self::managed(cx);
        match &self.found {
            Some((query, Ok(page))) if *query == self.query(cx) => page
                .people
                .iter()
                .filter(|p| {
                    !self.recipients.iter().any(|r| r.pubkey == p.pubkey)
                        && !excluded.contains(&p.pubkey)
                        && (!p.profile.is_agent || managed.contains(&p.pubkey))
                })
                .cloned()
                .collect(),
            _ => Vec::new(),
        }
    }

    /// `validateRecipients`.
    fn validate(&self, cx: &App) -> Result<(), String> {
        if self.receipt.is_none()
            && let (Some(outbox), Ok(key)) = (&self.outbox, &self.recovery_key)
            && share::recover(&outbox.pending(), key)
                .ok()
                .flatten()
                .is_some()
        {
            return Err("Resume the earlier sharing operation before sending another copy.".into());
        }
        let excluded = self.excluded();
        let managed = Self::managed(cx);
        if self
            .recipients
            .iter()
            .any(|p| excluded.contains(&p.pubkey) || (p.is_agent && !managed.contains(&p.pubkey)))
        {
            return Err("A selected recipient is no longer available.".into());
        }
        Ok(())
    }

    fn encode(
        &mut self,
        level: MemoryLevel,
        cx: &mut Context<Self>,
    ) -> Task<Result<EncodedSnapshot, String>> {
        if let Some(encoded) = self.encoded.get(&level) {
            return Task::ready(Ok(encoded.clone()));
        }
        (self.source.encode)(level, cx)
    }

    /// Asks before memory leaves (`request`).
    fn request(&mut self, action: Action, cx: &mut Context<Self>) {
        if !self.ready
            || self.busy()
            || self.confirmation.is_some()
            || (action == Action::Send && self.recipients.is_empty())
        {
            return;
        }
        if self.source.has_memory_options
            && self.level != MemoryLevel::None
            && !(action == Action::Send && self.receipt.is_some())
        {
            self.confirmation = Some((action, self.level));
            cx.notify();
        } else {
            let level = if self.source.has_memory_options {
                self.level
            } else {
                MemoryLevel::None
            };
            self.perform(action, level, cx);
        }
    }

    fn perform(&mut self, action: Action, level: MemoryLevel, cx: &mut Context<Self>) {
        if self.operation.is_some() || self.actions_disabled(cx) {
            return;
        }
        self.error = None;
        match action {
            Action::Send => self.send(level, cx),
            Action::Copy => self.copy_link(level, cx),
        }
        cx.notify();
    }

    fn fail_send(&mut self, cx: &mut Context<Self>) {
        self.phase = if self.receipt.is_some() {
            Phase::Error
        } else {
            Phase::Idle
        };
        self.error = Some(format!("Couldn’t send {}. Try again.", self.source.kind.name()).into());
        self.operation = None;
        cx.notify();
    }

    fn send(&mut self, level: MemoryLevel, cx: &mut Context<Self>) {
        let Some(outbox) = self.outbox.clone() else {
            return self.fail_send(cx);
        };
        let session = self.session.clone();
        let name = self.source.display_name.clone();
        let failed = self.failed();
        if let Some(receipt) = self.receipt.clone() {
            // Retry the exact earlier message; a failed one is checked
            // against the recipients first.
            if failed && self.validate(cx).is_err() {
                return self.fail_send(cx);
            }
            self.phase = Phase::Sending;
            self.operation = Some(cx.spawn(async move |this, cx| {
                let acknowledged = cx
                    .update(|cx| {
                        on_tokio(cx, async move {
                            send::delivered(&outbox, &receipt).await?;
                            outbox
                                .acknowledge(&receipt.event_id)
                                .await
                                .map_err(|error| error.to_string())
                        })
                    })
                    .await
                    .is_ok();
                let _ = this.update(cx, |this, cx| {
                    if acknowledged {
                        this.finish(&name, cx);
                    } else {
                        this.fail_send(cx);
                    }
                });
            }));
            return;
        }
        if let Err(error) = self.validate(cx) {
            self.error = Some(error.into());
            return;
        }
        let key = match self.recovery_key.clone() {
            Ok(key) => key,
            Err(error) => {
                self.error = Some(error.into());
                return;
            }
        };
        let recovery = Recovery {
            key,
            value: share::recovery_value(&self.recipients, level),
        };
        let pubkeys: Vec<String> = self.recipients.iter().map(|p| p.pubkey.clone()).collect();
        self.phase = Phase::Preparing;
        let encode = self.encode(level, cx);
        self.operation = Some(cx.spawn(async move |this, cx| {
            let result: Result<Receipt, Option<Receipt>> = async {
                let channel = session.open_dm(pubkeys).await.map_err(|_| None)?;
                let file = encode.await.map_err(|_| None)?;
                let _ = this.update(cx, |this, cx| {
                    this.encoded.insert(level, file.clone());
                    this.phase = Phase::Uploading;
                    cx.notify();
                });
                let still = this
                    .update(cx, |this, cx| this.validate(cx).is_ok())
                    .unwrap_or(false);
                if !still {
                    return Err(None);
                }
                let attachment = send::upload(session.media(), &file)
                    .await
                    .map_err(|_| None)?;
                let event_id = {
                    let (outbox, origin, channel) =
                        (outbox.clone(), session.origin().clone(), channel.clone());
                    cx.update(|cx| {
                        on_tokio(cx, async move {
                            send::queue(&outbox, &origin, &channel, attachment, recovery)
                        })
                    })
                    .await
                    .map_err(|_| None)?
                };
                // Kept before delivery: an uncertain result retries this
                // exact message.
                let receipt = Receipt { channel, event_id };
                let _ = this.update(cx, |this, cx| {
                    this.receipt = Some(receipt.clone());
                    this.phase = Phase::Sending;
                    cx.notify();
                });
                let confirm = receipt.clone();
                cx.update(|cx| {
                    on_tokio(cx, async move {
                        send::delivered(&outbox, &confirm).await?;
                        outbox
                            .acknowledge(&confirm.event_id)
                            .await
                            .map_err(|error| error.to_string())
                    })
                })
                .await
                .map_err(|_| Some(receipt.clone()))?;
                Ok(receipt)
            }
            .await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(_) => this.finish(&name, cx),
                Err(receipt) => {
                    if receipt.is_some() {
                        this.receipt = receipt;
                    }
                    this.fail_send(cx);
                }
            });
        }));
    }

    fn finish(&mut self, name: &str, cx: &mut Context<Self>) {
        self.phase = Phase::Done;
        self.operation = None;
        notice::success(format!("Sent a copy of {name}"), cx);
        snapshots::close(cx);
    }

    /// Uploads the file and copies its link: the URL as text, the named
    /// link as metadata a paste here turns back into the attachment.
    fn copy_link(&mut self, level: MemoryLevel, cx: &mut Context<Self>) {
        self.copy = Copy::Copying;
        let encode = self.encode(level, cx);
        let session = self.session.clone();
        let name = self.source.display_name.clone();
        self.operation = Some(cx.spawn(async move |this, cx| {
            let result: Result<(), String> = async {
                let file = encode.await?;
                let _ = this.update(cx, |this, _| {
                    this.encoded.insert(level, file.clone());
                });
                let attachment = send::upload(session.media(), &file).await?;
                let html = share::clipboard_html(&attachment, &name);
                cx.update(|cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string_with_metadata(
                        attachment.url.clone(),
                        html,
                    ))
                });
                Ok(())
            }
            .await;
            let _ = this.update(cx, |this, cx| {
                this.operation = None;
                match result {
                    Ok(()) => {
                        this.copy = Copy::Copied;
                        cx.spawn(async move |this, cx| {
                            cx.background_executor()
                                .timer(Duration::from_millis(1500))
                                .await;
                            let _ = this.update(cx, |this, cx| {
                                if this.copy == Copy::Copied {
                                    this.copy = Copy::Idle;
                                    cx.notify();
                                }
                            });
                        })
                        .detach();
                    }
                    Err(_) => {
                        this.copy = Copy::Idle;
                        this.error = Some("Couldn’t copy link. Try again.".into());
                    }
                }
                cx.notify();
            });
        }));
    }

    /// Changes recipients or level, first dismissing a failed earlier send
    /// (`edit`).
    fn edit(&mut self, change: impl FnOnce(&mut Self) + 'static, cx: &mut Context<Self>) {
        if self.locked() || self.operation.is_some() {
            return;
        }
        let Some(receipt) = self.receipt.clone() else {
            change(self);
            self.error = None;
            self.copy = Copy::Idle;
            cx.notify();
            return;
        };
        let Some(outbox) = self.outbox.clone() else {
            return;
        };
        if send::delivery(&outbox, &receipt) != Some(Delivery::Failed) {
            self.error = Some("Couldn’t dismiss the earlier send. Try again.".into());
            cx.notify();
            return;
        }
        self.pending = true;
        self.operation = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .update(|cx| {
                    on_tokio(cx, async move {
                        outbox
                            .dismiss(&receipt.event_id)
                            .await
                            .map_err(|error| error.to_string())
                    })
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.pending = false;
                this.operation = None;
                match result {
                    Ok(()) => {
                        this.receipt = None;
                        this.phase = Phase::Idle;
                        change(this);
                        this.error = None;
                        this.copy = Copy::Idle;
                    }
                    Err(_) => {
                        this.error = Some("Couldn’t dismiss the earlier send. Try again.".into())
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn add(
        &mut self,
        person: buzz_relay_client::members::Person,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.recipients.len() >= MAX_RECIPIENTS {
            return;
        }
        let recipient = Recipient {
            pubkey: person.pubkey,
            name: person.profile.name,
            is_agent: person.profile.is_agent,
        };
        self.edit(move |this| this.recipients.push(recipient), cx);
        let placeholder = if self.recipients.len() >= MAX_RECIPIENTS {
            format!("{MAX_RECIPIENTS} people selected")
        } else {
            "Add another person…".into()
        };
        self.input.update(cx, |state, cx| {
            state.set_value("", window, cx);
            state.set_placeholder(placeholder, window, cx);
        });
        self.picker_open = false;
    }

    fn remove(&mut self, pubkey: String, cx: &mut Context<Self>) {
        self.edit(
            move |this| this.recipients.retain(|p| p.pubkey != pubkey),
            cx,
        );
    }

    fn avatar(&self, name: &str, picture: Option<&str>, agent: bool, size: f32) -> Avatar {
        let mut avatar = Avatar::new(name.to_owned()).size(AvatarSize::Px(size));
        if agent {
            avatar = avatar.squircle();
        }
        if let Some(picture) = picture {
            avatar = avatar.image(crate::ui::media::avatar(picture, self.session.media()));
        }
        avatar
    }

    fn picker(&self, cx: &mut Context<Self>) -> Div {
        let c = *cx.colors();
        let disabled = self.locked() || self.phase == Phase::Done;
        let profiles = self.session.snapshot().borrow().profiles.clone();
        let chips = self.recipients.iter().map(|person| {
            let key = person.pubkey.clone();
            let picture = profiles.get(&person.pubkey).and_then(|p| p.picture.clone());
            div()
                .id(SharedString::from(format!(
                    "share-recipient-{}",
                    person.pubkey
                )))
                .flex()
                .items_center()
                .gap(rems(0.375))
                .pl(rems(0.25))
                .pr(rems(0.5))
                .py(rems(0.125))
                .rounded_full()
                .bg(c.affordance_subtle)
                .text_body_sm()
                .when(!disabled, |this| {
                    this.cursor_pointer()
                        .hover(|s| s.bg(c.affordance_subtle_hover))
                        .on_click(cx.listener(move |this, _, _, cx| this.remove(key.clone(), cx)))
                })
                .child(self.avatar(&person.name, picture.as_deref(), person.is_agent, 20.))
                .child(person.name.clone())
                .when(person.is_agent, |this| {
                    this.child(div().text_color(c.text_subtle).child("Agent"))
                })
                .when(!disabled, |this| {
                    this.child(Icon::new(IconName::X).size(12.))
                })
        });
        let full = self.recipients.len() >= MAX_RECIPIENTS;
        let open = self.picker_open && !disabled;
        let candidates = if open {
            self.candidates(cx)
        } else {
            Vec::new()
        };
        let mut list = div().flex().flex_col();
        if open {
            if full {
                list = list.child(paragraph("You can message up to 8 people at once."));
            } else {
                match &self.found {
                    Some((query, Err(_))) if *query == self.query(cx) => {
                        list = list.child(alert("Could not load people.", &c));
                    }
                    Some((query, Ok(_))) if *query == self.query(cx) && candidates.is_empty() => {
                        list = list.child(paragraph("No matching people."));
                    }
                    Some((query, Ok(_))) if *query == self.query(cx) => {}
                    _ => {
                        list = list.child(
                            div()
                                .text_body_sm()
                                .text_color(c.text_subtle)
                                .child("Loading people…"),
                        )
                    }
                }
                for person in candidates {
                    let chosen = person.clone();
                    list = list.child(
                        div()
                            .id(SharedString::from(format!(
                                "share-person-{}",
                                person.pubkey
                            )))
                            .flex()
                            .items_center()
                            .gap(rems(0.5))
                            .px(rems(0.5))
                            .py(rems(0.375))
                            .rounded(buzz_ui::tokens::radius::CONTROL)
                            .cursor_pointer()
                            .hover(|s| s.bg(c.affordance_subtle_hover))
                            .child(self.avatar(
                                &person.profile.name,
                                person.profile.picture.as_deref(),
                                person.profile.is_agent,
                                24.,
                            ))
                            .child(div().text_body().child(person.profile.name.clone()))
                            .when(person.profile.is_agent, |this| {
                                this.child(
                                    div()
                                        .text_body_sm()
                                        .text_color(c.text_subtle)
                                        .child("Agent"),
                                )
                            })
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.add(chosen.clone(), window, cx)
                            })),
                    );
                }
            }
        }
        div()
            .flex()
            .flex_col()
            .gap(rems(0.5))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(rems(0.5))
                    .text_label_sm()
                    .child("To:")
                    .children(chips),
            )
            .child(
                div()
                    .id("share-recipients-input")
                    .on_click(cx.listener(|this, _, window, cx| {
                        if !this.picker_open {
                            this.picker_open = true;
                            this.search(window, cx);
                        }
                    }))
                    .child(
                        SearchField::new(&self.input)
                            .surface(Surface::Floating)
                            .disabled(disabled || full),
                    ),
            )
            .when(open, |this| {
                this.child(
                    div()
                        .id("share-people")
                        .max_h(px(200.))
                        .overflow_y_scroll()
                        .child(list),
                )
            })
    }

    fn review_block(&mut self, cx: &mut Context<Self>) -> Option<Div> {
        let review = self.source.review.clone()?;
        let c = *cx.colors();
        let json = review(cx);
        // A changed configuration needs a fresh review.
        let current = json.as_ref().ok().cloned();
        if self.reviewed_json != current {
            self.reviewed_json = current;
            self.reviewed = false;
            // Files encoded from the old configuration are stale too.
            self.encoded.clear();
        }
        let open = self.review_open;
        Some(
            div()
                .flex()
                .flex_col()
                .gap(rems(1.))
                .child(paragraph(
                    "Portable configuration includes the agent name, instructions, model, \
                     provider, worker count and avatar URL when present. Review the actual \
                     values before sharing: free text can contain secrets that automated \
                     checks cannot detect.",
                ))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(rems(0.5))
                        .child(
                            summary(
                                "share-review",
                                "Review portable configuration",
                                open,
                                TextRole::Body,
                                &c,
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.review_open = !this.review_open;
                                cx.notify();
                            })),
                        )
                        .when(open, |this| {
                            this.child(pre(
                                "share-review-json",
                                json.clone().unwrap_or_else(|_| "null".into()),
                                12.,
                                &c,
                            ))
                        }),
                )
                .child(
                    buzz_ui::Checkbox::new("share-reviewed", self.reviewed)
                        .label(
                            "I reviewed the portable configuration and understand it may \
                             contain private information.",
                        )
                        .disabled(json.is_err())
                        .on_change(checked(cx, |this, value, cx| {
                            this.reviewed = value;
                            cx.notify();
                        })),
                )
                .children(json.err().map(|error| alert(error, &c))),
        )
    }

    fn confirmation(&self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (action, level) = self.confirmation?;
        let kind = self.source.kind.name();
        let what = if level == MemoryLevel::Core {
            "core memory"
        } else {
            "all memories"
        };
        let who = match action {
            Action::Copy => "Anyone with the link can view it.".to_string(),
            Action::Send => {
                let names: Vec<String> = self.recipients.iter().map(|p| p.name.clone()).collect();
                format!(
                    "{}—and anyone with the file link—can view it.",
                    share::audience(&names)
                )
            }
        };
        let alert = AlertDialog::new("Share memories?")
            .description(format!(
                "This {kind} includes plaintext {what}. {who} Only share with people you trust."
            ))
            .action(
                Button::new("share-memories-cancel", "Cancel")
                    .surface(Surface::Floating)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.confirmation = None;
                        cx.notify();
                    })),
            )
            .action(
                Button::new(
                    "share-memories-confirm",
                    if action == Action::Copy {
                        "Copy link"
                    } else {
                        "Send"
                    },
                )
                .prominent()
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.confirmation = None;
                    this.perform(action, level, cx);
                })),
            );
        let weak = cx.entity().downgrade();
        let dismiss: Dismiss = Rc::new(move |_, cx| {
            let _ = weak.update(cx, |this, cx| {
                this.confirmation = None;
                cx.notify();
            });
        });
        Some(overlay::modal(window, alert, Some(dismiss)).into_any_element())
    }
}

impl Render for ShareDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = *cx.colors();
        let kind = self.source.kind;
        let busy = self.busy();
        let locked = self.locked();
        let review = self.review_block(cx);
        let actions_disabled = self.actions_disabled(cx);
        let sending = self.phase.sending();
        let can_send =
            self.session.snapshot().borrow().roster == buzz_relay_client::RosterStatus::Ready;
        let send_label = if sending {
            "Sending…"
        } else if self.phase == Phase::Error && self.receipt.is_some() {
            "Retry send"
        } else {
            "Send"
        };
        let picker = self.picker(cx);
        let entity = cx.entity().downgrade();
        let level = self.level;
        let levels = LEVELS
            .iter()
            .map(|&next| {
                let weak = entity.clone();
                let pick: WindowAction = Rc::new(move |_, cx| {
                    let _ = weak.update(cx, |this, cx| {
                        this.level_open = false;
                        this.edit(move |this| this.level = next, cx);
                    });
                });
                (level_label(kind, next), next == level, pick)
            })
            .collect();
        let weak = entity.clone();
        let select = inline_select(
            "share-level",
            "",
            level_label(kind, level),
            self.level_open,
            locked,
            levels,
            cx.listener(|this, _, _, cx| {
                this.level_open = !this.level_open;
                cx.notify();
            }),
            Rc::new(move |_, cx| {
                let _ = weak.update(cx, |this, cx| {
                    this.level_open = false;
                    cx.notify();
                });
            }),
            window,
        );
        let copy_label = match self.copy {
            Copy::Copying => "Copying…",
            Copy::Copied => "Copied",
            Copy::Idle => "Copy link",
        };
        let on_export = self.source.on_export.clone();
        let confirmation = self.confirmation(window, cx);
        let settings = div()
            .flex()
            .flex_col()
            .gap(rems(0.5))
            .child(
                div()
                    .text_label_sm()
                    .text_color(c.text_subtle)
                    .child("Share settings"),
            )
            .when(self.source.has_memory_options, |this| {
                this.child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap(rems(0.75))
                        .child(div().text_label().child("What’s included"))
                        .child(select),
                )
            })
            .when(
                self.source.has_memory_options && level != MemoryLevel::None,
                |this| {
                    this.child(div().text_body_sm().text_color(c.text_subtle).child(
                        "Memory is stored as plaintext in the snapshot. Only share it \
                                 with people you trust.",
                    ))
                },
            )
            .child(
                div().flex().justify_end().child(
                    Button::new("share-copy-link", copy_label)
                        .surface(Surface::Floating)
                        .disabled(!self.ready || busy || actions_disabled)
                        .on_click(cx.listener(|this, _, _, cx| this.request(Action::Copy, cx))),
                ),
            );
        let show_pending_note =
            self.phase == Phase::Error && self.receipt.is_some() && !self.failed() && !busy;
        Dialog::new(
            "share-snapshot",
            format!("Share {}", self.source.display_name),
        )
        .description(format!(
            "Anyone you share this {} with will receive a copy they can add and use. \
                 Changes you make later won’t sync.",
            kind.name()
        ))
        .when(!busy, |this| {
            this.on_close(cx.listener(|_, _, _, cx| snapshots::close(cx)))
        })
        .child(
            div()
                .flex()
                .flex_col()
                .gap(rems(1.))
                .children(review)
                .child(
                    div()
                        .flex()
                        .items_start()
                        .gap(rems(0.5))
                        .child(div().flex_1().min_w_0().child(picker))
                        .when(!self.recipients.is_empty(), |this| {
                            this.child(
                                Button::new("share-send", send_label)
                                    .surface(Surface::Floating)
                                    .disabled(
                                        !self.ready
                                            || actions_disabled
                                            || busy
                                            || self.phase == Phase::Done
                                            || !can_send,
                                    )
                                    .on_click(
                                        cx.listener(|this, _, _, cx| {
                                            this.request(Action::Send, cx)
                                        }),
                                    ),
                            )
                        }),
                )
                .when(show_pending_note, |this| {
                    this.child(paragraph(
                        "Retry the earlier message to confirm its delivery before editing.",
                    ))
                })
                .child(settings)
                .children(self.source.catalog.clone())
                .children(self.error.clone().map(|error| alert(error, &c)))
                .children(confirmation),
        )
        .footer(
            Button::new("share-export", format!("Export {}", kind.name()))
                .surface(Surface::Floating)
                .disabled(busy)
                .on_click(move |_, window, cx| on_export(window, cx)),
        )
    }
}
