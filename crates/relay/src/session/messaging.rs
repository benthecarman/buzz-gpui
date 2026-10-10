//! Message actions beyond send, edit, react and remove (spec §5d.5,
//! §5d.8, §5d.10, §5i): reports, "Send to channel", thread follows, a
//! message's own read state, deletions by an agent's owner, and the
//! outbox itself so recovery UIs can list and retry its operations.

use super::{Actor, Message, Session};
use crate::event::{Verified, kinds};
use crate::fold::{Delivery as RowDelivery, MessageRow};
use crate::messages::{self, AgentOwner, Author, ReportType, SentFromThread, ThreadRoot};
use crate::outbox::{Delivery, Outbox, PublishError, SendOptions, accept_publish};
use buzz_identity::Purpose;
use nostr::prelude::EventId;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::oneshot;

/// Reports are published directly and give up after this (§5d.10).
const REPORT_TIMEOUT: Duration = Duration::from_secs(10);
const CLOSED: &str = "The session has closed";
const NOT_CONNECTED: &str = "Not connected to the relay yet";

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

impl Actor {
    /// A message shown in `channel`'s timeline or in an open thread.
    fn find_message(&self, channel: &str, id: &EventId) -> Option<Verified> {
        self.timelines
            .get(channel)
            .and_then(|timeline| timeline.find(id))
            .or_else(|| {
                self.threads
                    .values()
                    .filter(|thread| thread.channel == channel)
                    .find_map(|thread| thread.state.event(id).cloned())
            })
    }
}

impl Session {
    /// Runs `work` on the actor and hands back its answer.
    async fn with_actor<R: Send + 'static>(
        &self,
        work: impl FnOnce(&mut Actor) -> R + Send + 'static,
    ) -> Result<R, String> {
        let (reply, answer) = oneshot::channel();
        self.commands
            .send(Message::With(Box::new(move |actor| {
                let _ = reply.send(work(actor));
            })))
            .map_err(|_| CLOSED.to_string())?;
        answer.await.map_err(|_| CLOSED.to_string())
    }

    /// The session's outbox, once connected. A reconnect replaces it, so
    /// callers fetch it again when the session becomes ready.
    pub async fn outbox(&self) -> Option<Outbox> {
        self.with_actor(|actor| actor.outbox.clone())
            .await
            .ok()
            .flatten()
    }

    /// Reports a message to the community's moderators: a kind-1984
    /// published directly, never through the outbox, within 10 seconds.
    pub async fn report(
        &self,
        channel: &str,
        id: EventId,
        kind: ReportType,
        note: &str,
    ) -> Result<(), String> {
        let channel = channel.to_owned();
        let original = self
            .with_actor(move |actor| actor.find_message(&channel, &id))
            .await?
            .ok_or("Load the message before reporting it")?;
        let template = messages::report(original.event(), kind, note, now())
            .map_err(|error| error.to_string())?;
        let (http, identity) = (self.direct.http.clone(), self.direct.identity.clone());
        self.run(async move {
            let work = async move {
                let unsigned = identity
                    .template(
                        Purpose::Content,
                        template.kind,
                        template.created_at,
                        template.tags,
                        template.content,
                    )
                    .map_err(|error| error.to_string())?;
                let event = identity
                    .sign(Purpose::Content, unsigned)
                    .map_err(|error| error.to_string())?;
                let posted = http
                    .post(
                        "/events",
                        serde_json::to_string(&event).expect("events serialize"),
                    )
                    .await;
                accept_publish(posted, &event.id)
                    .map(drop)
                    .map_err(|error| match error {
                        PublishError::Rejected(message) | PublishError::Uncertain(message) => {
                            message
                        }
                    })
            };
            tokio::time::timeout(REPORT_TIMEOUT, work)
                .await
                .unwrap_or_else(|_| Err("The report timed out.".into()))
        })
        .await
    }

    /// Copies the viewer's own thread reply into the channel as a new
    /// top-level message (`sendToChannel`). `root` is the thread root as
    /// the panel shows it, for the excerpt.
    pub async fn send_to_channel(
        &self,
        reply: Arc<MessageRow>,
        root: Option<Arc<MessageRow>>,
    ) -> Result<(), String> {
        self.with_actor(move |actor| actor.send_to_channel(&reply, root.as_deref()))
            .await?
    }

    /// Reacts with `content`, reusing a reaction pill's own emoji tag, so a
    /// custom emoji works even when the viewer's catalog lacks it.
    pub async fn react_with(
        &self,
        channel: &str,
        id: EventId,
        content: &str,
        emoji: Option<messages::CustomEmoji>,
    ) -> Result<(), String> {
        let (channel, content) = (channel.to_owned(), content.to_owned());
        self.with_actor(move |actor| {
            let outbox = actor.outbox.clone().ok_or(NOT_CONNECTED)?;
            let original = actor
                .find_message(&channel, &id)
                .ok_or("Load the message first")?;
            let catalog = actor.emoji.snapshot();
            let custom = emoji.or_else(|| {
                content
                    .strip_prefix(':')
                    .and_then(|code| code.strip_suffix(':'))
                    .and_then(|code| catalog.get(&code.to_ascii_lowercase()))
                    .cloned()
            });
            let draft = messages::react(&original, &content, custom.as_ref(), catalog.catalog())
                .map_err(|error| error.to_string())?;
            outbox
                .send(draft, SendOptions::default())
                .map(drop)
                .map_err(|error| error.to_string())
        })
        .await?
    }

    /// Whether the viewer follows the thread rooted at `root`.
    pub async fn following(&self, channel: &str, root: EventId) -> bool {
        let channel = channel.to_owned();
        self.with_actor(move |actor| {
            actor
                .reads
                .as_mut()
                .is_some_and(|reads| reads.unread.following(&channel, &root.to_hex()))
        })
        .await
        .unwrap_or(false)
    }

    /// Follows or unfollows a thread on this device.
    pub async fn follow(
        &self,
        channel: &str,
        root: EventId,
        following: bool,
    ) -> Result<(), String> {
        let channel = channel.to_owned();
        self.with_actor(move |actor| {
            let reads = actor.reads.as_mut().ok_or(NOT_CONNECTED)?;
            let result = reads
                .unread
                .follow(&channel, &root.to_hex(), following)
                .map_err(|error| error.to_string());
            actor.publish();
            result
        })
        .await?
    }

    /// Whether a loaded message shows as unread (or forced unread) now,
    /// for the "Mark read" / "Mark unread" choice.
    pub async fn message_unread(&self, channel: &str, id: EventId) -> bool {
        let channel = channel.to_owned();
        self.with_actor(move |actor| {
            let Some(reads) = actor.reads.as_mut() else {
                return false;
            };
            let attention = reads.unread.attention(&channel, &id.to_hex(), &reads.sync);
            attention.unread || attention.forced
        })
        .await
        .unwrap_or(false)
    }

    /// Deletes an agent's message on behalf of its owner, the viewer. The
    /// caller passes the agent only after its signed profile attests the
    /// viewer as owner.
    pub async fn remove_as_owner(
        &self,
        channel: &str,
        id: EventId,
        agent: &str,
    ) -> Result<(), String> {
        let (channel, agent) = (channel.to_owned(), agent.to_owned());
        self.with_actor(move |actor| {
            let outbox = actor.outbox.clone().ok_or(NOT_CONNECTED)?;
            let target = actor
                .find_message(&channel, &id)
                .ok_or("Load the message first")?;
            let viewer = actor.identity.pubkey_hex();
            let authority = actor.relay.as_ref().map(|relay| relay.authority.clone());
            let author = Author {
                viewer: &viewer,
                signing_authority: authority.as_deref(),
            };
            let drafts = messages::remove(
                &author,
                &[id.to_hex()],
                |_| Some(target.event()),
                Some(AgentOwner {
                    agent_id: &agent,
                    owner_id: &viewer,
                }),
            )
            .map_err(|error| error.to_string())?;
            for draft in drafts {
                outbox
                    .send(draft, SendOptions::default())
                    .map_err(|error| error.to_string())?;
            }
            Ok(())
        })
        .await?
    }
}

impl Actor {
    fn send_to_channel(
        &mut self,
        row: &MessageRow,
        root: Option<&MessageRow>,
    ) -> Result<(), String> {
        let outbox = self.outbox.clone().ok_or(NOT_CONNECTED)?;
        let channel = row.channel.to_string();
        if !self
            .discovery
            .as_ref()
            .is_some_and(|d| d.can_participate(&channel))
        {
            return Err("You can't post in this channel".into());
        }
        let reply = self
            .find_message(&channel, &row.id)
            .ok_or("Load a thread reply before sending it to the channel")?;
        let source_id = row.attachment_source_id.unwrap_or(row.id);
        let source = self
            .find_message(&channel, &source_id)
            .ok_or("Reload the message attachments before sending")?;
        let viewer = self.identity.pubkey_hex();
        let authority = self.relay.as_ref().map(|relay| relay.authority.clone());
        let author = Author {
            viewer: &viewer,
            signing_authority: authority.as_deref(),
        };
        let mentions: Vec<String> = row
            .mentions
            .iter()
            .chain(&row.mention_references)
            .map(|key| key.to_hex())
            .collect();
        let urls: Vec<String> = row.attachments.iter().map(|a| a.url.clone()).collect();
        let emoji: Vec<messages::CustomEmoji> = row
            .emoji
            .iter()
            .map(|emoji| messages::CustomEmoji {
                shortcode: emoji.shortcode.clone(),
                url: emoji.url.clone(),
            })
            .collect();
        let content = row.source_content.as_deref().unwrap_or(&row.content);
        let root_id = root.map(|root| root.id.to_hex());
        let thread_root = root.zip(root_id.as_deref()).map(|(root, id)| ThreadRoot {
            id,
            channel_id: &root.channel,
            content: root.source_content.as_deref().unwrap_or(&root.content),
        });
        let delivery = row.delivery.as_ref().map(|local| match local.status {
            RowDelivery::Sending => Delivery::Sending,
            RowDelivery::Accepted => Delivery::Accepted,
            RowDelivery::Unknown => Delivery::Unknown,
            RowDelivery::Failed => Delivery::Failed,
            RowDelivery::Seen => Delivery::Seen,
        });
        let draft = messages::sent_from_thread(
            &author,
            &SentFromThread {
                reply: reply.event(),
                attachment_source: source.event(),
                content,
                attachment_urls: &urls,
                mentions: &mentions,
                edited: row.edited,
                emoji: &emoji,
                delivery,
                root: thread_root,
            },
        )
        .map_err(|error| error.to_string())?;
        debug_assert_eq!(draft.kind, kinds::CHAT);
        outbox
            .send(draft, SendOptions::default())
            .map(drop)
            .map_err(|error| error.to_string())
    }
}
