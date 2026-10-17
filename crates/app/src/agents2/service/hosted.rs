//! Hosted agents (buzz-app's `RemoteAgentType`, #870): agents of a type
//! whose provider holds the key and runs them, such as a plugin's. The app
//! makes one through the type's steps, signs only the owner's attestation
//! and the owner's agent list, and shows it as running elsewhere.

use super::super::listings::{self, Listing};
use super::super::registry::{self, RemoteAgentType};
use super::{Agents2, selected_origin};
use crate::runtime::Runtime;
use buzz_relay_client::RelayHttp;
use gpui_kit::*;
use std::rc::Rc;

fn provider(type_key: &str, cx: &App) -> Result<Rc<dyn RemoteAgentType>, String> {
    registry::type_of(type_key, cx)
        .and_then(|kind| kind.kind.remote())
        .ok_or_else(|| "That agent type is not available".to_owned())
}

/// Whether `type_key` is a hosted type.
pub fn is_hosted(type_key: &str, cx: &App) -> bool {
    provider(type_key, cx).is_ok()
}

impl Agents2 {
    /// Makes a hosted agent of `type_key` named `name` in the selected
    /// community: its provider makes the key, the owner attests it, the
    /// provider starts it, and the owner's list names it. Resolves with
    /// its pubkey.
    pub fn create_hosted(
        &mut self,
        type_key: &str,
        name: &str,
        picture: Option<String>,
        cx: &mut Context<Self>,
    ) -> Task<Result<String, String>> {
        let name = name.trim().to_owned();
        if name.is_empty() {
            return Task::ready(Err("Name the agent".into()));
        }
        let remote = match provider(type_key, cx) {
            Ok(remote) => remote,
            Err(error) => return Task::ready(Err(error)),
        };
        let Some(origin) = selected_origin(cx) else {
            return Task::ready(Err("Connect to its community first".into()));
        };
        let Some(binding) = self
            .bound
            .get(&origin)
            .and_then(|bound| bound.binding.as_ref())
        else {
            return Task::ready(Err("Connect to its community first".into()));
        };
        let (community, viewer, identity, session) = (
            binding.destination.clone(),
            binding.viewer.clone(),
            binding.identity.clone(),
            binding.session.clone(),
        );
        let provision = remote.provision(&name, &community, cx);
        let type_key = type_key.to_owned();
        cx.spawn(async move |this, cx| {
            let pubkey = provision.await?;
            let valid = pubkey.len() == 64
                && pubkey
                    .bytes()
                    .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'));
            if !valid {
                return Err("The provider did not return the agent's key".into());
            }
            let taken = this
                .update(cx, |this, _| {
                    pubkey == viewer
                        || this.saved().any(|saved| saved.summary.pubkey == pubkey)
                        || this
                            .listed
                            .rows_of(&origin)
                            .iter()
                            .any(|row| row.pubkey == pubkey)
                })
                .unwrap_or(true);
            if taken {
                return Err("The provider returned a key that is already in use".into());
            }
            let auth: Vec<String> = identity
                .attest_agent(&pubkey)
                .map_err(|error| error.to_string())
                .and_then(|auth| serde_json::from_str(&auth).map_err(|error| error.to_string()))?;
            let activate = cx.update(|cx| remote.activate(&pubkey, &name, &community, auth, cx));
            activate.await?;
            let entry = Listing {
                pubkey: pubkey.clone(),
                name: name.clone(),
                picture: picture.clone(),
                runtime: Some(type_key.clone()),
                prior: None,
            };
            let http = RelayHttp::new(session.origin().clone(), identity.clone());
            let write = {
                let (entry, owner, runtime) = (entry.clone(), identity.clone(), type_key.clone());
                cx.update(|cx| {
                    Runtime::handle(cx).spawn(async move {
                        listings::publish(&http, &owner, &entry, &runtime).await
                    })
                })
            };
            let prior = write
                .await
                .unwrap_or_else(|_| Err("The agent list was not updated. Retry.".into()))?;
            this.update(cx, |this, cx| {
                this.listed.set_row(
                    &origin,
                    &pubkey,
                    Some(Listing {
                        prior: Some(prior),
                        ..entry
                    }),
                );
                cx.notify();
            })
            .map_err(|_| "Agents closed".to_owned())?;
            // Listed with its picture either way; its profile is the
            // provider's.
            if let Some(picture) = picture {
                let profile =
                    cx.update(|cx| remote.profile(&pubkey, &community, &name, Some(&picture), cx));
                if let Err(error) = profile.await {
                    tracing::warn!(%error, "hosted agent picture not published");
                }
            }
            Ok(pubkey)
        })
    }

    /// Deletes a hosted agent: its provider stops it and drops its key,
    /// then the owner's list forgets it.
    pub fn remove_hosted(
        &mut self,
        pubkey: &str,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), String>> {
        let Some(agent) = self
            .elsewhere(cx)
            .into_iter()
            .find(|agent| agent.pubkey == pubkey)
        else {
            return Task::ready(Err("No such hosted agent".into()));
        };
        let remote = match provider(&agent.agent_type, cx) {
            Ok(remote) => remote,
            Err(error) => return Task::ready(Err(error)),
        };
        let Some(origin) = selected_origin(cx) else {
            return Task::ready(Err("Connect to its community first".into()));
        };
        let Some(binding) = self
            .bound
            .get(&origin)
            .and_then(|bound| bound.binding.as_ref())
        else {
            return Task::ready(Err("Connect to its community first".into()));
        };
        let (identity, session) = (binding.identity.clone(), binding.session.clone());
        let remove = remote.remove(&agent.pubkey, &agent.relay, cx);
        let pubkey = agent.pubkey;
        cx.spawn(async move |this, cx| {
            remove.await?;
            let http = RelayHttp::new(session.origin().clone(), identity.clone());
            let delete = {
                let pubkey = pubkey.clone();
                cx.update(|cx| {
                    Runtime::handle(cx)
                        .spawn(async move { listings::delete(&http, &identity, &pubkey).await })
                })
            };
            delete
                .await
                .unwrap_or_else(|_| Err("Record deletion is not confirmed. Retry.".into()))?;
            this.update(cx, |this, cx| {
                this.listed.set_row(&origin, &pubkey, None);
                cx.notify();
            })
            .map_err(|_| "Agents closed".to_owned())
        })
    }
}
