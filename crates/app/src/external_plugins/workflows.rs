//! A channel's workflows as the user sees them (buzz-app's `workflows`):
//! their definitions and run history, saving, deleting and triggering
//! them, and how those commands went. Commands need `manage-channels`.

use super::capabilities::{Failure, channel, community, on_tokio};
use super::protocol::INVALID_PARAMS;
use buzz_relay_client::workflows::{
    Availability, RunCursor, RunStatus, ViewStatus, WorkflowDefinition, WorkflowOperation,
    Workflows, delivery_label,
};
use gpui_kit::*;
use serde_json::{Value, json};

fn invalid(message: impl Into<String>) -> Failure {
    (INVALID_PARAMS, message.into())
}

fn text<'a>(params: &'a Value, key: &str) -> Option<&'a str> {
    params.get(key).and_then(Value::as_str)
}

fn status(status: ViewStatus) -> &'static str {
    match status {
        ViewStatus::Idle => "idle",
        ViewStatus::Loading => "loading",
        ViewStatus::Ready => "ready",
        ViewStatus::Error => "error",
        ViewStatus::Unavailable => "unavailable",
    }
}

fn definition(definition: &WorkflowDefinition) -> Value {
    json!({
        "id": definition.id,
        "owner": definition.owner,
        "channel": definition.channel_id,
        "revision": definition.revision,
        "createdAt": definition.created_at,
        "yaml": definition.yaml,
    })
}

fn availability(availability: Availability) -> Value {
    json!({
        "definitions": availability.definitions,
        "history": availability.history,
        "save": availability.save,
        "trigger": availability.trigger,
        "delete": availability.delete,
    })
}

fn operation(operation: &WorkflowOperation) -> Value {
    json!({
        "eventId": operation.event_id,
        "workflow": operation.workflow.id,
        "channel": operation.workflow.channel_id,
        "action": operation.action.as_str(),
        "createdAt": operation.created_at,
        "delivery": delivery_label(operation.delivery),
        "outcome": operation.outcome.as_str(),
        "error": operation.error,
        "run": operation.run_id,
        "secretHeld": operation.secret_held,
    })
}

/// The workflows of the channel at `channel`, read now.
async fn read(
    workflows: &Workflows,
    channel: String,
) -> Result<(Vec<WorkflowDefinition>, Value), Failure> {
    let view = workflows
        .definitions(vec![channel], false)
        .map_err(|error| (3, error))?;
    view.refresh(false).await;
    let snapshot = view.snapshot();
    if let Some(error) = &snapshot.error {
        return Err((3, error.clone()));
    }
    let answer = json!({
        "status": status(snapshot.status),
        "workflows": snapshot.data.items.iter().map(definition).collect::<Vec<_>>(),
        "partial": snapshot.data.partial,
    });
    Ok((snapshot.data.items.clone(), answer))
}

/// The definition named by `params.workflow` among `definitions`.
fn named(definitions: Vec<WorkflowDefinition>, id: &str) -> Result<WorkflowDefinition, Failure> {
    definitions
        .into_iter()
        .find(|definition| definition.id == id)
        .ok_or_else(|| (3, format!("There is no workflow {id} in the channel.")))
}

/// `workflows/list`: what the community offers, and a channel's
/// workflow definitions.
pub fn list(params: &Value, cx: &App) -> Result<Task<Result<Value, Failure>>, Failure> {
    let community = community(cx)?;
    let channel = channel(params, community.read(cx))?;
    let workflows = community.read(cx).session.workflows().clone();
    Ok(on_tokio(cx, async move {
        let available = availability(workflows.availability());
        let (_, mut answer) = read(&workflows, channel).await?;
        answer["available"] = available;
        Ok(answer)
    }))
}

/// `workflows/runs`: a page of a workflow's runs, newest first; `cursor`
/// is the `next` of the page before.
pub fn runs(params: &Value, cx: &App) -> Result<Task<Result<Value, Failure>>, Failure> {
    let community = community(cx)?;
    let channel = channel(params, community.read(cx))?;
    let id = text(params, "workflow")
        .ok_or_else(|| invalid("workflow is required"))?
        .to_owned();
    let cursor = match params.get("cursor").filter(|cursor| !cursor.is_null()) {
        None => None,
        Some(cursor) => Some(RunCursor {
            before: text(cursor, "before")
                .ok_or_else(|| invalid("cursor.before is required"))?
                .to_owned(),
            before_id: text(cursor, "beforeId")
                .ok_or_else(|| invalid("cursor.beforeId is required"))?
                .to_owned(),
        }),
    };
    let workflows = community.read(cx).session.workflows().clone();
    Ok(on_tokio(cx, async move {
        let (definitions, _) = read(&workflows, channel).await?;
        let workflow = named(definitions, &id)?;
        let view = workflows
            .runs(&workflow, cursor)
            .map_err(|error| (3, error))?;
        view.refresh(false).await;
        let snapshot = view.snapshot();
        if let Some(error) = &snapshot.error {
            return Err((3, error.clone()));
        }
        Ok(json!({
            "status": status(snapshot.status),
            "runs": snapshot.data.runs.iter().map(|run| json!({
                "id": run.id,
                "workflow": run.workflow_id,
                "status": match run.status {
                    RunStatus::Pending => "pending",
                    RunStatus::Running => "running",
                    RunStatus::WaitingApproval => "waitingApproval",
                    RunStatus::Completed => "completed",
                    RunStatus::Failed => "failed",
                    RunStatus::Cancelled => "cancelled",
                },
                "currentStep": run.current_step,
                "trace": run.trace,
                "startedAt": run.started_at,
                "completedAt": run.completed_at,
                "createdAt": run.created_at,
                "errorCode": run.error_code,
                "errorMessage": run.error_message,
            })).collect::<Vec<_>>(),
            "next": snapshot.data.next.as_ref().map(|next| json!({
                "before": next.before,
                "beforeId": next.before_id,
            })),
        }))
    }))
}

/// `workflows/save`, `workflows/delete` and `workflows/trigger`: sends
/// the command and answers with its event id; `workflows/operations`
/// then follows it.
pub fn command(
    method: &str,
    params: &Value,
    cx: &App,
) -> Result<Task<Result<Value, Failure>>, Failure> {
    let community = community(cx)?;
    let channel = channel(params, community.read(cx))?;
    let id = text(params, "workflow").map(str::to_owned);
    let yaml = text(params, "yaml").map(str::to_owned);
    let method = method.to_owned();
    if method != "workflows/save" && id.is_none() {
        return Err(invalid("workflow is required"));
    }
    if method == "workflows/save" && yaml.is_none() {
        return Err(invalid("yaml is required"));
    }
    let workflows = community.read(cx).session.workflows().clone();
    Ok(on_tokio(cx, async move {
        let existing = match &id {
            Some(id) => Some(named(read(&workflows, channel.clone()).await?.0, id)?),
            None => None,
        };
        let sent = match (method.as_str(), existing) {
            ("workflows/save", existing) => {
                workflows.save(&channel, yaml.unwrap_or_default(), existing.as_ref())
            }
            ("workflows/delete", Some(workflow)) => workflows.delete(&workflow),
            (_, Some(workflow)) => workflows.trigger(&workflow),
            (_, None) => return Err(invalid("workflow is required")),
        };
        sent.map(|event| json!({ "eventId": event }))
            .map_err(|error| (3, error))
    }))
}

/// `workflows/operations`: the commands sent this session and how each
/// went, with a new webhook's `webhookUrl` and its one-time `secret`
/// while it is held.
pub fn operations(cx: &App) -> Result<Value, Failure> {
    let workflows = community(cx)?.read(cx).session.workflows().clone();
    Ok(json!(
        workflows
            .operations()
            .iter()
            .map(|op| {
                let mut value = operation(op);
                value["webhookUrl"] = json!(workflows.webhook_url(&op.workflow.id));
                value
            })
            .collect::<Vec<_>>()
    ))
}

/// `workflows/takeSecret`: a new webhook's one-time secret, once.
pub fn take_secret(params: &Value, cx: &App) -> Result<Value, Failure> {
    let event = text(params, "eventId").ok_or_else(|| invalid("eventId is required"))?;
    let workflows = community(cx)?.read(cx).session.workflows().clone();
    Ok(json!(workflows.take_webhook_secret(event)))
}

/// `workflows/dismiss`: forgets a finished or failed command.
pub fn dismiss(params: &Value, cx: &App) -> Result<Task<Result<Value, Failure>>, Failure> {
    let event = text(params, "eventId")
        .ok_or_else(|| invalid("eventId is required"))?
        .to_owned();
    let workflows = community(cx)?.read(cx).session.workflows().clone();
    Ok(on_tokio(cx, async move {
        workflows
            .dismiss(&event)
            .await
            .map(|()| Value::Null)
            .map_err(|error| (3, error))
    }))
}
