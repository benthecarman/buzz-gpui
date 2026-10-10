//! Message actions beyond send, edit, react and remove (spec §5d.5,
//! §5d.8, §5i): thread follows, a message's own read state, deletions by
//! an agent's owner, and the outbox itself so recovery UIs can list and
//! retry its operations. Reports and "Send to channel" go through the
//! session's write checks (`Session::report`, `Session::send_to_channel`).

use super::{Actor, Message, Session};
use crate::event::Verified;
use crate::messages::{self, AgentOwner, Author};
use crate::outbox::{Outbox, SendOptions};
use nostr::prelude::EventId;
use tokio::sync::oneshot;

const CLOSED: &str = "The session has closed";
const NOT_CONNECTED: &str = "Not connected to the relay yet";

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
