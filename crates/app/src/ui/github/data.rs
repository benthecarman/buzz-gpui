//! A GitHub object's public details (`data.ts`).

use super::client::{FULL_JSON, Fetch, Response};
use super::references::{Kind, Reference};
use serde_json::Value;
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq)]
pub struct Branch {
    pub label: String,
    pub url: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum FactValue {
    Text(String),
    Changes {
        additions: String,
        deletions: String,
    },
    Branches {
        head: Branch,
        base: Branch,
    },
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Details {
    pub title: String,
    pub body: String,
    pub body_html: Option<String>,
    pub state: String,
    pub author: String,
    pub head_sha: Option<String>,
    pub author_url: Option<String>,
    pub author_avatar: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub merged_at: Option<String>,
    pub merged_by: Option<String>,
    pub merged_by_url: Option<String>,
    pub facts: Vec<(&'static str, FactValue)>,
}

pub const NOT_FOUND: &str =
    "This object is private or unavailable. Open it on GitHub to use your signed-in account.";
pub const RATE_LIMITED: &str =
    "GitHub’s public API limit was reached. Try again later or open it on GitHub.";
/// What WebKit's `response.json()` rejects with on a malformed body.
pub const UNREADABLE: &str = "The string did not match the expected pattern.";

/// A JSON value as React renders it in text: numbers without a trailing
/// `.0`, `null` as nothing.
pub fn display(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        Value::Number(number) => match number.as_f64() {
            Some(f) if f.fract() == 0. && f.abs() < 1e21 => format!("{}", f as i64),
            _ => number.to_string(),
        },
        other => other.to_string(),
    }
}

fn string<'a>(value: &'a Value, path: &[&str]) -> Option<&'a str> {
    path.iter()
        .try_fold(value, |value, key| value.get(key))?
        .as_str()
}

fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|n| n != 0.),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

/// `Number.isFinite(Date.parse(value))`, for GitHub's ISO timestamps.
pub fn valid_time(value: &str) -> bool {
    time(value).is_some()
}

/// Milliseconds since the epoch.
pub fn time(value: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|t| t.timestamp_millis())
}

/// Seconds since the epoch, for relative times.
pub fn seconds(value: &str) -> Option<u64> {
    time(value).map(|ms| (ms.max(0) / 1000) as u64)
}

fn login_url(login: &str) -> Option<String> {
    (!login.is_empty()
        && login
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-'))
    .then(|| format!("https://github.com/{login}"))
}

pub fn avatar(url: Option<&str>) -> Option<String> {
    url.filter(|url| url.starts_with("https://avatars.githubusercontent.com/"))
        .map(str::to_string)
}

fn branch_link(branch: &Value, pull_repository: &str) -> Branch {
    let repository = string(branch, &["repo", "full_name"]);
    let reference = string(branch, &["ref"]).filter(|r| !r.is_empty());
    let label = string(branch, &["label"]).unwrap_or_default();
    let valid = |r: &str| {
        let mut parts = r.split('/');
        let ok = |p: Option<&str>| {
            p.is_some_and(|p| {
                !p.is_empty()
                    && p.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
            })
        };
        ok(parts.next()) && ok(parts.next()) && parts.next().is_none()
    };
    Branch {
        label: match (repository, reference) {
            (Some(repository), Some(reference))
                if repository.eq_ignore_ascii_case(pull_repository) =>
            {
                reference.to_string()
            }
            _ => label.to_string(),
        },
        url: match (repository, reference) {
            (Some(repository), Some(reference)) if valid(repository) => Some(format!(
                "https://github.com/{repository}/tree/{}",
                reference
                    .split('/')
                    .map(super::client::encode_component)
                    .collect::<Vec<_>>()
                    .join("/")
            )),
            _ => None,
        },
    }
}

/// The request for `reference`.
pub fn endpoint(reference: &Reference) -> String {
    let id = reference
        .url
        .split('#')
        .next()
        .unwrap_or_default()
        .rsplit('/')
        .next()
        .unwrap_or_default();
    let surface = match reference.kind {
        Kind::Repository => String::new(),
        Kind::Pull => format!("/pulls/{id}"),
        Kind::Issue => format!("/issues/{id}"),
        Kind::Commit => format!("/commits/{id}"),
    };
    format!(
        "https://api.github.com/repos/{}{surface}",
        reference.repository
    )
}

/// Reads a details response.
pub fn details(reference: &Reference, response: &Response) -> Result<Details, String> {
    if !response.ok() {
        return Err(match response.status {
            404 => NOT_FOUND.to_string(),
            403 | 429 => RATE_LIMITED.to_string(),
            status => format!("GitHub couldn’t load this object ({status})."),
        });
    }
    let data: Value = serde_json::from_slice(&response.body).map_err(|_| UNREADABLE.to_string())?;
    let data = if data.is_object() {
        data
    } else {
        Value::Object(Default::default())
    };
    let pull = reference.kind == Kind::Pull;
    let mut facts = Vec::new();
    if let (Some(head), Some(base)) = (data.get("head"), data.get("base"))
        && truthy(Some(head))
        && truthy(Some(base))
    {
        facts.push((
            "Branch",
            FactValue::Branches {
                head: branch_link(head, &reference.repository),
                base: branch_link(base, &reference.repository),
            },
        ));
    }
    if let Some(files) = data.get("changed_files") {
        facts.push(("Files changed", FactValue::Text(display(files))));
    }
    if let (Some(additions), Some(deletions)) = (data.get("additions"), data.get("deletions")) {
        facts.push((
            "Changes",
            FactValue::Changes {
                additions: display(additions),
                deletions: display(deletions),
            },
        ));
    }
    if !pull && let Some(comments) = data.get("comments") {
        facts.push(("Comments", FactValue::Text(display(comments))));
    }
    if let Some(stars) = data.get("stargazers_count") {
        facts.push(("Stars", FactValue::Text(display(stars))));
    }
    if truthy(data.get("language")) {
        facts.push(("Language", FactValue::Text(display(&data["language"]))));
    }
    if truthy(data.get("default_branch")) {
        facts.push((
            "Default branch",
            FactValue::Text(display(&data["default_branch"])),
        ));
    }
    let message = string(&data, &["commit", "message"]);
    let mut lines = message.map(|m| m.split('\n'));
    let commit_title = lines.as_mut().and_then(Iterator::next);
    let commit_body = lines
        .map(|rest| rest.collect::<Vec<_>>().join("\n").trim().to_string())
        .unwrap_or_default();
    let commit_author = string(&data, &["commit", "author", "name"]);
    let user = string(&data, &["user", "login"]);
    let author = string(&data, &["author", "login"]);
    let owner = string(&data, &["owner", "login"]);
    let account = user
        .or(author)
        .or(if commit_author.is_none() { owner } else { None });
    let time_for = |key: &str, extra: bool| {
        string(&data, &[key])
            .filter(|t| pull && extra && valid_time(t))
            .map(str::to_string)
    };
    let merged = truthy(data.get("merged"));
    let merged_by = string(&data, &["merged_by", "login"]);
    let state = if merged {
        "Merged".to_string()
    } else if string(&data, &["state"]) == Some("closed") {
        "closed".to_string()
    } else if truthy(data.get("draft")) {
        "Draft".to_string()
    } else {
        string(&data, &["state"]).unwrap_or_default().to_string()
    };
    Ok(Details {
        title: string(&data, &["title"])
            .or(commit_title)
            .unwrap_or(&reference.repository)
            .to_string(),
        body: match data.get("body").filter(|b| !b.is_null()) {
            Some(body) => display(body),
            None => match data.get("description").filter(|d| !d.is_null()) {
                Some(description) => display(description),
                None => commit_body,
            },
        },
        body_html: string(&data, &["body_html"]).map(str::to_string),
        head_sha: string(&data, &["head", "sha"])
            .filter(|_| pull)
            .map(str::to_string),
        state,
        author: user
            .or(author)
            .or(commit_author)
            .or(owner)
            .unwrap_or_default()
            .to_string(),
        author_url: account.and_then(login_url),
        author_avatar: avatar(string(&data, &["user", "avatar_url"])),
        created_at: time_for("created_at", true),
        updated_at: time_for("updated_at", true),
        merged_at: time_for("merged_at", merged),
        merged_by: merged_by.map(str::to_string),
        merged_by_url: merged_by.and_then(login_url),
        facts,
    })
}

/// Loads `reference`'s details.
pub async fn load(fetch: Arc<dyn Fetch>, reference: Reference) -> Result<Details, String> {
    let response = fetch.get(endpoint(&reference), FULL_JSON).await?;
    details(&reference, &response)
}

#[cfg(test)]
mod tests {
    use super::super::client::testing::{Stub, json, status};
    use super::super::references::parse;
    use super::*;
    use futures::executor::block_on;
    use serde_json::json;

    fn pull() -> Reference {
        parse("https://github.com/block/buzz/pull/23#issuecomment-1").unwrap()
    }

    fn read(reference: &Reference, value: Value) -> Details {
        let stub = Stub::new(move |_| json(value.clone()));
        block_on(load(stub, reference.clone())).unwrap()
    }

    #[test]
    fn loads_pr_details_with_merged_state_branches_and_changes() {
        let value = json!({
            "title": "Simplify panels",
            "body": "An independent panel contract.",
            "body_html": "<p>An independent panel contract.</p>",
            "state": "closed",
            "merged": true,
            "merged_at": "2026-10-02T12:00:00Z",
            "merged_by": { "login": "maintainer" },
            "user": { "login": "author" },
            "head": { "label": "block:panels", "sha": "head-sha" },
            "base": { "label": "block:main" },
            "changed_files": 3,
            "comments": 5,
            "additions": 10,
            "deletions": 80,
        });
        let stub = Stub::new(move |_| json(value.clone()));
        let data = block_on(load(stub.clone(), pull())).unwrap();
        assert_eq!(
            *stub.calls.lock().unwrap(),
            vec![(
                "https://api.github.com/repos/block/buzz/pulls/23".to_string(),
                FULL_JSON
            )]
        );
        assert_eq!(data.title, "Simplify panels");
        assert_eq!(data.state, "Merged");
        assert_eq!(data.merged_at.as_deref(), Some("2026-10-02T12:00:00Z"));
        assert_eq!(data.merged_by.as_deref(), Some("maintainer"));
        assert_eq!(
            data.merged_by_url.as_deref(),
            Some("https://github.com/maintainer")
        );
        assert_eq!(data.author, "author");
        assert_eq!(data.body, "An independent panel contract.");
        assert_eq!(
            data.body_html.as_deref(),
            Some("<p>An independent panel contract.</p>")
        );
        assert_eq!(data.head_sha.as_deref(), Some("head-sha"));
        assert!(!data.facts.iter().any(|(label, _)| *label == "Comments"));
        assert!(data.facts.contains(&(
            "Changes",
            FactValue::Changes {
                additions: "10".into(),
                deletions: "80".into()
            }
        )));
    }

    #[test]
    fn explains_api_failures() {
        for (code, message) in [
            (404, "private or unavailable"),
            (403, "limit"),
            (429, "limit"),
            (500, "500"),
        ] {
            let stub = Stub::new(move |_| status(code));
            let error = block_on(load(stub, pull())).unwrap_err();
            assert!(error.contains(message), "{code}: {error}");
        }
    }

    #[test]
    fn does_not_expose_check_heads_on_non_prs() {
        let issue = parse("https://github.com/sample/project/issues/1").unwrap();
        let data = read(
            &issue,
            json!({ "title": "An issue", "head": { "sha": "not-a-pr" } }),
        );
        assert_eq!(data.head_sha, None);
    }

    #[test]
    fn shortens_branches_only_for_a_known_matching_repository() {
        for (repository, reference, label) in [
            (
                Some("block/buzz"),
                Some("tho/small-improvement"),
                "tho/small-improvement",
            ),
            (
                Some("BLOCK/Buzz"),
                Some("tho/small-improvement"),
                "tho/small-improvement",
            ),
            (
                Some("contributor/buzz"),
                Some("tho/small-improvement"),
                "block:tho/small-improvement",
            ),
            (
                Some("block/another-repo"),
                Some("tho/small-improvement"),
                "block:tho/small-improvement",
            ),
            (
                None,
                Some("tho/small-improvement"),
                "block:tho/small-improvement",
            ),
            (Some("block/buzz"), None, "block:tho/small-improvement"),
        ] {
            let mut head = json!({ "label": "block:tho/small-improvement" });
            if let Some(reference) = reference {
                head["ref"] = json!(reference);
            }
            head["repo"] = match repository {
                Some(name) => json!({ "full_name": name }),
                None => Value::Null,
            };
            let data = read(
                &pull(),
                json!({
                    "head": head,
                    "base": { "label": "block:main", "ref": "main", "repo": { "full_name": "block/buzz" } },
                }),
            );
            let url = repository.zip(reference).map(|(repository, reference)| {
                format!("https://github.com/{repository}/tree/{reference}")
            });
            assert!(
                data.facts.contains(&(
                    "Branch",
                    FactValue::Branches {
                        head: Branch {
                            label: label.into(),
                            url
                        },
                        base: Branch {
                            label: "main".into(),
                            url: Some("https://github.com/block/buzz/tree/main".into())
                        },
                    }
                )),
                "{repository:?} {reference:?}: {:?}",
                data.facts
            );
        }
    }

    #[test]
    fn encodes_branch_segments() {
        let data = read(
            &pull(),
            json!({
                "head": { "label": "contributor:fix/small detail", "ref": "fix/small detail", "repo": { "full_name": "contributor/project" } },
                "base": { "label": "example:main", "ref": "main", "repo": null },
            }),
        );
        let Some((_, FactValue::Branches { head, base })) = data.facts.first() else {
            panic!("missing branches")
        };
        assert_eq!(
            head.url.as_deref(),
            Some("https://github.com/contributor/project/tree/fix/small%20detail")
        );
        assert_eq!(head.label, "contributor:fix/small detail");
        assert_eq!(base.url, None);
        assert_eq!(base.label, "example:main");
    }

    #[test]
    fn uses_pr_creation_time_and_keeps_avatars_to_github() {
        let reference = parse("https://github.com/sample/project/pull/1").unwrap();
        let data = read(
            &reference,
            json!({
                "title": "A PR",
                "user": { "login": "author", "avatar_url": "https://avatars.githubusercontent.com/u/1?v=4" },
                "created_at": "2026-10-01T12:00:00Z",
                "updated_at": "2026-10-02T12:00:00Z",
            }),
        );
        assert_eq!(data.created_at.as_deref(), Some("2026-10-01T12:00:00Z"));
        assert_eq!(data.updated_at.as_deref(), Some("2026-10-02T12:00:00Z"));
        assert_eq!(
            data.author_avatar.as_deref(),
            Some("https://avatars.githubusercontent.com/u/1?v=4")
        );
        let other = read(
            &reference,
            json!({ "user": { "login": "a", "avatar_url": "https://other.test/a.png" } }),
        );
        assert_eq!(other.author_avatar, None);
    }

    #[test]
    fn keeps_comment_counts_on_issues() {
        let issue = parse("https://github.com/sample/project/issues/1").unwrap();
        let data = read(&issue, json!({ "comments": 0 }));
        assert!(
            data.facts
                .contains(&("Comments", FactValue::Text("0".into())))
        );
    }

    #[test]
    fn does_not_invent_merge_times() {
        for merged_at in [Value::Null, json!("bad"), json!(null)] {
            let mut value = json!({
                "merged": true,
                "updated_at": "2026-10-02T12:00:00Z",
                "merged_by": { "login": "bad/profile" },
            });
            if !merged_at.is_null() {
                value["merged_at"] = merged_at;
            }
            let data = read(&pull(), value);
            assert_eq!(data.state, "Merged");
            assert_eq!(data.merged_at, None);
            assert_eq!(data.merged_by_url, None);
        }
    }

    #[test]
    fn presents_terminal_states_before_draft() {
        for (state, draft, merged, label) in [
            ("open", false, false, "open"),
            ("open", true, false, "Draft"),
            ("closed", true, false, "closed"),
            ("closed", true, true, "Merged"),
        ] {
            let data = read(
                &pull(),
                json!({ "title": "A small improvement", "state": state, "draft": draft, "merged": merged }),
            );
            assert_eq!(data.state, label);
        }
    }

    #[test]
    fn reads_repositories_and_commits() {
        let repository = parse("https://github.com/block/buzz-app").unwrap();
        let data = details(
            &repository,
            &Response {
                status: 200,
                link: None,
                body: include_bytes!("fixtures/repo.json").to_vec(),
            },
        )
        .unwrap();
        assert_eq!(data.title, "block/buzz-app");
        assert_eq!(data.body, "A new desktop/web app experience for buzz");
        assert_eq!(data.author, "block");
        assert_eq!(data.author_url.as_deref(), Some("https://github.com/block"));
        let labels: Vec<_> = data.facts.iter().map(|(label, _)| *label).collect();
        assert_eq!(labels, ["Stars", "Language", "Default branch"]);

        let commit = parse(
            "https://github.com/block/buzz-app/commit/b48921f62fc2e26d12cc54b9234323c3f669e186",
        )
        .unwrap();
        assert_eq!(
            endpoint(&commit),
            "https://api.github.com/repos/block/buzz-app/commits/b48921f62fc2e26d12cc54b9234323c3f669e186"
        );
        let data = details(
            &commit,
            &Response {
                status: 200,
                link: None,
                body: include_bytes!("fixtures/commit.json").to_vec(),
            },
        )
        .unwrap();
        assert_eq!(
            data.title,
            "feat: add mobile pairing Settings plugin (#595)"
        );
        assert!(data.body.starts_with("Signed-off-by: klopez4212"));
        assert_eq!(data.author, "klopez4212");
        assert_eq!(data.state, "");
    }

    #[test]
    fn reads_a_recorded_pull_request() {
        let reference = parse("https://github.com/block/buzz-app/pull/595").unwrap();
        let data = details(
            &reference,
            &Response {
                status: 200,
                link: None,
                body: include_bytes!("fixtures/pull.json").to_vec(),
            },
        )
        .unwrap();
        assert_eq!(data.title, "feat: add mobile pairing Settings plugin");
        assert_eq!(data.state, "Merged");
        assert_eq!(
            data.head_sha.as_deref(),
            Some("01d9320e3f409dff8a27b4d009df1b2d5388c835")
        );
        let labels: Vec<_> = data.facts.iter().map(|(label, _)| *label).collect();
        assert_eq!(labels, ["Branch", "Files changed", "Changes"]);
        assert_eq!(
            data.facts[0].1,
            FactValue::Branches {
                head: Branch {
                    label: "kennylopez-mobile-pairing-plugin".into(),
                    url: Some(
                        "https://github.com/block/buzz-app/tree/kennylopez-mobile-pairing-plugin"
                            .into()
                    ),
                },
                base: Branch {
                    label: "main".into(),
                    url: Some("https://github.com/block/buzz-app/tree/main".into()),
                },
            }
        );
    }

    #[test]
    fn reads_a_recorded_issue_with_its_body() {
        let reference = parse("https://github.com/zed-industries/zed/issues/65416").unwrap();
        let data = details(
            &reference,
            &Response {
                status: 200,
                link: None,
                body: include_bytes!("fixtures/issue.json").to_vec(),
            },
        )
        .unwrap();
        assert_eq!(data.state, "open");
        assert_eq!(data.author, "XChikuX");
        assert_eq!(data.created_at, None, "only pull requests keep times");
        assert_eq!(data.facts, [("Comments", FactValue::Text("5".into()))]);
        let body =
            super::super::body::prepare(&data.body, data.body_html.as_deref(), &reference.url);
        let super::super::body::Body::Rich { blocks, metadata } = body else {
            panic!("expected Markdown")
        };
        assert!(!blocks.is_empty());
        let image =
            "https://github.com/user-attachments/assets/2046d390-4a31-4a89-a8ff-8cee258d3136";
        assert!(super::super::body::media(image, &metadata, true).is_some());
    }
}
