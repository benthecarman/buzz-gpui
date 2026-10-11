//! Model catalogs, connection tests and Databricks sign-in, through
//! buzz-app's `buzz_desktop::agent_models`.
//!
//! The model host admits one lookup at a time, by ticket. It reads agent
//! settings from this host and opens the sign-in page through the GUI that
//! asked ([`HostAsk::OpenUrl`]). The sign-in's redirect goes to a loopback
//! listener on this host; a GUI on another machine forwards the browser's
//! callback to it with [`Request::SignInCallback`](crate::api::Request).

use crate::api::{self, HostAsk, ModelCatalog, SignInPage};
use crate::{AgentHost, Asker, failed};
use buzz_agent_controller::{AgentEdit, Controller};
use buzz_desktop::agent_models::AgentContexts;
use buzz_host_rpc::RpcError;
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How long a GUI may forward a sign-in callback after the page opened:
/// buzz-agent's own browser sign-in timeout.
const SIGN_IN_WINDOW: Duration = Duration::from_secs(300);

/// This host's agent settings, as the model host reads them.
#[derive(Clone)]
struct Contexts(Arc<AgentHost>);

impl AgentContexts for Contexts {
    fn ensure_open(&self) -> impl Future<Output = Result<(), String>> + Send {
        let host = self.0.clone();
        async move { host.run_text(|_| Ok(())).await }
    }

    fn inherited_workspace(&self) -> impl Future<Output = Result<Option<String>, String>> + Send {
        let host = self.0.clone();
        async move {
            host.run_text(|owner| owner.controller.inherited_workspace())
                .await
        }
    }

    fn disconnect(&self, workspace: &str) -> impl Future<Output = Result<(), String>> + Send {
        let host = self.0.clone();
        let workspace = workspace.to_string();
        async move {
            let workspace = buzz_agent_controller::connection::origin(&workspace)?;
            host.run_text(move |owner| {
                owner.controller.disconnect(&workspace)?;
                owner.cancel_starts_for(&workspace);
                Ok(())
            })
            .await
        }
    }

    fn model_context(
        &self,
        id: Option<&str>,
        revision: Option<u64>,
        edit: AgentEdit,
    ) -> impl Future<Output = Result<buzz_agent_controller::ModelContext, String>> + Send {
        let host = self.0.clone();
        let id = id.map(str::to_owned);
        async move {
            host.run_text(move |owner| {
                let edit = owner.default_workspace(edit);
                match (id.as_deref(), revision) {
                    (Some(id), Some(revision)) => {
                        owner.controller.model_context(id, revision, edit)
                    }
                    (None, None) => {
                        Controller::draft_model_context(owner.controller.effective_draft(edit)?)
                    }
                    _ => Err("Invalid agent model context".into()),
                }
            })
            .await
        }
    }

    fn goose_model_context(
        &self,
        id: Option<&str>,
        revision: Option<u64>,
        edit: AgentEdit,
    ) -> impl Future<Output = Result<buzz_agent_controller::GooseModelContext, String>> + Send {
        let host = self.0.clone();
        let id = id.map(str::to_owned);
        async move {
            let _ = AgentHost::tools_path().await;
            host.run_text(move |owner| {
                let edit = owner.default_workspace(edit);
                match (id.as_deref(), revision) {
                    (Some(id), Some(revision)) => {
                        owner.controller.goose_model_context(id, revision, edit)
                    }
                    (None, None) => {
                        let edit = owner.controller.effective_draft(edit)?;
                        owner.controller.draft_goose_model_context(edit)
                    }
                    _ => Err("Invalid agent model context".into()),
                }
            })
            .await
        }
    }

    fn pi_model_context(
        &self,
        id: Option<&str>,
        revision: Option<u64>,
        edit: AgentEdit,
    ) -> impl Future<Output = Result<buzz_agent_controller::pi::PiContext, String>> + Send {
        let host = self.0.clone();
        let id = id.map(str::to_owned);
        async move {
            let _ = AgentHost::tools_path().await;
            host.run_text(move |owner| {
                let edit = owner.default_workspace(edit);
                match (id.as_deref(), revision) {
                    (Some(id), Some(revision)) => {
                        owner.controller.pi_model_context(id, revision, edit)
                    }
                    (None, None) => {
                        Controller::draft_pi_model_context(owner.controller.effective_draft(edit)?)
                    }
                    _ => Err("Invalid agent model context".into()),
                }
            })
            .await
        }
    }

    fn codex_model_context(
        &self,
        id: Option<&str>,
        revision: Option<u64>,
        edit: AgentEdit,
    ) -> impl Future<Output = Result<buzz_agent_controller::codex::CodexContext, String>> + Send
    {
        let host = self.0.clone();
        let id = id.map(str::to_owned);
        async move {
            let _ = AgentHost::tools_path().await;
            host.run_text(move |owner| {
                let edit = owner.default_workspace(edit);
                match (id.as_deref(), revision) {
                    (Some(id), Some(revision)) => {
                        owner.controller.codex_model_context(id, revision, edit)
                    }
                    (None, None) => {
                        let edit = owner.controller.effective_draft(edit)?;
                        owner.controller.draft_codex_model_context(edit)
                    }
                    _ => Err("Invalid agent model context".into()),
                }
            })
            .await
        }
    }
}

impl crate::Owner {
    /// An empty workspace means the host's default, as on create.
    fn default_workspace(&self, mut edit: AgentEdit) -> AgentEdit {
        if edit.workspace.is_empty() {
            edit.workspace = self.workspace.to_string_lossy().into_owned();
        }
        edit
    }
}

/// The loopback port a sign-in page redirects to.
fn redirect_port(url: &str) -> Option<u16> {
    let url = url::Url::parse(url).ok()?;
    let redirect = url
        .query_pairs()
        .find(|(name, _)| name == "redirect_uri")?
        .1;
    let redirect = url::Url::parse(&redirect).ok()?;
    matches!(redirect.host_str(), Some("localhost" | "127.0.0.1"))
        .then(|| redirect.port())
        .flatten()
}

impl AgentHost {
    /// Runs a model request under its ticket.
    pub(crate) async fn run_models(
        &self,
        ticket: u64,
        request: api::ModelRequest,
        asker: Option<Arc<dyn Asker>>,
    ) -> Result<api::Catalog, RpcError> {
        let host = self
            .weak
            .upgrade()
            .ok_or_else(|| failed("The agent host stopped"))?;
        let request: buzz_desktop::agent_models::Request =
            crate::convert(&request).map_err(|_| failed("Invalid model request"))?;
        let weak = self.weak.clone();
        let open_url = move |url: &str| -> Result<(), String> {
            let asker = asker.clone().ok_or("No app to open the sign-in browser")?;
            if let (Some(port), Some(host)) = (redirect_port(url), weak.upgrade())
                && let Ok(mut sign_ins) = host.sign_ins.lock()
            {
                sign_ins.retain(|_, opened| opened.elapsed() < SIGN_IN_WINDOW);
                sign_ins.insert(port, Instant::now());
            }
            let url = url.to_string();
            tokio::spawn(async move {
                if let Err(error) = asker.ask(HostAsk::OpenUrl { url }).await {
                    tracing::warn!(%error, "the app could not open the sign-in page");
                }
            });
            Ok(())
        };
        let catalog = self
            .models
            .run_request(ticket, request, Contexts(host), open_url)
            .await
            .map_err(RpcError::Failed)?;
        crate::convert(&catalog).map_err(|_| failed("Invalid model catalog"))
    }

    /// Runs one model request with its own ticket, for the requests GUIs
    /// sent before tickets.
    async fn one_model_request(
        &self,
        request: api::ModelRequest,
        asker: Option<Arc<dyn Asker>>,
    ) -> Result<api::Catalog, RpcError> {
        let ticket = self.models.begin().map_err(failed)?;
        let result = self.run_models(ticket, request, asker).await;
        let _ = self.models.cancel(ticket);
        result
    }

    pub(crate) async fn legacy_models(
        &self,
        id: Option<String>,
        revision: u64,
        edit: api::AgentEdit,
        asker: Option<Arc<dyn Asker>>,
    ) -> Result<ModelCatalog, RpcError> {
        let databricks = edit.harness.databricks.clone().unwrap_or_default();
        let catalog = self
            .one_model_request(
                api::ModelRequest {
                    expected_revision: id.as_ref().map(|_| revision),
                    id,
                    inherit_workspace: databricks.host.is_empty(),
                    host: databricks.host,
                    filter: databricks.filter,
                    edit: Some(edit),
                    action: api::ModelAction::Connect,
                },
                asker,
            )
            .await?;
        Ok(ModelCatalog {
            models: catalog.models,
            model_overridden: catalog.model_overridden,
        })
    }

    pub(crate) async fn legacy_test(
        &self,
        id: Option<String>,
        revision: u64,
        edit: api::AgentEdit,
    ) -> Result<Option<String>, RpcError> {
        let catalog = self
            .one_model_request(
                api::ModelRequest {
                    expected_revision: id.as_ref().map(|_| revision),
                    id,
                    edit: Some(edit),
                    action: api::ModelAction::Test,
                    ..Default::default()
                },
                None,
            )
            .await?;
        Ok(catalog.tested_model)
    }

    pub(crate) async fn legacy_disconnect(&self, workspace: String) -> Result<(), RpcError> {
        self.one_model_request(
            api::ModelRequest {
                inherit_workspace: workspace.is_empty(),
                host: workspace,
                action: api::ModelAction::Disconnect,
                ..Default::default()
            },
            None,
        )
        .await
        .map(drop)
    }

    /// Replays a browser callback the GUI received on this host's loopback
    /// sign-in listener, for a port a sign-in here opened.
    pub(crate) async fn sign_in_callback(
        &self,
        port: u16,
        target: String,
    ) -> Result<SignInPage, RpcError> {
        let known = self.sign_ins.lock().is_ok_and(|sign_ins| {
            sign_ins
                .get(&port)
                .is_some_and(|opened| opened.elapsed() < SIGN_IN_WINDOW)
        });
        if !known || !target.starts_with('/') || target.starts_with("//") || target.len() > 8192 {
            return Err(failed("This sign-in is no longer waiting"));
        }
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|_| failed("Could not reach the sign-in listener"))?;
        let response = client
            .get(format!("http://127.0.0.1:{port}{target}"))
            .send()
            .await
            .map_err(|_| failed("Could not reach the sign-in listener"))?;
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("text/html; charset=utf-8")
            .to_string();
        let body = response
            .text()
            .await
            .map_err(|_| failed("Could not read the sign-in page"))?;
        if let Ok(mut sign_ins) = self.sign_ins.lock() {
            sign_ins.remove(&port);
        }
        Ok(SignInPage {
            status,
            content_type,
            body: body.chars().take(64 * 1024).collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::redirect_port;

    #[test]
    fn sign_in_ports_come_from_loopback_redirects_only() {
        let page = |redirect: &str| {
            format!(
                "https://dbc.example/oidc/v1/authorize?response_type=code&redirect_uri={}",
                url::form_urlencoded::byte_serialize(redirect.as_bytes()).collect::<String>()
            )
        };
        assert_eq!(redirect_port(&page("http://localhost:43211")), Some(43211));
        assert_eq!(redirect_port(&page("http://127.0.0.1:8020/cb")), Some(8020));
        assert_eq!(redirect_port(&page("https://evil.example:8020")), None);
        assert_eq!(redirect_port("https://dbc.example/authorize"), None);
    }
}
