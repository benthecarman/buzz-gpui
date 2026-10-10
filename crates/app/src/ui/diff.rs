//! Diff messages (buzz-app's bundled `diffs` plugin): a kind-40008 row
//! renders as a card with a unified preview of its patch, and an expanded
//! dialog with a unified/split toggle, in place of Markdown.
//!
//! buzz-app parses patches with react-diff-view's `parseDiff`
//! (gitdiff-parser 0.3.1) and then validates them strictly (`parse.ts`).
//! The parser is ported quirk for quirk, because the validation is defined
//! against what it returns: a patch that is cut off, binary, in an unknown
//! format or over 100k stays readable as raw text instead.

use std::borrow::Cow;

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
}
