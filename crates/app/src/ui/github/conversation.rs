//! A pull request's conversation (`conversation.ts`): discussion comments
//! and submitted reviews, paged on request, merged into one timeline with
//! older stretches folded.

use super::client::{FULL_JSON, Fetch, Response};
use super::data::{self, Details};
use serde_json::Value;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

const PAGE_SIZE: u32 = 30;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Entry {
    pub id: i64,
    pub author: String,
    pub author_url: Option<String>,
    pub author_avatar: Option<String>,
    pub created_at: Option<String>,
    pub body: String,
    pub body_html: Option<String>,
    pub state: Option<String>,
}

impl Entry {
    /// The description, as the conversation's first message.
    pub fn description(details: &Details) -> Self {
        Self {
            id: 0,
            author: details.author.clone(),
            author_url: details.author_url.clone(),
            author_avatar: details.author_avatar.clone(),
            created_at: details.created_at.clone(),
            body: details.body.clone(),
            body_html: details.body_html.clone(),
            state: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Source {
    Discussion,
    Reviews,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Self::Discussion => "Discussion",
            Self::Reviews => "Reviews",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventKind {
    Discussion,
    Review,
    Merge,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    pub key: String,
    pub message: Entry,
    pub kind: EventKind,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    Event(Event),
    /// A folded stretch of the conversation.
    History {
        key: String,
        events: Vec<Event>,
        after_merge: bool,
    },
}

pub const UNEXPECTED_LINK: &str = "GitHub returned an unexpected next-page link.";

/// The page request for `source` of the pull request at `url`.
pub fn page_url(url: &str, source: Source, page: u32) -> Option<url::Url> {
    let parsed = url::Url::parse(url).ok()?;
    let path = parsed.path().replacen(
        "/pull/",
        match source {
            Source::Discussion => "/issues/",
            Source::Reviews => "/pulls/",
        },
        1,
    );
    let leaf = match source {
        Source::Reviews => "reviews",
        Source::Discussion => "comments",
    };
    url::Url::parse(&format!(
        "https://api.github.com/repos{path}/{leaf}?per_page={PAGE_SIZE}&page={page}"
    ))
    .ok()
}

/// `Number(value)`: whitespace-trimmed decimal, empty as 0.
fn number(value: Option<&str>) -> f64 {
    let value = value.unwrap_or("").trim();
    if value.is_empty() {
        return 0.;
    }
    value.parse().unwrap_or(f64::NAN)
}

/// The next page from a `Link` header. GitHub canonicalizes links to
/// repository ids; only the cursor is read, and later requests still use
/// the trusted owner/name endpoint.
pub fn next_page(link: Option<&str>, request: &url::Url, page: u32) -> Result<Option<u32>, String> {
    let Some(next) = link.and_then(|link| {
        link.split(',').find(|part| {
            part.split_once(';').is_some_and(|(_, params)| {
                params
                    .split(';')
                    .any(|param| param.trim() == "rel=\"next\"")
            })
        })
    }) else {
        return Ok(None);
    };
    let target = next
        .trim_start()
        .strip_prefix('<')
        .and_then(|rest| rest.split_once('>'))
        .map(|(target, _)| target)
        .filter(|target| !target.is_empty())
        .ok_or("GitHub returned an unreadable next-page link.")?;
    let parsed = url::Url::parse(target).map_err(|_| UNEXPECTED_LINK.to_string())?;
    let pages: Vec<String> = parsed
        .query_pairs()
        .filter(|(key, _)| key == "page")
        .map(|(_, value)| value.into_owned())
        .collect();
    let candidate = number(pages.first().map(String::as_str));
    let trusted: String = request
        .path()
        .split('/')
        .take(4)
        .collect::<Vec<_>>()
        .join("/");
    let canonical = match parsed.path().strip_prefix("/repositories/") {
        Some(rest) => match rest.split_once('/') {
            Some((id, tail))
                if id
                    .as_bytes()
                    .first()
                    .is_some_and(|b| (b'1'..=b'9').contains(b))
                    && id.bytes().all(|b| b.is_ascii_digit()) =>
            {
                format!("{trusted}/{tail}")
            }
            _ => parsed.path().to_string(),
        },
        None => parsed.path().to_string(),
    };
    if parsed.origin() != request.origin()
        || canonical != request.path()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.fragment().is_some_and(|f| !f.is_empty())
        || pages.len() != 1
        || candidate != f64::from(page + 1)
    {
        return Err(UNEXPECTED_LINK.to_string());
    }
    Ok(Some(page + 1))
}

fn safe_integer(value: Option<&Value>) -> Option<i64> {
    let number = value?.as_f64()?;
    (number.fract() == 0. && number.abs() <= 9_007_199_254_740_991.).then_some(number as i64)
}

/// Reads one page of `source`.
pub fn read_page(
    source: Source,
    request: &url::Url,
    page: u32,
    response: &Response,
) -> Result<(Vec<Entry>, Option<u32>), String> {
    if !response.ok() {
        return Err(match response.status {
            403 | 429 => "GitHub’s public API limit was reached. Try again later.".to_string(),
            status => format!("GitHub couldn’t load this source ({status})."),
        });
    }
    let data: Value =
        serde_json::from_slice(&response.body).map_err(|_| data::UNREADABLE.to_string())?;
    let Value::Array(items) = data else {
        return Err("GitHub returned an unreadable conversation page.".to_string());
    };
    let entries = items
        .iter()
        .filter_map(|item| {
            let id = safe_integer(item.get("id"))?;
            let submitted = item.get("submitted_at").and_then(Value::as_str);
            if source == Source::Reviews && submitted.is_none_or(str::is_empty) {
                return None;
            }
            let author = item
                .get("user")
                .and_then(|u| u.get("login"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let date = match source {
                Source::Reviews => submitted,
                Source::Discussion => item.get("created_at").and_then(Value::as_str),
            };
            Some(Entry {
                id,
                author_url: (!author.is_empty()
                    && author
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-'))
                .then(|| format!("https://github.com/{author}")),
                author,
                author_avatar: data::avatar(
                    item.get("user")
                        .and_then(|u| u.get("avatar_url"))
                        .and_then(Value::as_str),
                ),
                created_at: date.filter(|d| data::valid_time(d)).map(str::to_string),
                body: item
                    .get("body")
                    .filter(|b| !b.is_null())
                    .map(data::display)
                    .unwrap_or_default(),
                body_html: item
                    .get("body_html")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                state: item
                    .get("state")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            })
        })
        .collect();
    let next = next_page(response.link.as_deref(), request, page)?;
    Ok((entries, next))
}

/// Loads one page of `source` for the pull request at `url`.
pub async fn load_page(
    fetch: Arc<dyn Fetch>,
    url: String,
    source: Source,
    page: u32,
) -> Result<(Vec<Entry>, Option<u32>), String> {
    let request = page_url(&url, source, page).ok_or(UNEXPECTED_LINK)?;
    let response = fetch.get(request.to_string(), FULL_JSON).await?;
    read_page(source, &request, page, &response)
}

/// One source's pages, cursor and recovery state (`useConversationSource`).
#[derive(Clone, Debug, PartialEq)]
pub struct SourceState {
    pub entries: Vec<Entry>,
    pub next: Option<u32>,
    pub loading: bool,
    pub error: Option<String>,
    /// The page the current or last request asked for.
    pub page: u32,
}

impl Default for SourceState {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            next: None,
            loading: true,
            error: None,
            page: 1,
        }
    }
}

impl SourceState {
    /// Starts a request; `None` while one is running.
    pub fn begin(&mut self, page: u32) -> Option<u32> {
        if self.loading && self.page != page {
            return None;
        }
        self.loading = true;
        self.page = page;
        Some(page)
    }

    /// "Load more": the next page, when there is one and nothing runs.
    pub fn load_more(&mut self) -> Option<u32> {
        if self.loading {
            return None;
        }
        let next = self.next?;
        self.begin(next)
    }

    /// "Retry": the same page again, when nothing runs.
    pub fn retry(&mut self) -> Option<u32> {
        if self.loading {
            return None;
        }
        self.begin(self.page)
    }

    /// Applies a finished request. Pages merge by id: an entry keeps its
    /// first position and takes its latest content.
    pub fn finish(&mut self, result: Result<(Vec<Entry>, Option<u32>), String>) {
        self.loading = false;
        match result {
            Ok((entries, next)) => {
                let mut index: HashMap<i64, usize> = self
                    .entries
                    .iter()
                    .enumerate()
                    .map(|(i, e)| (e.id, i))
                    .collect();
                for entry in entries {
                    match index.get(&entry.id) {
                        Some(&at) => self.entries[at] = entry,
                        None => {
                            index.insert(entry.id, self.entries.len());
                            self.entries.push(entry);
                        }
                    }
                }
                self.next = next;
                self.error = None;
            }
            Err(error) => self.error = Some(error),
        }
    }

    /// Still loading, failed, or with more to load.
    pub fn incomplete(&self) -> bool {
        self.loading || self.error.is_some() || self.next.is_some()
    }
}

fn millis(entry: &Entry) -> Option<i64> {
    entry.created_at.as_deref().and_then(data::time)
}

fn chronological(a: &Entry, b: &Entry) -> Ordering {
    millis(a)
        .unwrap_or(0)
        .cmp(&millis(b).unwrap_or(0))
        .then(a.id.cmp(&b.id))
}

/// The timeline: the merge, discussion comments and reviews in time
/// order. Comment-only reviews with no summary are left out; their
/// inline code threads stay on GitHub.
pub fn events(discussion: &[Entry], reviews: &[Entry], details: Option<&Details>) -> Vec<Event> {
    let mut events = Vec::new();
    if let Some(details) = details
        && let Some(merged_at) = &details.merged_at
    {
        events.push(Event {
            key: "merge".into(),
            kind: EventKind::Merge,
            message: Entry {
                id: 0,
                author: details.merged_by.clone().unwrap_or_default(),
                author_url: details.merged_by_url.clone(),
                created_at: Some(merged_at.clone()),
                ..Entry::default()
            },
        });
    }
    events.extend(discussion.iter().map(|message| Event {
        key: format!("discussion-{}", message.id),
        message: message.clone(),
        kind: EventKind::Discussion,
    }));
    events.extend(
        reviews
            .iter()
            .filter(|m| m.state.as_deref() != Some("COMMENTED") || !m.body.trim().is_empty())
            .map(|message| Event {
                key: format!("review-{}", message.id),
                message: message.clone(),
                kind: EventKind::Review,
            }),
    );
    events.sort_by(|a, b| chronological(&a.message, &b.message).then_with(|| a.key.cmp(&b.key)));
    events
}

/// `a < b` where a missing time is `NaN`.
fn before(a: Option<i64>, b: Option<i64>) -> bool {
    matches!((a, b), (Some(a), Some(b)) if a < b)
}

/// Keeps milestones visible and folds conversation stretches, never
/// across the merge.
pub fn group(events: &[Event]) -> Vec<Item> {
    let merge = events.iter().find(|e| e.kind == EventKind::Merge);
    let merged_at = merge.and_then(|m| millis(&m.message));
    let mut latest: HashMap<String, i64> = HashMap::new();
    let mut superseded: HashSet<&str> = HashSet::new();
    for event in events.iter().rev() {
        // Dismissals are review history too; comments and unknown states
        // never replace a verdict.
        let decision = event.kind == EventKind::Review
            && matches!(
                event.message.state.as_deref(),
                Some("APPROVED" | "CHANGES_REQUESTED" | "DISMISSED")
            );
        if !decision {
            continue;
        }
        let Some(time) = millis(&event.message) else {
            continue;
        };
        let author = event.message.author.to_lowercase();
        let later = latest.get(&author).copied().unwrap_or(time);
        if before(Some(time), merged_at) || (!author.is_empty() && time < later) {
            superseded.insert(&event.key);
        }
        if !author.is_empty() {
            latest.insert(author, time.max(later));
        }
    }
    let mut grouped = Vec::new();
    let mut run: Vec<Event> = Vec::new();
    let mut after_merge = false;
    let flush = |run: &mut Vec<Event>, after_merge: bool, grouped: &mut Vec<Item>| {
        let approvals_only = run.iter().all(|event| {
            event.kind == EventKind::Review
                && event.message.state.as_deref() == Some("APPROVED")
                && before(millis(&event.message), merged_at)
        });
        let fold = run.first().is_some_and(|first| {
            !approvals_only
                && (run.len() > 1 || superseded.contains(first.key.as_str()) || after_merge)
        });
        if fold {
            grouped.push(Item::History {
                key: run[0].key.clone(),
                events: std::mem::take(run),
                after_merge,
            });
        } else {
            grouped.extend(run.drain(..).map(Item::Event));
        }
    };
    for (index, event) in events.iter().enumerate() {
        let comment = event.kind == EventKind::Discussion
            || (event.kind == EventKind::Review
                && event.message.state.as_deref() == Some("COMMENTED"));
        let post_merge = comment && before(merged_at, millis(&event.message));
        if post_merge
            || (index + 1 < events.len() && (comment || superseded.contains(event.key.as_str())))
        {
            if !run.is_empty() && after_merge != post_merge {
                flush(&mut run, after_merge, &mut grouped);
            }
            after_merge = post_merge;
            run.push(event.clone());
        } else {
            flush(&mut run, after_merge, &mut grouped);
            grouped.push(Item::Event(event.clone()));
        }
    }
    flush(&mut run, after_merge, &mut grouped);
    grouped
}

#[cfg(test)]
mod tests {
    use super::super::client::testing::{Stub, json, linked, status};
    use super::*;
    use futures::executor::block_on;
    use serde_json::json;

    const URL: &str = "https://github.com/sample/project/pull/1";

    fn date(hour: u32) -> String {
        format!("2026-10-01T{hour}:00:00Z")
    }

    fn entry(id: i64) -> Entry {
        Entry {
            id,
            author: "sample".into(),
            created_at: Some(date(12)),
            ..Entry::default()
        }
    }

    fn at(id: i64, hour: u32) -> Entry {
        Entry {
            created_at: Some(date(hour)),
            ..entry(id)
        }
    }

    fn review(id: i64, state: &str, hour: u32) -> Entry {
        Entry {
            state: Some(state.into()),
            ..at(id, hour)
        }
    }

    fn by(author: &str, entry: Entry) -> Entry {
        Entry {
            author: author.into(),
            ..entry
        }
    }

    fn untimed(entry: Entry) -> Entry {
        Entry {
            created_at: None,
            ..entry
        }
    }

    fn details() -> Details {
        Details {
            title: "A change".into(),
            state: "open".into(),
            author: "author".into(),
            author_url: Some("https://github.com/author".into()),
            created_at: Some("2026-10-01T12:00:00Z".into()),
            body: "Summary\n\n## Full description".into(),
            ..Details::default()
        }
    }

    fn merged(hour: u32) -> Details {
        Details {
            merged_at: Some(date(hour)),
            ..details()
        }
    }

    fn history(events: &[Event]) -> Item {
        Item::History {
            key: events[0].key.clone(),
            events: events.to_vec(),
            after_merge: false,
        }
    }

    fn after(events: &[Event]) -> Item {
        Item::History {
            key: events[0].key.clone(),
            events: events.to_vec(),
            after_merge: true,
        }
    }

    fn plain(events: &[Event]) -> Vec<Item> {
        events.iter().cloned().map(Item::Event).collect()
    }

    fn page(
        source: Source,
        page: u32,
        stub: Arc<Stub>,
    ) -> Result<(Vec<Entry>, Option<u32>), String> {
        block_on(load_page(stub, URL.into(), source, page))
    }

    #[test]
    fn loads_bounded_sources_and_honors_paging_without_following_destinations() {
        let stub = Stub::new(|_| {
            linked(
                json!([
                    { "id": 1, "user": { "login": "reviewer" }, "state": "APPROVED", "submitted_at": date(13), "body": "" },
                    { "id": 2, "state": "PENDING" },
                ]),
                "<https://api.github.com/repos/sample/project/pulls/1/reviews?per_page=30&page=2>; rel=\"next\"",
            )
        });
        let (entries, next) = page(Source::Reviews, 1, stub.clone()).unwrap();
        assert_eq!(next, Some(2));
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, 1);
        assert_eq!(entries[0].created_at, Some(date(13)));
        assert_eq!(entries[0].state.as_deref(), Some("APPROVED"));
        assert_eq!(entries[0].body, "");
        assert_eq!(
            *stub.calls.lock().unwrap(),
            vec![(
                "https://api.github.com/repos/sample/project/pulls/1/reviews?per_page=30&page=1"
                    .to_string(),
                FULL_JSON
            )]
        );
        let stub = Stub::new(|_| {
            linked(
                json!([]),
                "<https://other.test/private?page=2>; rel=\"next\"",
            )
        });
        let error = page(Source::Reviews, 1, stub.clone()).unwrap_err();
        assert!(error.contains("unexpected next-page"));
        assert_eq!(stub.urls().len(), 1);
    }

    #[test]
    fn accepts_repository_id_paging_and_rebuilds_the_trusted_request() {
        for (source, path) in [
            (Source::Discussion, "issues/1/comments"),
            (Source::Reviews, "pulls/1/reviews"),
        ] {
            let first = json!({ "id": 1, "created_at": date(13), "submitted_at": date(13), "body": "First page" });
            let link = format!(
                "<https://api.github.com/repositories/123456/{path}?per_page=30&page=2>; rel=\"next\", <https://api.github.com/repositories/123456/{path}?per_page=30&page=4>; rel=\"last\""
            );
            let stub = Stub::new(move |target| {
                if target.contains("page=2") {
                    let mut second = first.clone();
                    second["id"] = json!(2);
                    second["body"] = json!("Second page");
                    json(json!([second]))
                } else {
                    linked(json!([first.clone()]), &link)
                }
            });
            let (entries, next) = page(source, 1, stub.clone()).unwrap();
            assert_eq!(entries.len(), 1);
            assert_eq!(next, Some(2));
            let (entries, next) = page(source, 2, stub.clone()).unwrap();
            assert_eq!(entries[0].body, "Second page");
            assert_eq!(next, None);
            assert_eq!(
                stub.urls(),
                vec![
                    format!(
                        "https://api.github.com/repos/sample/project/{path}?per_page=30&page=1"
                    ),
                    format!(
                        "https://api.github.com/repos/sample/project/{path}?per_page=30&page=2"
                    ),
                ]
            );
        }
    }

    #[test]
    fn rejects_unexpected_pagination() {
        for source in [Source::Discussion, Source::Reviews] {
            let path = match source {
                Source::Reviews => "pulls/1/reviews",
                Source::Discussion => "issues/1/comments",
            };
            let other = match source {
                Source::Reviews => "issues/1/comments",
                Source::Discussion => "pulls/1/reviews",
            };
            let valid = format!("https://api.github.com/repositories/123/{path}?page=2");
            let links = [
                valid.replace("api.github.com", "other.test"),
                valid.replace("https:", "http:"),
                valid.replace("repositories/123", "repos/other/project"),
                valid.replace("repositories/123", "repositories/abc"),
                valid.replace("/1/", "/2/"),
                valid.replace(path, other),
                valid.replace("page=2", "page=1"),
                valid.replace("page=2", "page=3"),
                format!("{valid}&page=3"),
                format!("{valid}#fragment"),
                valid.replace("api.github.com", "user@api.github.com"),
            ];
            for link in links {
                let header = format!("<{link}>; rel=\"next\"");
                let stub = Stub::new(move |_| linked(json!([]), &header));
                let error = page(source, 1, stub).unwrap_err();
                assert!(error.contains("unexpected next-page"), "{link}");
            }
            assert_eq!(
                next_page(
                    Some(&format!("<{valid}>; rel=\"next\"")),
                    &page_url(URL, source, 1).unwrap(),
                    1
                ),
                Ok(Some(2))
            );
        }
    }

    #[test]
    fn explains_source_failures() {
        for (code, message) in [(403, "limit"), (429, "limit"), (500, "(500)")] {
            let error = page(Source::Discussion, 1, Stub::new(move |_| status(code))).unwrap_err();
            assert!(error.contains(message), "{error}");
        }
        let error = page(Source::Discussion, 1, Stub::new(|_| json(json!({})))).unwrap_err();
        assert_eq!(error, "GitHub returned an unreadable conversation page.");
    }

    #[test]
    fn uses_discussion_creation_time_and_omits_unsafe_avatars() {
        let stub = Stub::new(|_| {
            json(json!([
                { "id": 4, "created_at": "bad", "user": { "login": "sample", "avatar_url": "https://other.test/avatar" }, "body": null },
                { "id": 5, "created_at": date(13), "user": { "login": "author", "avatar_url": "https://avatars.githubusercontent.com/u/1" }, "body": "Comment" },
            ]))
        });
        let (entries, _) = page(Source::Discussion, 1, stub.clone()).unwrap();
        assert_eq!(
            stub.urls()[0],
            "https://api.github.com/repos/sample/project/issues/1/comments?per_page=30&page=1"
        );
        assert_eq!(entries[0].created_at, None);
        assert_eq!(entries[0].author_avatar, None);
        assert_eq!(entries[0].body, "");
        assert_eq!(entries[1].created_at, Some(date(13)));
        assert_eq!(
            entries[1].author_avatar.as_deref(),
            Some("https://avatars.githubusercontent.com/u/1")
        );
        assert_eq!(entries[1].body, "Comment");
    }

    #[test]
    fn reads_recorded_pages() {
        let request = page_url(
            "https://github.com/block/buzz-app/pull/595",
            Source::Reviews,
            1,
        )
        .unwrap();
        let response = Response {
            status: 200,
            link: Some("<https://api.github.com/repositories/1364527927/pulls/595/reviews?per_page=30&page=2>; rel=\"next\", <https://api.github.com/repositories/1364527927/pulls/595/reviews?per_page=30&page=2>; rel=\"last\"".into()),
            body: include_bytes!("fixtures/reviews.json").to_vec(),
        };
        let (reviews, next) = read_page(Source::Reviews, &request, 1, &response).unwrap();
        assert_eq!(reviews.len(), 30);
        assert_eq!(next, Some(2));
        let request = page_url(
            "https://github.com/block/buzz-app/pull/595",
            Source::Discussion,
            1,
        )
        .unwrap();
        let response = Response {
            status: 200,
            link: None,
            body: include_bytes!("fixtures/comments.json").to_vec(),
        };
        let (comments, next) = read_page(Source::Discussion, &request, 1, &response).unwrap();
        assert_eq!(comments.len(), 4);
        assert_eq!(next, None);
        let timeline = events(&comments, &reviews, None);
        // Empty comment-only reviews are left out.
        assert_eq!(timeline.len(), 4 + 11);
        // The last page links back, not on.
        let request = page_url(
            "https://github.com/block/buzz-app/pull/595",
            Source::Reviews,
            2,
        )
        .unwrap();
        let response = Response {
            status: 200,
            link: Some("<https://api.github.com/repositories/1364527927/pulls/595/reviews?per_page=30&page=1>; rel=\"prev\", <https://api.github.com/repositories/1364527927/pulls/595/reviews?per_page=30&page=1>; rel=\"first\"".into()),
            body: include_bytes!("fixtures/reviews2.json").to_vec(),
        };
        let (more, next) = read_page(Source::Reviews, &request, 2, &response).unwrap();
        assert_eq!(more.len(), 5);
        assert_eq!(next, None);
        let mut state = SourceState::default();
        state.finish(Ok((reviews, Some(2))));
        assert_eq!(state.load_more(), Some(2));
        state.finish(Ok((more, None)));
        assert_eq!(state.entries.len(), 35);
        assert!(!state.incomplete());
    }

    #[test]
    fn source_state_merges_pages_and_recovers() {
        let mut state = SourceState::default();
        assert_eq!(state.load_more(), None);
        state.finish(Ok((vec![entry(1), entry(2)], Some(2))));
        assert!(state.incomplete());
        assert_eq!(state.load_more(), Some(2));
        assert_eq!(state.retry(), None, "busy");
        state.finish(Err("GitHub couldn’t load this source (500).".into()));
        assert_eq!(state.entries.len(), 2, "pages stay during a failure");
        assert_eq!(state.next, Some(2));
        assert_eq!(state.retry(), Some(2));
        let mut updated = entry(2);
        updated.body = "Edited".into();
        state.finish(Ok((vec![updated, entry(3)], None)));
        assert_eq!(
            state.entries.iter().map(|e| e.id).collect::<Vec<_>>(),
            [1, 2, 3]
        );
        assert_eq!(state.entries[1].body, "Edited");
        assert_eq!(state.error, None);
        assert!(!state.incomplete());
    }

    #[test]
    fn orders_discussion_and_reviews_with_deterministic_ties() {
        let events = events(&[at(20, 13), at(21, 14)], &[at(10, 14), at(22, 14)], None);
        let keys: Vec<_> = events.iter().map(|e| e.key.as_str()).collect();
        assert_eq!(
            keys,
            ["discussion-20", "review-10", "discussion-21", "review-22"]
        );
    }

    #[test]
    fn omits_only_empty_comment_only_reviews() {
        let with_body = |id, state: &str, body: &str| Entry {
            state: Some(state.into()),
            body: body.into(),
            ..entry(id)
        };
        let reviews = [
            with_body(10, "COMMENTED", ""),
            with_body(11, "COMMENTED", " \n\t "),
            with_body(12, "COMMENTED", "Review summary"),
            with_body(
                13,
                "COMMENTED",
                "![Evidence](https://raw.githubusercontent.com/sample/project/main/preview.png)",
            ),
            with_body(14, "APPROVED", ""),
            with_body(15, "CHANGES_REQUESTED", ""),
            with_body(16, "DISMISSED", ""),
            with_body(17, "UNKNOWN", ""),
        ];
        let events = events(&[entry(20)], &reviews, None);
        let keys: Vec<_> = events.iter().map(|e| e.key.as_str()).collect();
        assert_eq!(
            keys,
            [
                "review-12",
                "review-13",
                "review-14",
                "review-15",
                "review-16",
                "review-17",
                "discussion-20"
            ]
        );
    }

    #[test]
    fn folds_only_consecutive_older_comments() {
        let events = events(
            &[at(1, 12), at(3, 14), at(8, 19), at(9, 20)],
            &[
                Entry {
                    body: "A review".into(),
                    ..review(2, "COMMENTED", 13)
                },
                review(4, "APPROVED", 15),
                by("another-reviewer", review(5, "CHANGES_REQUESTED", 16)),
                by("dismissed-reviewer", review(6, "DISMISSED", 17)),
                review(7, "UNKNOWN", 18),
            ],
            None,
        );
        let grouped = group(&events);
        let kinds: Vec<_> = grouped
            .iter()
            .map(|item| match item {
                Item::History { .. } => "history",
                Item::Event(e) => match e.kind {
                    EventKind::Review => "review",
                    EventKind::Discussion => "discussion",
                    EventKind::Merge => "merge",
                },
            })
            .collect();
        assert_eq!(
            kinds,
            [
                "history",
                "review",
                "review",
                "review",
                "review",
                "discussion",
                "discussion"
            ]
        );
        assert_eq!(grouped[0], history(&events[0..3]));
        assert_eq!(
            grouped.last(),
            Some(&Item::Event(events.last().unwrap().clone()))
        );
        let comments: Vec<Event> = events
            .iter()
            .filter(|e| e.kind == EventKind::Discussion)
            .cloned()
            .collect();
        assert_eq!(
            group(&comments),
            vec![
                history(&comments[..comments.len() - 1]),
                Item::Event(comments.last().unwrap().clone())
            ]
        );
        assert_eq!(group(&[]), vec![]);
        assert_eq!(group(&comments[..1]), plain(&comments[..1]));
    }

    #[test]
    fn places_merge_at_its_real_time() {
        let details = Details {
            merged_at: Some(date(17)),
            merged_by: Some("maintainer".into()),
            merged_by_url: Some("https://github.com/maintainer".into()),
            ..details()
        };
        let events = events(
            &[at(1, 13), at(2, 18)],
            &[review(3, "APPROVED", 14)],
            Some(&details),
        );
        let keys: Vec<_> = events.iter().map(|e| e.key.as_str()).collect();
        assert_eq!(keys, ["discussion-1", "review-3", "merge", "discussion-2"]);
        assert_eq!(events[2].message.author, "maintainer");
        assert_eq!(events[2].message.created_at, Some(date(17)));
        assert_eq!(
            events[2].message.author_url.as_deref(),
            Some("https://github.com/maintainer")
        );
        assert_eq!(super::events(&[], &[], Some(&self::details())), vec![]);
        assert_eq!(
            super::events(&[entry(1)], &[], Some(&merged(17)))
                .last()
                .map(|e| e.kind),
            Some(EventKind::Merge)
        );
    }

    #[test]
    fn folds_pre_merge_and_post_merge_separately() {
        let events = events(
            &[at(2, 13), at(5, 19)],
            &[
                review(1, "CHANGES_REQUESTED", 12),
                review(3, "APPROVED", 14),
                review(4, "CHANGES_REQUESTED", 18),
            ],
            Some(&merged(17)),
        );
        let mut expected = vec![history(&events[0..3])];
        expected.extend(plain(&events[3..5]));
        expected.push(after(&events[5..6]));
        assert_eq!(group(&events), expected);
        let merge_only = super::events(&[], &[], Some(&merged(17)));
        assert_eq!(group(&merge_only), plain(&merge_only));
    }

    #[test]
    fn keeps_pre_merge_approval_only_stretches_visible() {
        for reviews in [
            vec![review(1, "APPROVED", 13)],
            vec![
                review(1, "APPROVED", 13),
                by("another", review(2, "APPROVED", 14)),
            ],
        ] {
            let events = events(&[], &reviews, Some(&merged(17)));
            assert_eq!(group(&events), plain(&events));
        }
        let request = events(
            &[],
            &[review(1, "CHANGES_REQUESTED", 13)],
            Some(&merged(17)),
        );
        assert_eq!(
            group(&request),
            vec![history(&request[0..1]), Item::Event(request[1].clone())]
        );
        let comments = events(&[entry(1)], &[], Some(&merged(17)));
        assert_eq!(group(&comments), plain(&comments));
    }

    #[test]
    fn keeps_approval_milestones_around_folded_stretches() {
        let events = events(
            &[at(2, 13), at(3, 14)],
            &[
                review(1, "APPROVED", 12),
                review(4, "APPROVED", 15),
                review(5, "UNKNOWN", 16),
                review(6, "APPROVED", 17),
            ],
            Some(&merged(18)),
        );
        let mut expected = vec![history(&events[0..4])];
        expected.extend(plain(&events[4..]));
        assert_eq!(group(&events), expected);
    }

    #[test]
    fn folds_the_newest_post_merge_comment_but_not_unknown_times_or_verdicts() {
        for discussion in [true, false] {
            let comment = Entry {
                state: Some("COMMENTED".into()),
                body: "Follow-up".into(),
                ..at(1, 18)
            };
            let events = if discussion {
                events(&[comment], &[], Some(&merged(17)))
            } else {
                events(&[], &[comment], Some(&merged(17)))
            };
            assert_eq!(
                group(&events),
                vec![Item::Event(events[0].clone()), after(&events[1..2])]
            );
        }
        let events = events(
            &[at(1, 18), at(2, 19), untimed(entry(4))],
            &[review(3, "CHANGES_REQUESTED", 20)],
            Some(&merged(17)),
        );
        assert_eq!(
            group(&events),
            vec![
                Item::Event(events[0].clone()),
                Item::Event(events[1].clone()),
                after(&events[2..4]),
                Item::Event(events[4].clone()),
            ]
        );
    }

    #[test]
    fn keeps_each_reviewers_latest_verdict() {
        let events = events(
            &[at(6, 18)],
            &[
                by("Alice", review(1, "CHANGES_REQUESTED", 12)),
                by("Bob", review(2, "CHANGES_REQUESTED", 13)),
                by("alice", review(3, "APPROVED", 14)),
                Entry {
                    body: "Still checking".into(),
                    ..by("Bob", review(4, "COMMENTED", 15))
                },
                by("Carol", review(5, "APPROVED", 16)),
            ],
            None,
        );
        let mut expected = vec![history(&events[0..1])];
        expected.extend(plain(&events[1..]));
        assert_eq!(group(&events), expected);
        let revised = super::events(
            &[],
            &[
                review(1, "APPROVED", 13),
                review(2, "CHANGES_REQUESTED", 14),
            ],
            None,
        );
        assert_eq!(
            group(&revised),
            vec![history(&revised[0..1]), Item::Event(revised[1].clone())]
        );
    }

    #[test]
    fn does_not_infer_supersession_without_actors_or_times() {
        let reviews = [
            by("", review(1, "CHANGES_REQUESTED", 12)),
            by("", review(2, "APPROVED", 13)),
            untimed(review(3, "CHANGES_REQUESTED", 0)),
            review(4, "APPROVED", 14),
            review(5, "CHANGES_REQUESTED", 14),
        ];
        let plain_events = events(&[], &reviews, None);
        assert_eq!(group(&plain_events), plain(&plain_events));
        let merged_events = events(&[], &reviews, Some(&merged(13)));
        let grouped = group(&merged_events);
        let find = |key: &str| {
            grouped.iter().find(|item| match item {
                Item::Event(e) => e.key == key,
                Item::History { key: k, .. } => k == key,
            })
        };
        assert!(matches!(find("review-1"), Some(Item::History { .. })));
        assert!(matches!(find("review-2"), Some(Item::Event(_))));
        assert!(matches!(find("review-3"), Some(Item::Event(_))));
    }

    #[test]
    fn accounts_for_review_states_in_pre_merge_history() {
        for state in [
            Some("APPROVED"),
            Some("CHANGES_REQUESTED"),
            Some("COMMENTED"),
            Some("DISMISSED"),
            Some("PENDING"),
            Some("UNKNOWN"),
            None,
        ] {
            let summary = Entry {
                state: state.map(str::to_string),
                body: "Review summary".into(),
                ..at(2, 13)
            };
            let events = events(&[at(1, 12)], &[summary], Some(&merged(17)));
            let known = matches!(
                state,
                Some("APPROVED" | "CHANGES_REQUESTED" | "COMMENTED" | "DISMISSED")
            );
            let expected = if known {
                vec![history(&events[0..2]), Item::Event(events[2].clone())]
            } else {
                plain(&events)
            };
            assert_eq!(group(&events), expected, "{state:?}");
        }
    }

    #[test]
    fn folds_dismissals_superseded_by_the_same_reviewer() {
        for state in ["APPROVED", "CHANGES_REQUESTED", "DISMISSED"] {
            let events = events(
                &[],
                &[
                    by("Reviewer", review(1, "DISMISSED", 12)),
                    by("reviewer", review(2, state, 13)),
                ],
                None,
            );
            assert_eq!(
                group(&events),
                vec![history(&events[0..1]), Item::Event(events[1].clone())]
            );
        }
        for state in ["APPROVED", "CHANGES_REQUESTED"] {
            let events = events(
                &[],
                &[review(1, state, 12), review(2, "DISMISSED", 13)],
                None,
            );
            assert_eq!(
                group(&events),
                vec![history(&events[0..1]), Item::Event(events[1].clone())]
            );
        }
        for state in [Some("COMMENTED"), Some("PENDING"), Some("UNKNOWN"), None] {
            let later = Entry {
                state: state.map(str::to_string),
                body: "Review summary".into(),
                ..at(2, 13)
            };
            let events = events(&[], &[review(1, "DISMISSED", 12), later], None);
            assert_eq!(group(&events), plain(&events), "{state:?}");
        }
    }

    #[test]
    fn does_not_hide_unproven_dismissals() {
        for reviews in [
            vec![
                untimed(review(1, "DISMISSED", 0)),
                review(2, "APPROVED", 13),
            ],
            vec![review(1, "DISMISSED", 13), review(2, "APPROVED", 13)],
            vec![
                by("", review(1, "DISMISSED", 12)),
                by("", review(2, "APPROVED", 13)),
            ],
            vec![
                review(1, "DISMISSED", 12),
                by("another", review(2, "APPROVED", 13)),
            ],
        ] {
            let events = events(&[], &reviews, None);
            assert_eq!(group(&events), plain(&events));
        }
        let events = events(&[], &[review(1, "DISMISSED", 17)], Some(&merged(17)));
        assert_eq!(group(&events), plain(&events));
        let post = super::events(
            &[],
            &[review(1, "DISMISSED", 18), review(2, "UNKNOWN", 19)],
            Some(&merged(17)),
        );
        assert_eq!(group(&post), plain(&post));
        let lone = super::events(
            &[],
            &[review(1, "DISMISSED", 13), review(2, "UNKNOWN", 14)],
            Some(&merged(17)),
        );
        let mut expected = vec![history(&lone[0..1])];
        expected.extend(plain(&lone[1..]));
        assert_eq!(group(&lone), expected);
    }
}
