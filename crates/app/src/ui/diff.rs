//! Diff messages (buzz-app's bundled `diffs` plugin): a kind-40008 row
//! renders as a card with a unified preview of its patch, and an expanded
//! dialog with a unified/split toggle, in place of Markdown.
//!
//! buzz-app parses patches with react-diff-view's `parseDiff`
//! (gitdiff-parser 0.3.1) and then validates them strictly (`parse.ts`).
//! The parser is ported quirk for quirk, because the validation is defined
//! against what it returns: a patch that is cut off, binary, in an unknown
//! format or over 100k stays readable as raw text instead.

use super::navigation;
use super::overlay;
use buzz_relay_client::fold::{DiffMeta, MessageRow};
use buzz_ui::{
    ActiveTheme, Button, ButtonSize, ButtonVariant, Colors, IconButton, IconButtonSize, IconName,
    Surface, TypeStyled,
    tokens::{radius, shadow},
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::borrow::Cow;
use std::rc::Rc;
use std::sync::Arc;

// ── parse (parse.ts, react-diff-view's parseDiff, gitdiff-parser) ──────────

/// The legacy sender caps patches at 60 KiB; anything over this many
/// UTF-16 units stays raw rather than building a diff table.
const MAX_PATCH: usize = 100_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffType {
    Add,
    Copy,
    Delete,
    Modify,
    Rename,
}

impl DiffType {
    fn label(self) -> &'static str {
        match self {
            Self::Add => "New file",
            Self::Copy => "Copied",
            Self::Delete => "Deleted",
            Self::Modify => "Modified",
            Self::Rename => "Renamed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    Insert,
    Delete,
    Normal,
}

/// One patch line. An insert has only a new line number, a delete only an
/// old one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub kind: ChangeKind,
    pub content: String,
    pub old_line: Option<u64>,
    pub new_line: Option<u64>,
}

impl Change {
    /// The patch line this change was read from.
    fn is_line(&self, line: &str) -> bool {
        let prefix = match self.kind {
            ChangeKind::Insert => '+',
            ChangeKind::Delete => '-',
            ChangeKind::Normal => ' ',
        };
        line.strip_prefix(prefix) == Some(self.content.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    /// The whole `@@` line.
    pub content: String,
    pub old_start: u64,
    pub new_start: u64,
    pub old_lines: u64,
    pub new_lines: u64,
    pub changes: Vec<Change>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileData {
    /// Empty when unknown (gitdiff-parser's falsy paths); `/dev/null` is
    /// kept as read.
    pub old_path: String,
    pub new_path: String,
    pub kind: DiffType,
    pub hunks: Vec<Hunk>,
    pub old_ending_newline: bool,
    pub new_ending_newline: bool,
}

/// `parsePatch`: the files of a complete textual patch, an empty list for
/// blank content, or `None` when the content should stay raw.
pub fn parse_patch(content: &str) -> Option<Vec<FileData>> {
    if content.len() > MAX_PATCH && content.encode_utf16().count() > MAX_PATCH {
        return None;
    }
    if js_trim_start(content).is_empty() {
        return Some(Vec::new());
    }
    let mut files = parse_diff(content)?;
    files.retain(|file| {
        !file.hunks.is_empty() || !file.old_path.is_empty() || !file.new_path.is_empty()
    });
    // The parser accepts incomplete hunks, and reads an explicit `,0` as the
    // omitted count of 1. Restore the header counts, then require them.
    for hunk in files.iter_mut().flat_map(|file| &mut file.hunks) {
        let header = hunk_header(&hunk.content)?;
        if !closes_header(header.rest) {
            return None;
        }
        hunk.old_lines = header.old_count.unwrap_or(1);
        hunk.new_lines = header.new_count.unwrap_or(1);
        let count =
            |skip: ChangeKind| hunk.changes.iter().filter(|c| c.kind != skip).count() as u64;
        if count(ChangeKind::Insert) != hunk.old_lines
            || count(ChangeKind::Delete) != hunk.new_lines
        {
            return None;
        }
    }
    (!files.is_empty() && consumes_patch(content, &files)).then_some(files)
}

/// `consumesPatch`: the parser silently skips input it does not know, so
/// every line must be a hunk line it returned, an exact no-newline marker
/// after one, or one of Git's known file headers.
fn consumes_patch(content: &str, files: &[FileData]) -> bool {
    let hunks: Vec<&Hunk> = files.iter().flat_map(|file| &file.hunks).collect();
    let mut next_hunk = 0;
    let mut current: Option<&Hunk> = None;
    let mut index = 0;
    let mut can_mark_newline = false;
    let mut lines: Vec<&str> = content.split('\n').collect();
    if lines.last() == Some(&"") {
        lines.pop();
    }
    for line in lines {
        if line.starts_with("diff --git ") {
            if current.is_some_and(|hunk| index != hunk.changes.len()) {
                return false;
            }
            current = None;
        } else if line.starts_with("@@") {
            if current.is_some_and(|hunk| index != hunk.changes.len()) {
                return false;
            }
            let Some(hunk) = hunks.get(next_hunk) else {
                return false;
            };
            next_hunk += 1;
            if line != hunk.content {
                return false;
            }
            current = Some(hunk);
            index = 0;
        } else if let Some(hunk) = current {
            if line == "\\ No newline at end of file" && can_mark_newline {
                can_mark_newline = false;
                continue;
            }
            if !hunk.changes.get(index).is_some_and(|c| c.is_line(line)) {
                return false;
            }
            index += 1;
            can_mark_newline = true;
            continue;
        } else if !known_header(line) {
            return false;
        }
        can_mark_newline = false;
    }
    next_hunk == hunks.len() && current.is_none_or(|hunk| index == hunk.changes.len())
}

/// `^(?:--- |\+\+\+ |index [0-9a-f]+\.\.[0-9a-f]+(?: \d+)?$|(?:old|new|deleted
/// file|new file) mode \d+$|(?:dis)?similarity index \d+%$|(?:rename|copy)
/// (?:from|to) |Binary files .+ differ$)`.
fn known_header(line: &str) -> bool {
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let hex = |s: &str| !s.is_empty() && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    if [
        "--- ",
        "+++ ",
        "rename from ",
        "rename to ",
        "copy from ",
        "copy to ",
    ]
    .iter()
    .any(|prefix| line.starts_with(prefix))
    {
        return true;
    }
    if let Some(rest) = line.strip_prefix("index ") {
        let (range, mode) = match rest.split_once(' ') {
            Some((range, mode)) => (range, Some(mode)),
            None => (rest, None),
        };
        return range
            .split_once("..")
            .is_some_and(|(old, new)| hex(old) && hex(new))
            && mode.is_none_or(digits);
    }
    for prefix in [
        "old mode ",
        "new mode ",
        "deleted file mode ",
        "new file mode ",
    ] {
        if let Some(mode) = line.strip_prefix(prefix) {
            return digits(mode);
        }
    }
    if let Some(rest) = line
        .strip_prefix("similarity index ")
        .or_else(|| line.strip_prefix("dissimilarity index "))
    {
        return rest.strip_suffix('%').is_some_and(digits);
    }
    line.strip_prefix("Binary files ")
        .and_then(|rest| rest.strip_suffix(" differ"))
        .is_some_and(|middle| {
            !middle.is_empty() && !middle.contains(['\r', '\u{2028}', '\u{2029}'])
        })
}

/// `fileLabel`: `old → new` for a move, else the one path, else the
/// message's file tag, else "diff".
pub fn file_label(file: &FileData, fallback: Option<&str>) -> String {
    let path = |p: &str| (!p.is_empty() && p != "/dev/null").then(|| p.to_owned());
    match (path(&file.old_path), path(&file.new_path)) {
        (Some(old), Some(new)) if old != new => format!("{old} → {new}"),
        (old, new) => new
            .or(old)
            .or_else(|| fallback.filter(|f| !f.is_empty()).map(str::to_owned))
            .unwrap_or_else(|| "diff".into()),
    }
}

/// JavaScript's `\s`.
fn js_space(c: char) -> bool {
    (c.is_whitespace() && c != '\u{85}') || c == '\u{feff}'
}

fn js_trim_start(s: &str) -> &str {
    s.trim_start_matches(js_space)
}

/// JavaScript's `s.slice(start, end)` in characters (negative counts from
/// the end).
fn js_slice(s: &str, start: isize, end: Option<isize>) -> String {
    let chars: Vec<char> = s.chars().collect();
    let len = chars.len() as isize;
    let clamp = |i: isize| if i < 0 { (len + i).max(0) } else { i.min(len) };
    let (from, to) = (clamp(start), end.map_or(len, clamp));
    if from >= to {
        String::new()
    } else {
        chars[from as usize..to as usize].iter().collect()
    }
}

/// `s.slice(n)` for a non-negative `n`.
fn skip(s: &str, n: usize) -> &str {
    s.char_indices().nth(n).map_or("", |(i, _)| &s[i..])
}

/// A hunk header's numbers, and what follows them.
struct HunkHeader<'a> {
    old_start: u64,
    old_count: Option<u64>,
    new_start: u64,
    new_count: Option<u64>,
    rest: &'a str,
}

/// gitdiff-parser's `^@@\s+-(\d+)(,(\d+))?\s+\+(\d+)(,(\d+))?`.
fn hunk_header(line: &str) -> Option<HunkHeader<'_>> {
    fn spaces(s: &str) -> Option<&str> {
        let rest = s.trim_start_matches(js_space);
        (rest.len() < s.len()).then_some(rest)
    }
    fn number(s: &str) -> Option<(u64, &str)> {
        let end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
        Some((s[..end].parse().ok()?, &s[end..]))
    }
    fn count(s: &str) -> (Option<u64>, &str) {
        s.strip_prefix(',')
            .and_then(number)
            .map_or((None, s), |(n, rest)| (Some(n), rest))
    }
    let rest = spaces(line.strip_prefix("@@")?)?.strip_prefix('-')?;
    let (old_start, rest) = number(rest)?;
    let (old_count, rest) = count(rest);
    let rest = spaces(rest)?.strip_prefix('+')?;
    let (new_start, rest) = number(rest)?;
    let (new_count, rest) = count(rest);
    Some(HunkHeader {
        old_start,
        old_count,
        new_start,
        new_count,
        rest,
    })
}

/// The `\s+@@` that `parse.ts` also requires after the counts.
fn closes_header(rest: &str) -> bool {
    let trimmed = rest.trim_start_matches(js_space);
    trimmed.len() < rest.len() && trimmed.starts_with("@@")
}

/// react-diff-view's `parseDiff`: a plain unified patch gets a synthetic
/// Git header before gitdiff-parser reads it.
fn parse_diff(content: &str) -> Option<Vec<FileData>> {
    gitdiff_parse(&normalize_diff_text(js_trim_start(content)))
}

/// `normalizeDiffText`, including its `indexOf` of -1 arithmetic.
fn normalize_diff_text(text: &str) -> Cow<'_, str> {
    if text.starts_with("diff --git") {
        return text.into();
    }
    let drop_last = |s: &str| js_slice(s, 0, Some(-1));
    let first = text.find('\n');
    let second = first.and_then(|a| text[a + 1..].find('\n').map(|b| a + 1 + b));
    let first_line = match first {
        Some(a) => text[..a].to_owned(),
        None => drop_last(text),
    };
    let second_line = match (first, second) {
        (Some(a), Some(b)) => text[a + 1..b].to_owned(),
        (Some(a), None) => drop_last(&text[a + 1..]),
        (None, _) => drop_last(text),
    };
    let rest = second.map_or(text, |b| &text[b + 1..]);
    let path = |line: &str| {
        let parts: Vec<&str> = line.split(' ').collect();
        parts
            .get(1..parts.len().saturating_sub(3))
            .unwrap_or_default()
            .join(" ")
    };
    let (old, new) = (path(&first_line), path(&second_line));
    format!(
        "diff --git a/{old} b/{new}\nindex 1111111..2222222 100644\n--- a/{old}\n+++ b/{new}\n{rest}"
    )
    .into()
}

/// gitdiff-parser's `parsePathFromFirstLine`. `None` where it would throw.
fn paths_from_first_line(line: &str) -> Option<(String, String)> {
    let files = skip(line, 11);
    let chars: Vec<char> = files.chars().collect();
    let index_of = |from: isize| {
        let from = from.max(0) as usize;
        chars
            .iter()
            .skip(from)
            .position(|&c| c == '"')
            .map_or(-1, |i| (i + from) as isize)
    };
    match index_of(0) {
        -1 => {
            let mut segs = files.split(' ');
            let old = skip(segs.next()?, 2).to_owned();
            let new = skip(segs.next()?, 2).to_owned();
            Some((old, new))
        }
        0 => {
            let next = index_of(2);
            let old = js_slice(files, 3, Some(next));
            let new_quote = index_of(next + 1);
            let new = if new_quote < 0 {
                js_slice(files, next + 4, None)
            } else {
                js_slice(files, new_quote + 3, Some(-1))
            };
            Some((old, new))
        }
        _ => {
            let mut segs = files.split(' ');
            let old = skip(segs.next()?, 2).to_owned();
            let new = js_slice(segs.next()?, 3, Some(-1));
            Some((old, new))
        }
    }
}

/// gitdiff-parser 0.3.1's `parse`. `None` where it would throw, which
/// `parsePatch` turns into the raw fallback.
fn gitdiff_parse(source: &str) -> Option<Vec<FileData>> {
    let lines: Vec<&str> = source.split('\n').collect();
    let mut files: Vec<FileData> = Vec::new();
    let mut in_hunks = false;
    let mut current: Option<usize> = None;
    // Not reset per file, as in the original.
    let mut hunk: Option<(usize, usize)> = None;
    let mut info_type: Option<&str> = None;
    let (mut old_line, mut new_line) = (0u64, 0u64);
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if line.starts_with("diff --git") {
            let (old_path, new_path) = paths_from_first_line(line)?;
            files.push(FileData {
                old_path,
                new_path,
                kind: DiffType::Modify,
                hunks: Vec::new(),
                old_ending_newline: true,
                new_ending_newline: true,
            });
            let f = files.len() - 1;
            current = Some(f);
            let mut kind = None;
            loop {
                i += 1;
                let Some(&meta) = lines.get(i).filter(|l| !l.is_empty()) else {
                    break;
                };
                let space = meta.find(' ');
                // A line without a space keeps the previous type (even
                // across files).
                if let Some(space) = space {
                    info_type = Some(&meta[..space]);
                }
                let after = space.map_or(meta, |s| &meta[s + 1..]);
                match info_type {
                    Some("diff") => {
                        i -= 1;
                        break;
                    }
                    Some(t @ ("copy" | "rename")) => {
                        if after.starts_with("from") {
                            files[f].old_path = skip(after, 5).to_owned();
                        } else {
                            files[f].new_path = skip(after, 3).to_owned();
                        }
                        kind = Some(if t == "copy" {
                            DiffType::Copy
                        } else {
                            DiffType::Rename
                        });
                    }
                    Some("---") => {
                        let mut old = after.to_owned();
                        i += 1;
                        let mut new = skip(lines.get(i)?, 4).to_owned();
                        if old == "/dev/null" {
                            new = skip(&new, 2).to_owned();
                            kind = Some(DiffType::Add);
                        } else if new == "/dev/null" {
                            old = skip(&old, 2).to_owned();
                            kind = Some(DiffType::Delete);
                        } else {
                            kind = Some(DiffType::Modify);
                            old = skip(&old, 2).to_owned();
                            new = skip(&new, 2).to_owned();
                        }
                        if !old.is_empty() {
                            files[f].old_path = old;
                        }
                        if !new.is_empty() {
                            files[f].new_path = new;
                        }
                        in_hunks = true;
                        break;
                    }
                    _ => {}
                }
            }
            files[f].kind = kind.unwrap_or(DiffType::Modify);
        } else if line.starts_with("Binary") {
            let f = current?;
            files[f].kind = if line.contains("/dev/null and") {
                DiffType::Add
            } else if line.contains("and /dev/null") {
                DiffType::Delete
            } else {
                DiffType::Modify
            };
            in_hunks = false;
            current = None;
        } else if in_hunks {
            if line.starts_with("@@") {
                let header = hunk_header(line)?;
                let f = current?;
                files[f].hunks.push(Hunk {
                    content: line.to_owned(),
                    old_start: header.old_start,
                    new_start: header.new_start,
                    old_lines: header.old_count.filter(|&n| n != 0).unwrap_or(1),
                    new_lines: header.new_count.filter(|&n| n != 0).unwrap_or(1),
                    changes: Vec::new(),
                });
                hunk = Some((f, files[f].hunks.len() - 1));
                old_line = header.old_start;
                new_line = header.new_start;
            } else {
                let content = skip(line, 1).to_owned();
                let change = match line.chars().next() {
                    Some('+') => {
                        new_line += 1;
                        Some(Change {
                            kind: ChangeKind::Insert,
                            content,
                            old_line: None,
                            new_line: Some(new_line - 1),
                        })
                    }
                    Some('-') => {
                        old_line += 1;
                        Some(Change {
                            kind: ChangeKind::Delete,
                            content,
                            old_line: Some(old_line - 1),
                            new_line: None,
                        })
                    }
                    Some(' ') => {
                        old_line += 1;
                        new_line += 1;
                        Some(Change {
                            kind: ChangeKind::Normal,
                            content,
                            old_line: Some(old_line - 1),
                            new_line: Some(new_line - 1),
                        })
                    }
                    Some('\\') => {
                        let (hf, hh) = hunk?;
                        let last = files[hf].hunks[hh].changes.last()?.kind;
                        let f = current?;
                        if last != ChangeKind::Delete {
                            files[f].new_ending_newline = false;
                        }
                        if last != ChangeKind::Insert {
                            files[f].old_ending_newline = false;
                        }
                        None
                    }
                    _ => None,
                };
                if let Some(change) = change {
                    let (hf, hh) = hunk?;
                    files[hf].hunks[hh].changes.push(change);
                }
            }
        }
        i += 1;
    }
    Some(files)
}

// ── card model (DiffMessage.tsx) ───────────────────────────────────────────

/// What the card shows around the patch.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Header {
    /// The file tag, or "Diff".
    title: String,
    /// The commit page, else the repository: HTTPS only.
    source: Option<String>,
    /// The short SHA (or "Source" when linked without one).
    commit_label: Option<String>,
    description: Option<String>,
    truncated: bool,
}

impl Header {
    fn new(meta: &DiffMeta) -> Self {
        let repo = meta.repo_url.as_deref().and_then(safe_message_url);
        let commit = meta.commit_sha.as_deref();
        let commit_url = repo.as_deref().zip(commit).and_then(|(repo, commit)| {
            ((7..=64).contains(&commit.len()) && commit.bytes().all(|b| b.is_ascii_hexdigit()))
                .then(|| format!("{}/commit/{commit}", repo.strip_suffix('/').unwrap_or(repo)))
        });
        let source = commit_url.or(repo);
        let short = commit.map(|c| c.chars().take(7).collect::<String>());
        let commit_label = if source.is_some() {
            Some(
                short
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "Source".into()),
            )
        } else {
            short.filter(|s| !s.is_empty())
        };
        Self {
            title: meta
                .file_path
                .clone()
                .filter(|p| !p.is_empty())
                .unwrap_or_else(|| "Diff".into()),
            source,
            commit_label,
            description: meta.description.clone().filter(|d| !d.is_empty()),
            truncated: meta.truncated,
        }
    }
}

/// `safeMessageUrl`: credential-free HTTPS, WHATWG-serialized.
fn safe_message_url(value: &str) -> Option<String> {
    let url = url::Url::parse(value).ok()?;
    (url.scheme() == "https" && url.username().is_empty() && url.password().is_none())
        .then(|| url.into())
}

/// The split view's rows: a delete followed by an insert share a row
/// (react-diff-view's `SplitHunk`).
fn split_rows(changes: &[Change]) -> Vec<(Option<&Change>, Option<&Change>)> {
    let mut rows = Vec::new();
    let mut i = 0;
    while i < changes.len() {
        let change = &changes[i];
        rows.push(match change.kind {
            ChangeKind::Normal => (Some(change), Some(change)),
            ChangeKind::Delete => match changes.get(i + 1) {
                Some(next) if next.kind == ChangeKind::Insert => {
                    i += 1;
                    (Some(change), Some(next))
                }
                _ => (Some(change), None),
            },
            ChangeKind::Insert => (None, Some(change)),
        });
        i += 1;
    }
    rows
}

/// Tabs to the browser's 8-column stops, since `pre-wrap` keeps them.
fn expand_tabs(text: &str) -> Cow<'_, str> {
    if !text.contains('\t') {
        return text.into();
    }
    let mut out = String::with_capacity(text.len() + 8);
    let mut column = 0;
    for c in text.chars() {
        match c {
            '\t' => {
                let n = 8 - column % 8;
                out.extend(std::iter::repeat_n(' ', n));
                column += n;
            }
            '\n' => {
                out.push(c);
                column = 0;
            }
            _ => {
                out.push(c);
                column += 1;
            }
        }
    }
    out.into()
}

// ── rendering (DiffMessage.tsx, DiffViewer.tsx, Diffs.module.css) ──────────

/// One `ch` of the 12px mono face (JetBrains Mono advances 0.6em).
const CH: f32 = 0.75 * 0.6;
/// `.diff-gutter-col`: 7ch.
const GUTTER: f32 = 7. * CH;

/// The card for a diff row, or `None` for other rows.
pub fn card(row: &Arc<MessageRow>) -> Option<AnyElement> {
    row.diff.as_ref()?;
    Some(DiffCard { row: row.clone() }.into_any_element())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Unified,
    Split,
}

/// A row's patch as [`parse_patch`] read it.
type Parsed = Rc<Option<Vec<FileData>>>;

/// Per-row state: the parsed patch (re-read only when the content
/// changes) and the dialog.
struct State {
    parsed: Option<(Arc<MessageRow>, Parsed)>,
    open: bool,
    view: View,
    focus: FocusHandle,
    preview: ScrollHandle,
}

impl State {
    fn files(&mut self, row: &Arc<MessageRow>) -> Parsed {
        match &self.parsed {
            Some((seen, files)) if Arc::ptr_eq(seen, row) || seen.content == row.content => {
                files.clone()
            }
            _ => {
                let files = Rc::new(parse_patch(&row.content));
                self.parsed = Some((row.clone(), files.clone()));
                files
            }
        }
    }
}

#[derive(IntoElement)]
struct DiffCard {
    row: Arc<MessageRow>,
}

impl RenderOnce for DiffCard {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let c = *cx.colors();
        let Some(meta) = &self.row.diff else {
            return div().into_any_element();
        };
        let header = Header::new(meta);
        let id = self.row.id.to_hex();
        let state =
            window.use_keyed_state(SharedString::from(format!("diff-{id}")), cx, |_, cx| {
                State {
                    parsed: None,
                    open: false,
                    view: View::Unified,
                    focus: cx.focus_handle(),
                    preview: ScrollHandle::new(),
                }
            });
        let files = state.update(cx, |state, _| state.files(&self.row));
        let (open, view, focus, preview) = {
            let s = state.read(cx);
            (s.open, s.view, s.focus.clone(), s.preview.clone())
        };
        let content = &self.row.content;
        let file_path = meta.file_path.as_deref();

        let expand = {
            let state = state.clone();
            let focus = focus.clone();
            IconButton::new(
                SharedString::from(format!("diff-expand-{id}")),
                IconName::ArrowsOut,
            )
            .size(IconButtonSize::Sm)
            .on_click(move |_, window, cx| {
                state.update(cx, |state, cx| {
                    state.view = View::Unified;
                    state.open = true;
                    cx.notify();
                });
                focus.focus(window, cx);
            })
        };
        let source = header
            .commit_label
            .clone()
            .map(|label| match &header.source {
                Some(url) => {
                    link(format!("diff-source-{id}"), label, url.clone()).into_any_element()
                }
                None => div().child(label).into_any_element(),
            });

        let card = div()
            .flex()
            .flex_col()
            .min_w_0()
            .overflow_hidden()
            .border_1()
            .border_color(c.border_standard)
            .rounded(radius::CONTROL)
            .bg(c.surface_panel)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(rems(0.5))
                    .px(rems(0.75))
                    .py(rems(0.5))
                    .bg(c.surface_inset)
                    .text_mono()
                    .text_color(c.text_standard)
                    .child(div().min_w_0().truncate().child(header.title.clone()))
                    .child(
                        div()
                            .ml_auto()
                            .flex_none()
                            .whitespace_nowrap()
                            .children(source),
                    )
                    .child(expand),
            )
            .when_some(header.description.clone(), |this, text| {
                this.child(note(&c).child(text))
            })
            .child(
                div()
                    .id(SharedString::from(format!("diff-preview-{id}")))
                    .max_h(rems(25.))
                    .overflow_y_scroll()
                    .track_scroll(&preview)
                    .on_scroll_wheel(chain_scroll(preview.clone()))
                    .p(rems(0.75))
                    .text_color(c.text_standard)
                    .child(viewer(
                        &format!("diff-{id}"),
                        content,
                        files.as_deref(),
                        file_path,
                        View::Unified,
                        &c,
                    )),
            )
            .when(header.truncated, |this| {
                this.child(truncated(&format!("diff-notice-{id}"), &header, &c))
            });

        let dialog = open.then(|| {
            let close = {
                let state = state.clone();
                move |_: &mut Window, cx: &mut App| {
                    state.update(cx, |state, cx| {
                        state.open = false;
                        cx.notify();
                    });
                }
            };
            let toggle = |label: &'static str, target: View| {
                let state = state.clone();
                Button::new(SharedString::from(format!("diff-{label}-{id}")), label)
                    .size(ButtonSize::Sm)
                    .surface(Surface::Floating)
                    .variant(if view == target {
                        ButtonVariant::Subtle
                    } else {
                        ButtonVariant::Ghost
                    })
                    .on_click(move |_, _, cx| {
                        state.update(cx, |state, cx| {
                            state.view = target;
                            cx.notify();
                        })
                    })
            };
            let body = div()
                .flex()
                .flex_col()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(rems(0.5))
                        .mb(rems(1.))
                        .child(toggle("Unified", View::Unified))
                        .child(toggle("Split", View::Split)),
                )
                .when(header.truncated, |this| {
                    this.child(truncated(&format!("diff-dialog-notice-{id}"), &header, &c))
                })
                .child(viewer(
                    &format!("diff-dialog-{id}"),
                    content,
                    files.as_deref(),
                    file_path,
                    view,
                    &c,
                ));
            overlay::modal(
                window,
                expanded_dialog(&id, &header.title, body, focus, close, window, &c),
                None,
            )
        });

        div()
            .min_w_0()
            .child(card)
            .children(dialog)
            .into_any_element()
    }
}

/// `size="expanded"`: the window less 24px a side; the body scrolls. Like
/// buzz-app's dialog, only Escape and the close button dismiss it.
fn expanded_dialog(
    id: &str,
    title: &str,
    body: Div,
    focus: FocusHandle,
    close: impl Fn(&mut Window, &mut App) + Clone + 'static,
    window: &Window,
    c: &Colors,
) -> impl IntoElement {
    let inset = rems(3.).to_pixels(window.rem_size());
    let viewport = window.viewport_size();
    let on_escape = close.clone();
    div()
        .id(SharedString::from(format!("diff-dialog-{id}")))
        .key_context(buzz_ui::DIALOG_CONTEXT)
        .track_focus(&focus)
        .on_action(move |_: &buzz_ui::Cancel, window, cx| on_escape(window, cx))
        .flex()
        .flex_col()
        .w(viewport.width - inset)
        .h(viewport.height - inset)
        .overflow_hidden()
        .p(rems(1.5))
        .bg(c.surface_popover)
        .border_1()
        .border_color(c.border_standard)
        .rounded(radius::PANEL)
        .shadow(shadow::sm())
        .text_color(c.text_standard)
        .text_body()
        .child(
            div()
                .flex()
                .items_start()
                .justify_between()
                .gap(rems(1.))
                .child(div().min_w_0().text_label().child(title.to_owned()))
                .child(
                    IconButton::new("diff-dialog-close", IconName::X)
                        .size(IconButtonSize::Sm)
                        .surface(Surface::Floating)
                        .on_click(move |_, window, cx| close(window, cx)),
                ),
        )
        .child(
            div()
                .id("diff-dialog-body")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .py(rems(1.5))
                .child(body),
        )
}

/// `.description, .notice`.
fn note(c: &Colors) -> Div {
    div()
        .px(rems(0.75))
        .py(rems(0.5))
        .text_body_sm()
        .text_color(c.text_subtle)
}

/// "Diff truncated." with a link to the source when there is one.
fn truncated(id: &str, header: &Header, c: &Colors) -> Div {
    const LEAD: &str = "Diff truncated. ";
    let Some(url) = header.source.clone() else {
        return note(c).child(format!(
            "{LEAD}View the full diff at the source repository."
        ));
    };
    let text = format!("{LEAD}View full diff at the source repository");
    let range = LEAD.len()..text.len();
    let styled = StyledText::new(text).with_highlights([(
        range.clone(),
        HighlightStyle {
            underline: Some(UnderlineStyle {
                thickness: px(1.),
                color: None,
                wavy: false,
            }),
            ..Default::default()
        },
    )]);
    note(c).child(
        InteractiveText::new(SharedString::from(id.to_owned()), styled)
            .on_click(vec![range], move |_, window, cx| {
                navigation::open_url(&url, window, cx)
            }),
    )
}

/// An underlined link in the surrounding colour (`.card a`).
fn link(id: String, label: String, url: String) -> Stateful<Div> {
    div()
        .id(SharedString::from(id))
        .cursor_pointer()
        .underline()
        .child(label)
        .on_click(move |_, window, cx| navigation::open_url(&url, window, cx))
}

/// Lets the page scroll once the preview cannot, as a browser chains
/// wheel scrolling. Runs after the preview applied the delta.
fn chain_scroll(handle: ScrollHandle) -> impl Fn(&ScrollWheelEvent, &mut Window, &mut App) {
    move |event, window, cx| {
        let delta = event.delta.pixel_delta(window.line_height()).y;
        let max = handle.max_offset().y;
        let before = -handle.offset().y + delta;
        let consumed = (delta < px(0.) && before < max) || (delta > px(0.) && before > px(0.));
        if consumed {
            cx.stop_propagation();
        }
    }
}

/// `DiffViewer`: the raw fallback, the empty state, or each file.
fn viewer(
    id: &str,
    content: &str,
    files: Option<&[FileData]>,
    file_path: Option<&str>,
    view: View,
    c: &Colors,
) -> AnyElement {
    let Some(files) = files else {
        return raw(id, content);
    };
    if files.is_empty() {
        return div()
            .text_body_sm()
            .child("No diff content")
            .into_any_element();
    }
    div()
        .flex()
        .flex_col()
        .gap(rems(0.75))
        .text_mono()
        .children(files.iter().enumerate().map(|(index, file)| {
            let label = file_label(file, file_path);
            let kind = file.kind;
            let changes = file.hunks.iter().flat_map(|hunk| &hunk.changes);
            let additions = changes
                .clone()
                .filter(|c| c.kind == ChangeKind::Insert)
                .count();
            let deletions = changes.filter(|c| c.kind == ChangeKind::Delete).count();
            let show_header =
                files.len() > 1 || Some(label.as_str()) != file_path || kind != DiffType::Modify;
            div()
                .flex()
                .flex_col()
                .min_w_0()
                .when(show_header, |this| {
                    this.child(
                        div()
                            .flex()
                            .items_center()
                            .gap(rems(0.5))
                            .px(rems(0.75))
                            .py(rems(0.5))
                            .bg(c.surface_inset)
                            .child(div().min_w_0().truncate().child(label))
                            .child(div().flex_none().child(kind.label()))
                            .child(
                                div()
                                    .flex_none()
                                    .text_color(c.text_success)
                                    .child(format!("+{additions}")),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .text_color(c.text_danger)
                                    .child(format!("−{deletions}")),
                            ),
                    )
                })
                .child(if file.hunks.is_empty() {
                    note(c)
                        .child("No textual hunks in this diff.")
                        .into_any_element()
                } else {
                    match view {
                        View::Unified => unified(file, c).into_any_element(),
                        View::Split => div()
                            .id(SharedString::from(format!("{id}-file-{index}")))
                            .overflow_x_scroll()
                            .restrict_scroll_to_axis()
                            .child(split(file, c).w_full().min_w(rems(48.)))
                            .into_any_element(),
                    }
                })
        }))
        .into_any_element()
}

/// `pre.raw`: the patch as sent, scrolling sideways.
fn raw(id: &str, content: &str) -> AnyElement {
    // A `pre` does not show a final newline as an empty line.
    let text = content.strip_suffix('\n').unwrap_or(content);
    div()
        .id(SharedString::from(format!("{id}-raw")))
        .overflow_x_scroll()
        .restrict_scroll_to_axis()
        .text_mono()
        .child(
            div()
                .whitespace_nowrap()
                .child(expand_tabs(text).into_owned()),
        )
        .into_any_element()
}

/// The fills and text colour of a change's cells.
fn tone(kind: ChangeKind, c: &Colors) -> (Option<Hsla>, Hsla) {
    match kind {
        ChangeKind::Insert => (Some(c.affordance_success), c.text_success),
        ChangeKind::Delete => (Some(c.affordance_danger), c.text_danger),
        ChangeKind::Normal => (None, c.text_standard),
    }
}

/// `.diff-gutter`: the line number, right-aligned in 7ch.
fn gutter(number: Option<u64>, kind: ChangeKind, c: &Colors) -> Div {
    let (fill, text) = tone(kind, c);
    div()
        .flex_none()
        .w(rems(GUTTER))
        .px(rems(CH))
        .flex()
        .justify_end()
        .whitespace_nowrap()
        .text_color(text)
        .when_some(fill, |this, fill| this.bg(fill))
        .children(number.map(|n| n.to_string()))
}

/// `.diff-code`: the line, wrapping anywhere.
fn code(change: &Change, c: &Colors) -> Div {
    let (fill, text) = tone(change.kind, c);
    div()
        .flex_1()
        .min_w_0()
        .pl(rems(0.375))
        .text_color(text)
        .when_some(fill, |this, fill| this.bg(fill))
        .child(expand_tabs(&change.content).into_owned())
}

fn line_row() -> Div {
    div().flex().w_full().min_h(rems(1.))
}

/// The unified table: old and new gutters, then the line.
fn unified(file: &FileData, c: &Colors) -> Div {
    div()
        .flex()
        .flex_col()
        .w_full()
        .children(
            file.hunks
                .iter()
                .flat_map(|hunk| &hunk.changes)
                .map(|change| {
                    line_row()
                        .child(gutter(change.old_line, change.kind, c))
                        .child(gutter(change.new_line, change.kind, c))
                        .child(code(change, c))
                }),
        )
}

/// The split table: old and new side by side. A new or deleted file has
/// one side only.
fn split(file: &FileData, c: &Colors) -> Div {
    let monotonous = matches!(file.kind, DiffType::Add | DiffType::Delete);
    let old_side = |change: Option<&Change>| match change {
        Some(change) => [gutter(change.old_line, change.kind, c), code(change, c)],
        None => omitted(c),
    };
    let new_side = |change: Option<&Change>| match change {
        Some(change) => [gutter(change.new_line, change.kind, c), code(change, c)],
        None => omitted(c),
    };
    div()
        .flex()
        .flex_col()
        .children(file.hunks.iter().flat_map(|hunk| {
            split_rows(&hunk.changes)
                .into_iter()
                .map(|(old, new)| {
                    let row = line_row();
                    if monotonous {
                        match old {
                            Some(_) => row.children(old_side(old)),
                            None => row.children(new_side(new)),
                        }
                    } else {
                        row.children(old_side(old)).children(new_side(new))
                    }
                })
                .collect::<Vec<_>>()
        }))
}

/// `.diff-gutter-omit`: an empty side, marked by a 2px rule at 4.6ch.
fn omitted(c: &Colors) -> [Div; 2] {
    [
        div().flex_none().w(rems(GUTTER)).relative().child(
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left(rems(5.6 * CH))
                .w(px(2.))
                .bg(c.border_standard),
        ),
        div().flex_1().min_w_0(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    // gpui's `test` macro comes in with the glob above.
    use core::prelude::v1::test;

    const PATCH: &str =
        "diff --git a/a.ts b/a.ts\n--- a/a.ts\n+++ b/a.ts\n@@ -1 +1 @@\n-old\n+new\n";

    fn change(kind: ChangeKind, content: &str, line: u64) -> Change {
        Change {
            kind,
            content: content.into(),
            old_line: (kind == ChangeKind::Delete).then_some(line),
            new_line: (kind == ChangeKind::Insert).then_some(line),
        }
    }

    #[test]
    fn parses_multiple_files_and_preserves_change_line_numbers() {
        let files = parse_patch(&format!("{PATCH}{}", PATCH.replace("a.ts", "b.ts"))).unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(
            files[0].hunks[0].changes,
            [
                change(ChangeKind::Delete, "old", 1),
                change(ChangeKind::Insert, "new", 1)
            ]
        );
        assert_eq!(file_label(&files[1], None), "b.ts");
    }

    #[test]
    fn preserves_explicit_zero_counts() {
        use ChangeKind::{Delete, Insert};
        for (name, old_path, new_path, range, lines, old_lines, new_lines, kind, line) in [
            (
                "new file",
                "/dev/null",
                "b/a.ts",
                "-0,0 +1,2",
                "+one\n+two",
                0,
                2,
                Insert,
                1,
            ),
            (
                "deleted file",
                "a/a.ts",
                "/dev/null",
                "-1,2 +0,0",
                "-one\n-two",
                2,
                0,
                Delete,
                1,
            ),
            (
                "zero-context insertion",
                "a/a.ts",
                "b/a.ts",
                "-3,0 +4,2",
                "+one\n+two",
                0,
                2,
                Insert,
                4,
            ),
            (
                "zero-context deletion",
                "a/a.ts",
                "b/a.ts",
                "-4,2 +3,0",
                "-one\n-two",
                2,
                0,
                Delete,
                4,
            ),
        ] {
            let files = parse_patch(&format!(
                "diff --git a/a.ts b/a.ts\n--- {old_path}\n+++ {new_path}\n@@ {range} @@\n{lines}\n"
            ))
            .unwrap_or_else(|| panic!("{name}"));
            assert_eq!(files.len(), 1, "{name}");
            let hunk = &files[0].hunks[0];
            assert_eq!(
                (hunk.old_lines, hunk.new_lines),
                (old_lines, new_lines),
                "{name}"
            );
            assert_eq!(
                hunk.changes,
                [change(kind, "one", line), change(kind, "two", line + 1)],
                "{name}"
            );
        }
    }

    #[test]
    fn retains_changes_with_no_textual_hunks() {
        for (name, content, label, kind) in [
            (
                "rename",
                "diff --git a/old.ts b/new.ts\nsimilarity index 100%\nrename from old.ts\nrename to new.ts\n",
                "old.ts → new.ts",
                DiffType::Rename,
            ),
            (
                "binary",
                "diff --git a/image.png b/image.png\nindex 1234567..abcdef0 100644\nBinary files a/image.png and b/image.png differ\n",
                "image.png",
                DiffType::Modify,
            ),
            (
                "mode",
                "diff --git a/run b/run\nold mode 100644\nnew mode 100755\n",
                "run",
                DiffType::Modify,
            ),
        ] {
            let files = parse_patch(content).unwrap_or_else(|| panic!("{name}"));
            assert_eq!(files.len(), 1, "{name}");
            assert!(files[0].hunks.is_empty(), "{name}");
            assert_eq!(file_label(&files[0], None), label, "{name}");
            assert_eq!(files[0].kind, kind, "{name}");
        }
    }

    #[test]
    fn falls_back_on_malformed_incomplete_or_excessive_content() {
        let cases = [
            "malformed <script>alert(1)</script>".to_owned(),
            PATCH.replace("-1 +1", "-1,3 +1,3"),
            format!("{PATCH}UNPARSED_TRAILER\n"),
            PATCH.replace("-old", "UNKNOWN_HUNK_LINE\n-old"),
            PATCH.replace("--- a/a.ts", "UNKNOWN_HEADER\n--- a/a.ts"),
            format!("PREAMBLE\n{PATCH}"),
            format!("{PATCH}\\ not a newline marker\n"),
            format!("{PATCH}\\ No newline at end of file\n\\ No newline at end of file\n"),
            "diff --git a/a.bin b/a.bin\nGIT binary patch\nliteral 3\nKcmZQzWC8#H2mk;8\n"
                .to_owned(),
            "x".repeat(100_001),
        ];
        for content in cases {
            assert_eq!(parse_patch(&content), None, "{content:.60}");
        }
    }

    #[test]
    fn distinguishes_empty_patches_and_supports_plain_unified_patches() {
        assert_eq!(parse_patch(" \n"), Some(Vec::new()));
        let plain = &PATCH[PATCH.find("---").unwrap()..];
        let files = parse_patch(plain).unwrap();
        assert_eq!(files[0].hunks.len(), 1);
        // The synthetic header has no paths, so the file tag names it.
        assert_eq!(file_label(&files[0], Some("a.ts")), "a.ts");
        assert_eq!(file_label(&files[0], None), "diff");
    }

    #[test]
    fn retains_multiple_hunks_and_exact_no_newline_markers() {
        let content = format!(
            "{}\\ No newline at end of file\n@@ -4 +4 @@\n-before\n+after\n",
            PATCH.replace("-old\n", "-old\n\\ No newline at end of file\n")
        );
        let files = parse_patch(&content).unwrap();
        assert_eq!(files[0].hunks.len(), 2);
        assert!(!files[0].old_ending_newline);
        assert!(!files[0].new_ending_newline);
        let contents: Vec<&str> = files[0].hunks[1]
            .changes
            .iter()
            .map(|c| c.content.as_str())
            .collect();
        assert_eq!(contents, ["before", "after"]);
    }

    #[test]
    fn keeps_context_line_numbers_on_both_sides() {
        let files = parse_patch(
            "diff --git a/a b/a\nindex 1234567..89abcde 100644\n--- a/a\n+++ b/a\n@@ -10,3 +20,3 @@ fn main\n keep\n-old\n+new\n keep\n",
        )
        .unwrap();
        let changes = &files[0].hunks[0].changes;
        assert_eq!(
            (changes[0].old_line, changes[0].new_line),
            (Some(10), Some(20))
        );
        assert_eq!(changes[1].old_line, Some(11));
        assert_eq!(changes[2].new_line, Some(21));
        assert_eq!(
            (changes[3].old_line, changes[3].new_line),
            (Some(12), Some(22))
        );
    }

    #[test]
    fn labels_new_deleted_and_renamed_files() {
        let added = parse_patch(
            "diff --git a/n.ts b/n.ts\nnew file mode 100644\nindex 0000000..1111111\n--- /dev/null\n+++ b/n.ts\n@@ -0,0 +1 @@\n+hi\n",
        )
        .unwrap();
        assert_eq!(added[0].kind, DiffType::Add);
        assert_eq!(added[0].kind.label(), "New file");
        assert_eq!(file_label(&added[0], None), "n.ts");
        let deleted = parse_patch(
            "diff --git a/d.ts b/d.ts\ndeleted file mode 100644\n--- a/d.ts\n+++ /dev/null\n@@ -1 +0,0 @@\n-bye\n",
        )
        .unwrap();
        assert_eq!(deleted[0].kind, DiffType::Delete);
        assert_eq!(file_label(&deleted[0], None), "d.ts");
        // A rename with edits reads as a modification, as in buzz-app.
        let moved = parse_patch(
            "diff --git a/x.ts b/y.ts\nsimilarity index 90%\nrename from x.ts\nrename to y.ts\n--- a/x.ts\n+++ b/y.ts\n@@ -1 +1 @@\n-a\n+b\n",
        )
        .unwrap();
        assert_eq!(moved[0].kind, DiffType::Modify);
        assert_eq!(file_label(&moved[0], Some("y.ts")), "x.ts → y.ts");
    }

    #[test]
    fn pairs_deletes_with_following_inserts_in_split_rows() {
        let files = parse_patch(
            "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1,4 +1,4 @@\n same\n-d1\n-d2\n+i1\n+i2\n same\n",
        )
        .unwrap();
        let rows = split_rows(&files[0].hunks[0].changes);
        fn text(c: Option<&Change>) -> Option<&str> {
            c.map(|c| c.content.as_str())
        }
        let rows: Vec<_> = rows.into_iter().map(|(o, n)| (text(o), text(n))).collect();
        assert_eq!(
            rows,
            [
                (Some("same"), Some("same")),
                (Some("d1"), None),
                (Some("d2"), Some("i1")),
                (None, Some("i2")),
                (Some("same"), Some("same")),
            ]
        );
    }

    fn meta(repo: &str) -> DiffMeta {
        DiffMeta {
            file_path: Some("a.ts".into()),
            repo_url: Some(repo.into()),
            commit_sha: Some("abcdef0".into()),
            description: None,
            truncated: true,
        }
    }

    #[test]
    fn links_the_short_commit_to_its_page() {
        let header = Header::new(&meta("https://example.com/repo"));
        assert_eq!(header.title, "a.ts");
        assert_eq!(header.commit_label.as_deref(), Some("abcdef0"));
        assert_eq!(
            header.source.as_deref(),
            Some("https://example.com/repo/commit/abcdef0")
        );
        let header = Header::new(&DiffMeta {
            commit_sha: Some("0123456789abcdef".into()),
            ..meta("https://example.com/")
        });
        assert_eq!(header.commit_label.as_deref(), Some("0123456"));
        assert_eq!(
            header.source.as_deref(),
            Some("https://example.com/commit/0123456789abcdef")
        );
    }

    #[test]
    fn rejects_unsafe_repositories_and_falls_back_to_source() {
        for repo in [
            "javascript:alert(1)",
            "http://example.com/repo",
            "https://user:pw@example.com/repo",
        ] {
            let header = Header::new(&meta(repo));
            assert_eq!(header.source, None, "{repo}");
            // Unlinked, the SHA is plain text.
            assert_eq!(header.commit_label.as_deref(), Some("abcdef0"), "{repo}");
        }
        // Not a SHA: the repository itself, labelled "Source" without one.
        let header = Header::new(&DiffMeta {
            commit_sha: Some("main".into()),
            ..meta("https://example.com/repo")
        });
        assert_eq!(header.source.as_deref(), Some("https://example.com/repo"));
        assert_eq!(header.commit_label.as_deref(), Some("main"));
        let header = Header::new(&DiffMeta {
            commit_sha: None,
            ..meta("https://example.com/repo")
        });
        assert_eq!(header.commit_label.as_deref(), Some("Source"));
        let header = Header::new(&DiffMeta::default());
        assert_eq!(header.title, "Diff");
        assert_eq!((header.source, header.commit_label), (None, None));
    }

    #[test]
    fn keeps_raw_content_that_looks_like_markup() {
        // DiffMessage.test.tsx: neither shape renders as Markdown or HTML.
        let markup = "<script>alert(1)</script> ![image](https://example.com/no.png)\n";
        assert_eq!(parse_patch(markup), None);
        let image_line = "diff --git a/a.ts b/a.ts\n--- a/a.ts\n+++ b/a.ts\n@@ -1 +1 @@\n-old\n+![code](https://example.com/no.png) <script>alert(1)</script>\n";
        let files = parse_patch(image_line).unwrap();
        assert_eq!(
            files[0].hunks[0].changes[1].content,
            "![code](https://example.com/no.png) <script>alert(1)</script>"
        );
        assert_eq!(
            parse_patch(&format!(
                "{image_line}UNPARSED_TRAILER <img src=x onerror=alert(1)>\n"
            )),
            None
        );
    }

    #[test]
    fn renders_new_and_deleted_single_line_files_richly() {
        for (old, new, range, line, kind) in [
            ("/dev/null", "b/a.ts", "-0,0 +1", "+new", ChangeKind::Insert),
            ("a/a.ts", "/dev/null", "-1 +0,0", "-old", ChangeKind::Delete),
        ] {
            let files = parse_patch(&format!(
                "diff --git a/a.ts b/a.ts\n--- {old}\n+++ {new}\n@@ {range} @@\n{line}\n"
            ))
            .unwrap();
            let change = &files[0].hunks[0].changes[0];
            assert_eq!((change.kind, change.content.as_str()), (kind, &line[1..]));
        }
    }

    #[test]
    fn expands_tabs_to_eight_column_stops() {
        assert_eq!(expand_tabs("\tx"), "        x");
        assert_eq!(expand_tabs("ab\tc"), "ab      c");
        assert_eq!(expand_tabs("plain"), "plain");
    }
}
