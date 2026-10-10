//! Model lookups on a host: buzz-app's `createAgentModels`
//! (`src/features/agents/models.ts`).
//!
//! The host runs one lookup at a time, by ticket, apart from agent writes.
//! A lookup can be cancelled before or after its ticket arrives, and gives
//! up after 185 seconds.

use super::HostConnection;
use buzz_host::api::{Catalog, ModelRequest, Request, Response};
use buzz_host_rpc::RpcError;
use gpui_kit::{Context, Task};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const LIMIT: Duration = Duration::from_secs(185);
const CANCELLED: &str = "Connection cancelled.";

#[derive(Default)]
struct State {
    ticket: Option<u64>,
    cancelled: bool,
}

/// Cancels the lookup it came with.
#[derive(Clone, Default)]
pub struct Lookup(Arc<Mutex<State>>);

impl Lookup {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.0.lock().unwrap_or_else(|poison| poison.into_inner())
    }

    pub fn cancelled(&self) -> bool {
        self.lock().cancelled
    }

    /// The same lookup, not just an equal one.
    pub fn same(&self, other: &Lookup) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl HostConnection {
    /// Runs one model request. The returned [`Lookup`] cancels it.
    pub fn models(
        &mut self,
        request: ModelRequest,
        cx: &mut Context<Self>,
    ) -> (Task<Result<Catalog, String>>, Lookup) {
        let lookup = Lookup::default();
        let begin = self.request(Request::ModelsBegin, cx);
        let handle = lookup.clone();
        let task = cx.spawn(async move |this, cx| {
            let ticket =
                match begin.await {
                    Ok(Response::Ticket(ticket)) => ticket,
                    Ok(_) => return Err(
                        "Could not load models. Retry explicitly; your model entry is unchanged."
                            .into(),
                    ),
                    Err(RpcError::Failed(reason)) => return Err(reason),
                    Err(_) => return Err(
                        "Could not load models. Retry explicitly; your model entry is unchanged."
                            .into(),
                    ),
                };
            let cancelled = {
                let mut state = handle.lock();
                state.ticket = Some(ticket);
                state.cancelled
            };
            if cancelled {
                let _ = this.update(cx, |host, cx| {
                    host.request(Request::ModelsCancel { ticket }, cx).detach()
                });
                return Err(CANCELLED.into());
            }
            let run = this
                .update(cx, |host, cx| {
                    host.request_within(Request::ModelsRun { ticket, request }, LIMIT, cx)
                })
                .map_err(|_| CANCELLED.to_string())?;
            let result = run.await;
            let _ = this.update(cx, |host, cx| {
                host.request(Request::ModelsCancel { ticket }, cx).detach()
            });
            if handle.cancelled() {
                return Err(CANCELLED.into());
            }
            match result {
                Ok(Response::Catalog(catalog)) => Ok(catalog),
                Err(RpcError::Failed(reason)) => Err(reason),
                Err(RpcError::TimedOut) => Err("Connection timed out; retry explicitly".into()),
                _ => Err(
                    "Could not load models. Retry explicitly; your model entry is unchanged."
                        .into(),
                ),
            }
        });
        (task, lookup)
    }

    /// Cancels a lookup: its ticket now, or as soon as it arrives.
    pub fn cancel_models(&mut self, lookup: &Lookup, cx: &mut Context<Self>) {
        let ticket = {
            let mut state = lookup.lock();
            state.cancelled = true;
            state.ticket
        };
        if let Some(ticket) = ticket {
            self.request(Request::ModelsCancel { ticket }, cx).detach();
        }
    }
}
