//! Checks on a pull request's head commit (`checks.ts`): check runs and
//! legacy commit statuses, at most three pages of each, read once.

use super::client::{Fetch, JSON, encode_component};
use serde_json::{Map, Value};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Category {
    Failing,
    Cancelled,
    Pending,
    Unknown,
    Skipped,
    Neutral,
    Successful,
}

impl Category {
    /// Count order, as the summary lists them.
    pub const COUNTS: [Self; 7] = [
        Self::Failing,
        Self::Cancelled,
        Self::Pending,
        Self::Unknown,
        Self::Skipped,
        Self::Neutral,
        Self::Successful,
    ];
    /// Row order: actionable results first.
    const PRIORITY: [Self; 7] = [
        Self::Failing,
        Self::Cancelled,
        Self::Pending,
        Self::Unknown,
        Self::Successful,
        Self::Skipped,
        Self::Neutral,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Failing => "failing",
            Self::Cancelled => "cancelled",
            Self::Pending => "pending",
            Self::Unknown => "unknown",
            Self::Skipped => "skipped",
            Self::Neutral => "neutral",
            Self::Successful => "successful",
        }
    }

    fn priority(self) -> usize {
        Self::PRIORITY.iter().position(|c| *c == self).unwrap_or(0)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Check {
    pub key: String,
    pub name: String,
    pub category: Category,
    pub label: String,
    pub description: Option<String>,
    pub url: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Failure,
    Pending,
    Success,
    Neutral,
    Unavailable,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Summary {
    pub state: State,
    pub label: String,
    pub description: String,
    pub checks: Option<Vec<Check>>,
}

const SCOPE: &str = "PR head commit · ";

pub fn unavailable() -> Summary {
    Summary {
        state: State::Unavailable,
        label: "Unavailable".into(),
        description: format!("{SCOPE}Couldn’t load all checks. Try again or open on GitHub."),
        checks: None,
    }
}

/// Shown while checks load.
pub fn loading() -> Summary {
    Summary {
        state: State::Neutral,
        label: "Loading…".into(),
        description: "Loading checks on the PR head commit.".into(),
        checks: None,
    }
}

fn result_label(outcome: &str) -> Option<&'static str> {
    Some(match outcome {
        "success" => "Passed",
        "failure" | "error" => "Failed",
        "timed_out" => "Timed out",
        "action_required" => "Action required",
        "stale" => "Stale",
        "startup_failure" => "Startup failed",
        "cancelled" => "Cancelled",
        "skipped" => "Skipped",
        "neutral" => "Neutral",
        "queued" => "Queued",
        "in_progress" => "In progress",
        "waiting" => "Waiting",
        "pending" => "Pending",
        "requested" => "Requested",
        _ => return None,
    })
}

fn safe_url(value: Option<&Value>) -> Option<String> {
    let url = url::Url::parse(value?.as_str()?).ok()?;
    (url.scheme() == "https" && url.username().is_empty() && url.password().is_none())
        .then(|| url.to_string())
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Runs,
    Statuses,
}

/// One source's items, and whether every page was read.
async fn read(
    fetch: &Arc<dyn Fetch>,
    repository: &str,
    sha: &str,
    kind: Kind,
) -> Result<(Vec<Value>, bool), ()> {
    let mut items = Vec::new();
    for page in 1..=3 {
        let url = match kind {
            Kind::Runs => format!(
                "https://api.github.com/repos/{repository}/commits/{}/check-runs?per_page=100&page={page}&filter=latest",
                encode_component(sha)
            ),
            Kind::Statuses => format!(
                "https://api.github.com/repos/{repository}/commits/{}/status?per_page=100&page={page}",
                encode_component(sha)
            ),
        };
        let response = fetch.get(url, JSON).await.map_err(|_| ())?;
        if !response.ok() {
            return Err(());
        }
        let data: Value = serde_json::from_slice(&response.body).map_err(|_| ())?;
        let batch = data
            .get(match kind {
                Kind::Runs => "check_runs",
                Kind::Statuses => "statuses",
            })
            .and_then(Value::as_array)
            .ok_or(())?;
        let total = data
            .get("total_count")
            .and_then(Value::as_f64)
            .filter(|t| t.fract() == 0. && *t >= 0.)
            .ok_or(())?;
        items.extend(batch.iter().cloned());
        let more = (items.len() as f64) < total
            || response
                .link
                .as_deref()
                .is_some_and(|link| link.contains("rel=\"next\""));
        if !more {
            return Ok((items, true));
        }
        if batch.is_empty() {
            return Err(());
        }
    }
    Ok((items, false))
}

fn check(item: &Map<String, Value>, index: usize) -> Check {
    let is_run = item.contains_key("status");
    let text = |value: Option<&Value>| value.and_then(Value::as_str).map(str::to_string);
    let status = item.get("status").and_then(Value::as_str);
    let conclusion = item.get("conclusion").and_then(Value::as_str);
    let mut category = Category::Unknown;
    if is_run {
        if matches!(
            status,
            Some("queued" | "in_progress" | "waiting" | "pending" | "requested")
        ) {
            category = Category::Pending;
        } else if status == Some("completed") {
            category = match conclusion {
                Some("success") => Category::Successful,
                Some("cancelled") => Category::Cancelled,
                Some("skipped") => Category::Skipped,
                Some("neutral") => Category::Neutral,
                Some("failure" | "timed_out" | "action_required" | "stale" | "startup_failure") => {
                    Category::Failing
                }
                _ => Category::Unknown,
            };
        }
    } else if item.contains_key("state") {
        category = match item.get("state").and_then(Value::as_str) {
            Some("success") => Category::Successful,
            Some("pending") => Category::Pending,
            Some("failure" | "error") => Category::Failing,
            _ => Category::Unknown,
        };
    }
    let name = if is_run {
        text(item.get("name"))
    } else {
        text(item.get("context"))
    };
    let description = if is_run {
        item.get("output").and_then(|o| o.get("title"))
    } else {
        item.get("description")
    };
    let outcome = if is_run {
        if status == Some("completed") {
            conclusion
        } else {
            status
        }
    } else {
        item.get("state").and_then(Value::as_str)
    };
    Check {
        key: format!("check-{index}"),
        name: name.filter(|n| !n.trim().is_empty()).unwrap_or_else(|| {
            if is_run {
                "Unnamed check"
            } else {
                "Unnamed status"
            }
            .into()
        }),
        category,
        label: outcome
            .filter(|_| category != Category::Unknown)
            .and_then(result_label)
            .unwrap_or("Unknown")
            .to_string(),
        description: text(description).filter(|d| !d.trim().is_empty()),
        url: if is_run {
            safe_url(item.get("details_url")).or_else(|| safe_url(item.get("html_url")))
        } else {
            safe_url(item.get("target_url"))
        },
    }
}

/// Summarizes both sources' items.
fn summarize(runs: (Vec<Value>, bool), statuses: (Vec<Value>, bool)) -> Option<Summary> {
    let complete = runs.1 && statuses.1;
    let mut checks = Vec::new();
    for item in runs.0.iter().chain(statuses.0.iter()) {
        let item = item.as_object()?;
        checks.push(check(item, checks.len()));
    }
    let count = |category: Category| checks.iter().filter(|c| c.category == category).count();
    let total = checks.len();
    let description = if total > 0 {
        let parts: Vec<String> = Category::COUNTS
            .iter()
            .map(|c| (c, count(*c)))
            .filter(|(_, n)| *n > 0)
            .map(|(c, n)| format!("{n} {}", c.name()))
            .collect();
        format!(
            "{} {}",
            parts.join(", "),
            if total == 1 { "check" } else { "checks" }
        )
    } else {
        "No checks reported".to_string()
    };
    let state = if !complete {
        State::Unavailable
    } else if count(Category::Failing) + count(Category::Cancelled) > 0 {
        State::Failure
    } else if count(Category::Pending) > 0 {
        State::Pending
    } else if count(Category::Unknown) > 0 {
        State::Unavailable
    } else if count(Category::Successful) > 0 {
        State::Success
    } else {
        State::Neutral
    };
    let label = if !complete {
        "Incomplete"
    } else {
        match state {
            State::Failure => "Some not successful",
            State::Pending => "Pending",
            State::Success => "Successful",
            State::Unavailable => "Unknown",
            State::Neutral if total > 0 => "No failures",
            State::Neutral => "No checks",
        }
    };
    checks.sort_by_key(|c| c.category.priority());
    Some(Summary {
        checks: Some(checks),
        state,
        label: label.into(),
        description: format!(
            "{SCOPE}{}{description}.",
            if complete {
                ""
            } else {
                "Partial counts (page limit reached): "
            }
        ),
    })
}

/// Loads and summarizes the checks on `sha`. Any failure is
/// "Unavailable", never green.
pub async fn load(fetch: Arc<dyn Fetch>, repository: String, sha: String) -> Summary {
    let (runs, statuses) = futures::join!(
        read(&fetch, &repository, &sha, Kind::Runs),
        read(&fetch, &repository, &sha, Kind::Statuses)
    );
    match (runs, statuses) {
        (Ok(runs), Ok(statuses)) => summarize(runs, statuses).unwrap_or_else(unavailable),
        _ => unavailable(),
    }
}

#[cfg(test)]
mod tests {
    use super::super::client::testing::{Stub, json, linked, status};
    use super::super::client::{Fetched, Response};
    use super::*;
    use futures::executor::block_on;
    use serde_json::json;

    fn run(conclusion: Option<&str>, status: &str) -> Value {
        json!({ "status": status, "conclusion": conclusion })
    }

    fn done(conclusion: &str) -> Value {
        run(Some(conclusion), "completed")
    }

    fn fixtures(runs: Vec<Value>, statuses: Vec<Value>) -> Arc<Stub> {
        Stub::new(move |url| {
            if url.contains("check-runs") {
                json(json!({ "total_count": runs.len(), "check_runs": runs }))
            } else {
                json(
                    json!({ "total_count": statuses.len(), "state": "pending", "statuses": statuses }),
                )
            }
        })
    }

    fn summary(stub: Arc<Stub>) -> Summary {
        block_on(load(stub, "sample/project".into(), "head-sha".into()))
    }

    #[test]
    fn combines_runs_and_contexts_without_treating_empty_combined_pending_as_pending() {
        let stub = fixtures(
            vec![done("success"), done("success"), done("skipped")],
            vec![],
        );
        let result = summary(stub.clone());
        assert_eq!(result.state, State::Success);
        assert_eq!(result.label, "Successful");
        assert_eq!(
            result.description,
            "PR head commit · 1 skipped, 2 successful checks."
        );
        let calls = stub.calls.lock().unwrap().clone();
        assert_eq!(calls.len(), 2);
        assert!(calls.contains(&(
            "https://api.github.com/repos/sample/project/commits/head-sha/check-runs?per_page=100&page=1&filter=latest".into(),
            JSON
        )));
    }

    #[test]
    fn keeps_categorized_counts_and_prioritizes_failure() {
        let stub = fixtures(
            vec![
                done("failure"),
                done("cancelled"),
                done("skipped"),
                done("neutral"),
                run(None, "queued"),
                run(None, "in_progress"),
                done("success"),
            ],
            vec![
                json!({ "state": "error" }),
                json!({ "state": "pending" }),
                json!({ "state": "success" }),
            ],
        );
        let result = summary(stub);
        assert_eq!(result.state, State::Failure);
        assert_eq!(result.label, "Some not successful");
        assert_eq!(
            result.description,
            "PR head commit · 2 failing, 1 cancelled, 3 pending, 1 skipped, 1 neutral, 2 successful checks."
        );
    }

    #[test]
    fn does_not_mark_failures_successful() {
        for conclusion in [
            "failure",
            "timed_out",
            "action_required",
            "stale",
            "startup_failure",
            "cancelled",
        ] {
            assert_eq!(
                summary(fixtures(vec![done(conclusion)], vec![])).state,
                State::Failure,
                "{conclusion}"
            );
        }
    }

    #[test]
    fn represents_running_states_as_pending() {
        for status in ["queued", "in_progress", "waiting", "pending", "requested"] {
            let result = summary(fixtures(vec![run(None, status)], vec![]));
            assert_eq!(result.state, State::Pending);
            assert_eq!(result.description, "PR head commit · 1 pending check.");
        }
    }

    #[test]
    fn preserves_empty_neutral_and_unknown_results() {
        for (runs, state, label) in [
            (vec![], State::Neutral, "No checks"),
            (
                vec![done("skipped"), done("neutral")],
                State::Neutral,
                "No failures",
            ),
            (
                vec![run(None, "completed"), done("success")],
                State::Unavailable,
                "Unknown",
            ),
            (vec![done("new-conclusion")], State::Unavailable, "Unknown"),
            (vec![run(None, "new-status")], State::Unavailable, "Unknown"),
        ] {
            let result = summary(fixtures(runs, vec![]));
            assert_eq!((result.state, result.label.as_str()), (state, label));
        }
    }

    fn page_of(url: &str) -> usize {
        url.split("page=")
            .nth(2)
            .and_then(|p| p.split('&').next())
            .and_then(|p| p.parse().ok())
            .unwrap_or(1)
    }

    #[test]
    fn reads_later_pages_before_claiming_complete_counts() {
        let stub = Stub::new(|url| {
            let page = page_of(url);
            let runs = url.contains("check-runs");
            let item = if runs {
                done("success")
            } else {
                json!({ "state": "success" })
            };
            let items = vec![item; if page == 1 { 100 } else { 1 }];
            let mut body = json!({ "total_count": 101 });
            body[if runs { "check_runs" } else { "statuses" }] = json!(items);
            json(body)
        });
        let result = summary(stub.clone());
        assert_eq!(result.state, State::Success);
        assert_eq!(
            result.description,
            "PR head commit · 202 successful checks."
        );
        assert_eq!(stub.urls().len(), 4);
    }

    #[test]
    fn caps_reads_at_three_pages_and_labels_counts_partial() {
        let stub = Stub::new(|url| {
            if url.contains("check-runs") {
                json(json!({ "total_count": 301, "check_runs": vec![done("success"); 100] }))
            } else {
                json(json!({ "total_count": 0, "statuses": [] }))
            }
        });
        let result = summary(stub.clone());
        assert_eq!(result.state, State::Unavailable);
        assert_eq!(result.label, "Incomplete");
        assert_eq!(
            result.description,
            "PR head commit · Partial counts (page limit reached): 300 successful checks."
        );
        assert_eq!(stub.urls().len(), 4);
    }

    #[test]
    fn honors_next_page_evidence_over_totals() {
        let stub = Stub::new(|url| {
            if url.contains("check-runs") {
                let page = page_of(url);
                let body = json!({ "total_count": 1, "check_runs": [done(if page == 1 { "success" } else { "failure" })] });
                if page == 1 {
                    linked(
                        body,
                        "<https://api.github.com/example?page=2>; rel=\"next\"",
                    )
                } else {
                    json(body)
                }
            } else {
                json(json!({ "total_count": 0, "statuses": [] }))
            }
        });
        assert_eq!(summary(stub).state, State::Failure);
    }

    #[test]
    fn keeps_failed_reads_unavailable_not_green() {
        for failure in ["500", "network", "malformed", "empty-page", "invalid-item"] {
            let stub = Stub::new(move |url| -> Fetched {
                if !url.contains("check-runs") {
                    return json(json!({ "total_count": 1, "statuses": [{ "state": "success" }] }));
                }
                match failure {
                    "network" => Err("offline".into()),
                    "malformed" => json(json!({})),
                    "empty-page" => json(json!({ "total_count": 1, "check_runs": [] })),
                    "invalid-item" => json(json!({ "total_count": 1, "check_runs": [null] })),
                    _ => status(500),
                }
            });
            let result = summary(stub);
            assert_eq!(
                (result.state, result.label.as_str()),
                (State::Unavailable, "Unavailable"),
                "{failure}"
            );
        }
    }

    #[test]
    fn does_not_call_runs_a_success_when_statuses_fail() {
        let stub = Stub::new(|url| {
            if url.contains("check-runs") {
                json(json!({ "total_count": 1, "check_runs": [done("success")] }))
            } else {
                status(429)
            }
        });
        assert_eq!(summary(stub).label, "Unavailable");
    }

    #[test]
    fn retains_row_details_and_prioritizes_actionable_results() {
        let named = |value: Value, name: &str| {
            let mut value = value;
            value["name"] = json!(name);
            value
        };
        let mut typecheck = named(done("success"), "Typecheck");
        typecheck["details_url"] = json!("https://github.com/sample/project/actions/runs/1");
        typecheck["output"] = json!({ "title": "No errors" });
        let stub = fixtures(
            vec![
                typecheck,
                named(run(None, "in_progress"), "Browser tests"),
                named(done("cancelled"), "Cancelled build"),
                named(done("skipped"), "Optional deployment"),
                named(done("neutral"), "Advisory"),
                named(done("unexpected"), "Unrecognized result"),
            ],
            vec![
                json!({ "state": "failure", "context": "CI / build", "description": "Build needs attention", "target_url": "https://ci.example.test/build/1" }),
                json!({ "state": "success", "context": "DCO", "description": "All commits signed off" }),
            ],
        );
        let result = summary(stub.clone());
        let checks = result.checks.unwrap();
        let rows: Vec<_> = checks
            .iter()
            .map(|c| (c.name.as_str(), c.label.as_str()))
            .collect();
        assert_eq!(
            rows,
            [
                ("CI / build", "Failed"),
                ("Cancelled build", "Cancelled"),
                ("Browser tests", "In progress"),
                ("Unrecognized result", "Unknown"),
                ("Typecheck", "Passed"),
                ("DCO", "Passed"),
                ("Optional deployment", "Skipped"),
                ("Advisory", "Neutral"),
            ]
        );
        let typecheck = checks.iter().find(|c| c.name == "Typecheck").unwrap();
        assert_eq!(typecheck.description.as_deref(), Some("No errors"));
        assert_eq!(
            typecheck.url.as_deref(),
            Some("https://github.com/sample/project/actions/runs/1")
        );
        assert_eq!(
            checks[0].description.as_deref(),
            Some("Build needs attention")
        );
        assert_eq!(
            checks[0].url.as_deref(),
            Some("https://ci.example.test/build/1")
        );
        let keys: std::collections::HashSet<_> = checks.iter().map(|c| &c.key).collect();
        assert_eq!(keys.len(), 8);
        assert_eq!(stub.urls().len(), 2);
    }

    #[test]
    fn keeps_missing_metadata_readable_and_omits_unsafe_links() {
        let stub = fixtures(
            vec![
                json!({ "status": "completed", "conclusion": "success", "name": "", "details_url": "javascript:alert(1)", "html_url": "https://github.com/sample/project/actions/runs/1" }),
                json!({ "status": "completed", "conclusion": "success", "name": {}, "output": { "title": [] }, "details_url": "https://name:password@example.test/" }),
                json!({ "status": "completed", "conclusion": "toString", "details_url": "/relative" }),
            ],
            vec![
                json!({ "state": "success", "context": "Legacy", "description": 42, "target_url": "http://example.test/" }),
                json!({ "state": "success", "target_url": "data:text/html,test" }),
            ],
        );
        let result = summary(stub);
        assert_eq!(result.state, State::Unavailable);
        let checks = result.checks.unwrap();
        let names: Vec<_> = checks.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "Unnamed check",
                "Unnamed check",
                "Unnamed check",
                "Legacy",
                "Unnamed status"
            ]
        );
        let linked: Vec<_> = checks.iter().filter_map(|c| c.url.as_deref()).collect();
        assert_eq!(linked, ["https://github.com/sample/project/actions/runs/1"]);
        assert!(checks.iter().all(|c| c.description.is_none()));
        assert_eq!(checks[0].label, "Unknown");
    }

    #[test]
    fn summarizes_recorded_checks() {
        let stub = Stub::new(|url| {
            Ok(Response {
                status: 200,
                link: None,
                body: if url.contains("check-runs") {
                    include_bytes!("fixtures/runs.json").to_vec()
                } else {
                    include_bytes!("fixtures/status.json").to_vec()
                },
            })
        });
        let result = summary(stub);
        assert_eq!(result.state, State::Success);
        assert_eq!(
            result.description,
            "PR head commit · 1 skipped, 21 successful checks."
        );
    }
}
