//! The `## Todos` section of a channel Canvas (`bundled/todos/model.ts`).
//!
//! Items are read from the CommonMark structure, so examples in code
//! blocks, quotes and nested lists are not todos. Each edit changes only
//! the bytes it is about: a status mark, an inserted line, or the
//! assignment suffix, and leaves every other byte of the Canvas alone.
//! Offsets are byte offsets into the Canvas text.

use markdown::mdast::Node;
use regex::Regex;
use std::sync::LazyLock;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Assignee {
    pub pubkey: String,
    pub name: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Status {
    Todo,
    Doing,
    Done,
}

impl Status {
    /// `[/]` is the common Markdown convention for an in-progress task.
    fn mark(self) -> &'static str {
        match self {
            Self::Todo => " ",
            Self::Doing => "/",
            Self::Done => "x",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemAssignee {
    pub pubkey: String,
    pub name: String,
    /// Where the ` · Assignee: ` suffix starts.
    pub start: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Todo {
    /// The status mark's byte.
    pub offset: usize,
    pub label: String,
    pub status: Status,
    /// The end of the item's first line, without trailing white space.
    pub end: usize,
    pub assignee: Option<ItemAssignee>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Todos {
    pub items: Vec<Todo>,
    /// The end of the `## Todos` heading, when there is one.
    pub start: Option<usize>,
}

const ASSIGNMENT: &str = " · Assignee: ";

static ITEM: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^([-+*][ \t]+\[)([ xX/])\][ \t]+([^\r\n]+)").unwrap());

/// JavaScript's `trim()`.
fn js_trim(text: &str) -> &str {
    text.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}')
}

fn js_trim_end(text: &str) -> &str {
    text.trim_end_matches(|c: char| c.is_whitespace() || c == '\u{feff}')
}

fn start_offset(node: &Node) -> Option<usize> {
    node.position().map(|position| position.start.offset)
}

fn end_offset(node: &Node) -> Option<usize> {
    node.position().map(|position| position.end.offset)
}

fn is_todos_heading(node: &Node) -> bool {
    match node {
        Node::Heading(heading) => {
            heading.depth == 2
                && matches!(heading.children.as_slice(), [Node::Text(text)] if text.value == "Todos")
        }
        _ => false,
    }
}

/// `nostr:npub1…` for a 64-character hex key (`profileTarget`). Like
/// buzz-app it encodes the bytes without checking they are a curve point.
pub fn profile_target(pubkey: &str) -> Option<String> {
    if pubkey.len() != 64 || !pubkey.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let bytes: Vec<u8> = (0..32)
        .map(|i| u8::from_str_radix(&pubkey[i * 2..i * 2 + 2], 16).unwrap())
        .collect();
    let hrp = bech32::Hrp::parse("npub").ok()?;
    let npub = bech32::encode::<bech32::Bech32>(hrp, &bytes).ok()?;
    Some(format!("nostr:{npub}"))
}

/// The key a `nostr:npub1…` link names (`profileKey`), lowercase hex.
pub fn profile_key(target: &str) -> Option<String> {
    let npub = target.strip_prefix("nostr:")?;
    let data = npub.strip_prefix("npub1")?;
    if data.is_empty()
        || !data
            .bytes()
            .all(|b| b"023456789acdefghjklmnpqrstuvwxyz".contains(&b))
    {
        return None;
    }
    let (hrp, bytes) = bech32::decode(npub).ok()?;
    if hrp.as_str() != "npub" || bytes.len() != 32 {
        return None;
    }
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    // Only the canonical Bech32 (not Bech32m) spelling names the key.
    (profile_target(&hex)?.strip_prefix("nostr:")? == npub).then_some(hex)
}

/// The todos in the Canvas's one `## Todos` section.
pub fn read_todos(content: &str) -> Result<Todos, String> {
    let tree = markdown::to_mdast(content, &markdown::ParseOptions::default())
        .map_err(|error| error.to_string())?;
    let nodes = tree.children().map(Vec::as_slice).unwrap_or_default();
    let headings: Vec<usize> = nodes
        .iter()
        .enumerate()
        .filter(|(_, node)| is_todos_heading(node))
        .map(|(index, _)| index)
        .collect();
    if headings.len() > 1 {
        return Err("Canvas has more than one Todos section. Keep one ## Todos heading in Canvas before editing here.".into());
    }
    let heading = headings.first().copied();
    let start = heading.and_then(|index| end_offset(&nodes[index]));
    let following = heading.map_or(&[][..], |index| &nodes[index + 1..]);
    let boundary = following
        .iter()
        .position(|node| matches!(node, Node::Heading(h) if h.depth <= 2))
        .unwrap_or(following.len());
    let mut items = Vec::new();
    for node in &following[..boundary] {
        let Node::List(list) = node else { continue };
        if list.ordered {
            continue;
        }
        for item in &list.children {
            let Some(offset) = start_offset(item) else {
                continue;
            };
            let rest = &content[offset..];
            let Some(found) = ITEM.captures(rest) else {
                continue;
            };
            let (whole, opening, mark, text) = (&found[0], &found[1], &found[2], &found[3]);
            let end = offset + js_trim_end(whole).len();
            let link = match item.children().and_then(|children| children.first()) {
                Some(Node::Paragraph(paragraph)) => paragraph
                    .children
                    .iter()
                    .find(|child| matches!(child, Node::Link(_)) && end_offset(child) == Some(end)),
                _ => None,
            };
            let assignee = link.and_then(|node| {
                let Node::Link(link) = node else { return None };
                let link_start = start_offset(node)?;
                let pubkey = profile_key(&link.url)?;
                let name = match link.children.as_slice() {
                    [Node::Text(text)] if !text.value.is_empty() => text.value.clone(),
                    _ => return None,
                };
                let target = profile_target(&pubkey)?;
                let source = content.get(link_start..end)?;
                let prefix = link_start
                    .checked_sub(ASSIGNMENT.len())
                    .and_then(|from| content.get(from..link_start));
                (link.title.is_none()
                    && source.ends_with(&format!("]({target})"))
                    && prefix == Some(ASSIGNMENT))
                .then(|| ItemAssignee {
                    pubkey,
                    name,
                    start: link_start - ASSIGNMENT.len(),
                })
            });
            let label_start = offset + whole.len() - text.len();
            let label_end = assignee.as_ref().map_or(end, |a| a.start);
            items.push(Todo {
                offset: offset + opening.len(),
                label: js_trim(content.get(label_start..label_end).unwrap_or_default()).to_owned(),
                status: match mark {
                    " " => Status::Todo,
                    "/" => Status::Doing,
                    _ => Status::Done,
                },
                end,
                assignee,
            });
        }
    }
    Ok(Todos { items, start })
}

const CHANGED: &str = "This todo changed. Reload the Canvas before editing it.";

pub fn set_status(content: &str, offset: usize, status: Status) -> Result<String, String> {
    if !read_todos(content)?
        .items
        .iter()
        .any(|item| item.offset == offset)
    {
        return Err(CHANGED.into());
    }
    Ok(format!(
        "{}{}{}",
        &content[..offset],
        status.mark(),
        &content[offset + 1..]
    ))
}

pub fn add_todo(content: &str, label: &str) -> Result<String, String> {
    let text = js_trim(label);
    if text.is_empty() || text.contains(['\r', '\n']) {
        return Err("Use a single line for your todo.".into());
    }
    let Todos { start, items } = read_todos(content)?;
    let newline = if content.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let line = format!("- [ ] {text}");
    let next = match start {
        None => format!(
            "{content}{}## Todos{newline}{newline}{line}{newline}",
            if content.is_empty() {
                String::new()
            } else {
                format!("{newline}{newline}")
            }
        ),
        Some(start) => format!(
            "{}{newline}{newline}{line}{newline}{}",
            &content[..start],
            &content[start..]
        ),
    };
    if read_todos(&next)?.items.len() != items.len() + 1 {
        return Err(
            "Could not add a todo safely. Check Canvas for unfinished Markdown blocks.".into(),
        );
    }
    Ok(next)
}

/// Markdown punctuation a name is escaped against.
fn escape(name: &str) -> String {
    let mut escaped = String::with_capacity(name.len());
    for c in name.chars() {
        if "\\`*_{}[]()#+-.!<>|&".contains(c) {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    escaped
}

/// Edits only the readable assignment suffix, leaving the rest of the
/// Canvas intact. `None` clears it.
pub fn assign_todo(
    content: &str,
    offset: usize,
    assignee: Option<&Assignee>,
) -> Result<String, String> {
    let items = read_todos(content)?.items;
    let item = items
        .iter()
        .find(|item| item.offset == offset)
        .ok_or(CHANGED)?;
    let target = assignee.map(|assignee| profile_target(&assignee.pubkey));
    if let Some(None) = target {
        return Err("Choose a valid Buzz user.".into());
    }
    let suffix = match (assignee, target.flatten()) {
        (Some(assignee), Some(target)) => {
            let collapsed = assignee
                .name
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            let name = if collapsed.is_empty() {
                assignee.pubkey.clone()
            } else {
                collapsed
            };
            format!("{ASSIGNMENT}[{}]({target})", escape(&name))
        }
        _ => String::new(),
    };
    let start = item.assignee.as_ref().map_or(item.end, |a| a.start);
    let next = format!("{}{suffix}{}", &content[..start], &content[item.end..]);
    let parsed = read_todos(&next)?.items;
    let edited = parsed.iter().find(|value| value.offset == offset);
    if parsed.len() != items.len()
        || edited.map(|e| &e.label) != Some(&item.label)
        || edited.and_then(|e| e.assignee.as_ref().map(|a| a.pubkey.clone()))
            != assignee.map(|a| a.pubkey.to_ascii_lowercase())
    {
        return Err("Could not assign safely. Check this todo’s Markdown in Canvas.".into());
    }
    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    fn labels(content: &str) -> Vec<String> {
        read_todos(content)
            .unwrap()
            .items
            .into_iter()
            .map(|item| item.label)
            .collect()
    }

    fn first(content: &str) -> Todo {
        read_todos(content).unwrap().items.remove(0)
    }

    fn alex() -> Assignee {
        Assignee {
            pubkey: "a".repeat(64),
            name: "Alex".into(),
        }
    }

    fn sam() -> Assignee {
        Assignee {
            pubkey: "b".repeat(64),
            name: "Sam".into(),
        }
    }

    fn target(pubkey: &str) -> String {
        profile_target(pubkey).unwrap()
    }

    #[test]
    fn changes_only_the_selected_checkbox_byte_with_duplicates_and_crlf() {
        let content = "# Notes\r\nuntouched  \r\n\r\n## Todos\r\n\r\n- [ ] Same\r\n- [X] Same\r\n\r\n## Keep\r\n- [ ] Not a todo\r\n";
        let items = read_todos(content).unwrap().items;
        let seen: Vec<_> = items.iter().map(|i| (i.label.as_str(), i.status)).collect();
        assert_eq!(seen, [("Same", Status::Todo), ("Same", Status::Done)]);
        let changed = set_status(content, items[1].offset, Status::Todo).unwrap();
        assert_eq!(changed, content.replace("[X]", "[ ]"));
        assert_eq!(
            set_status(&changed, items[1].offset, Status::Done).unwrap(),
            content.replace("[X]", "[x]")
        );
    }

    #[test]
    fn reads_and_writes_doing_as_slash() {
        let content = "## Todos\n\n- [ ] Plan\n* [/] Build · note\n- [x] Ship\n";
        let items = read_todos(content).unwrap().items;
        let seen: Vec<_> = items.iter().map(|i| (i.label.as_str(), i.status)).collect();
        assert_eq!(
            seen,
            [
                ("Plan", Status::Todo),
                ("Build · note", Status::Doing),
                ("Ship", Status::Done)
            ]
        );
        let doing = set_status(content, items[0].offset, Status::Doing).unwrap();
        assert_eq!(doing, content.replace("- [ ] Plan", "- [/] Plan"));
        assert_eq!(
            set_status(&doing, items[0].offset, Status::Todo).unwrap(),
            content
        );
        assert_eq!(
            set_status(content, items[1].offset, Status::Done).unwrap(),
            content.replace("* [/]", "* [x]")
        );
        assert!(
            read_todos("## Todos\n- [-] Other\n- [//] Mark\n")
                .unwrap()
                .items
                .is_empty()
        );
    }

    #[test]
    fn recognizes_markdown_structure_not_examples_or_nested_lists() {
        let content = "```md\n## Todos\n- [ ] Fenced\n```\n\n## Todos\n\n- [ ] Real\n  - [ ] Nested\n\n    [ ] Continuation\n\n> - [ ] Quoted\n\n```\n- [ ] Code\n```\n\n### Details\n\n+ [x] Also real\n\n# Next\n\n- [ ] Other\n";
        assert_eq!(labels(content), ["Real", "Also real"]);
    }

    #[test]
    fn adding_preserves_prose_sections_markers_and_trailing_space() {
        let before = "Preamble  \r\n\r\n## Todos ###\r\n\r\nNotes  \r\n* [x] Old\r\n\r\n## Other\r\nKeep exactly.  ";
        let next = add_todo(before, " New **task** ").unwrap();
        assert_eq!(
            next,
            before.replace("## Todos ###", "## Todos ###\r\n\r\n- [ ] New **task**\r\n")
        );
        assert_eq!(labels(&next), ["New **task**", "Old"]);
    }

    #[test]
    fn creates_one_ordinary_section_without_rewriting_content() {
        for content in ["", "# Notes\nKeep me.", "# Notes\r\nKeep me."] {
            let next = add_todo(content, "First").unwrap();
            assert!(next.starts_with(content));
            assert_eq!(labels(&next), ["First"]);
            assert_eq!(
                labels(&add_todo(&next, "Second").unwrap()),
                ["Second", "First"]
            );
        }
    }

    #[test]
    fn rejects_ambiguous_multiline_stale_and_swallowed_edits() {
        assert!(
            read_todos("## Todos\n\n## Todos\n")
                .unwrap_err()
                .contains("more than one")
        );
        assert!(
            add_todo("", "a\n## Escape")
                .unwrap_err()
                .contains("single line")
        );
        assert!(add_todo("", "   ").unwrap_err().contains("single line"));
        assert!(
            set_status("## Todos\n- [ ] Hi", 0, Status::Done)
                .unwrap_err()
                .contains("changed")
        );
        assert!(
            add_todo("```unterminated\n", "Oops")
                .unwrap_err()
                .contains("safely")
        );
        assert!(
            add_todo("<!-- open comment\n", "Oops")
                .unwrap_err()
                .contains("safely")
        );
    }

    #[test]
    fn assigns_replaces_and_clears_only_the_suffix() {
        let before = "# Notes\r\nKeep this.\r\n\r\n## Todos\r\n\r\n* [X] Ship **it**  \r\n  - Keep nested.\r\n\r\n## Decisions\r\nUntouched.";
        let item = first(before);
        let (alex, sam) = (alex(), sam());
        let assigned = assign_todo(before, item.offset, Some(&alex)).unwrap();
        assert_eq!(
            assigned,
            before.replace(
                "Ship **it**",
                &format!("Ship **it** · Assignee: [Alex]({})", target(&alex.pubkey))
            )
        );
        let read = first(&assigned).assignee.unwrap();
        assert_eq!((read.pubkey, read.name), (alex.pubkey.clone(), alex.name));
        assert_eq!(
            set_status(&assigned, item.offset, Status::Doing).unwrap(),
            assigned.replace("[X]", "[/]")
        );
        let changed = assign_todo(&assigned, item.offset, Some(&sam)).unwrap();
        assert_eq!(
            changed,
            assigned.replace(
                &format!("[Alex]({})", target(&alex.pubkey)),
                &format!("[Sam]({})", target(&sam.pubkey))
            )
        );
        assert_eq!(assign_todo(&changed, item.offset, None).unwrap(), before);
    }

    #[test]
    fn round_trips_untrusted_names() {
        for name in [
            "A] (B) \\ *C* &amp; <script>",
            "A\nB\rC",
            "",
            "👑 Queen [test]",
        ] {
            let before = "## Todos\n- [ ] First\n- [ ] Second\n";
            let item = first(before);
            let assignee = Assignee {
                name: name.into(),
                ..alex()
            };
            let assigned = assign_todo(before, item.offset, Some(&assignee)).unwrap();
            let read = first(&assigned).assignee.unwrap();
            let collapsed = name.split_whitespace().collect::<Vec<_>>().join(" ");
            let expected = if collapsed.is_empty() {
                alex().pubkey
            } else {
                collapsed
            };
            assert_eq!(
                (read.pubkey, read.name),
                (alex().pubkey, expected),
                "{name:?}"
            );
            assert_eq!(assign_todo(&assigned, item.offset, None).unwrap(), before);
        }
    }

    #[test]
    fn preserves_lookalike_prose() {
        let alex = target(&alex().pubkey);
        for label in [
            format!("Task [Alex]({alex})"),
            format!("Task · Assignee: [Alex]({alex} \"title\")"),
            format!("Task · Assignee: [Alex](<{alex}>)"),
            "Task · Assignee: [Alex](https://example.com)".to_owned(),
            "Task · Assignee: [Alex](nostr:npub1invalid)".to_owned(),
            format!("Task · Assignee: [Alex]({alex}) after"),
            format!("Task ` · Assignee: [Alex]({alex})`"),
            format!("Task · Assignee: **[Alex]({alex})**"),
        ] {
            let before = format!("## Todos\n- [ ] {label}\n");
            let item = first(&before);
            assert_eq!(item.assignee, None, "{label}");
            assert_eq!(assign_todo(&before, item.offset, None).unwrap(), before);
            let next = assign_todo(&before, item.offset, Some(&sam())).unwrap();
            assert_eq!(first(&next).label, label);
            assert_eq!(assign_todo(&next, item.offset, None).unwrap(), before);
        }
    }

    #[test]
    fn refuses_swallowed_assignments_invalid_users_and_stale_offsets() {
        let before = "## Todos\n- [ ] Task `unfinished\n  closed`";
        let offset = first(before).offset;
        assert!(
            assign_todo(before, offset, Some(&alex()))
                .unwrap_err()
                .contains("safely")
        );
        assert!(
            assign_todo("## Todos\n- [ ] Task", 0, Some(&alex()))
                .unwrap_err()
                .contains("changed")
        );
        let bad = Assignee {
            pubkey: "bad".into(),
            ..alex()
        };
        assert!(
            assign_todo(before, offset, Some(&bad))
                .unwrap_err()
                .contains("valid")
        );
    }

    #[test]
    fn reads_a_first_line_assignment_beside_continuation_links() {
        let before = format!(
            "## Todos\n- [ ] Task · Assignee: [Alex]({})\n  See [docs](https://example.com).\n",
            target(&alex().pubkey)
        );
        let item = first(&before);
        assert_eq!(item.assignee.as_ref().unwrap().pubkey, alex().pubkey);
        assert_eq!(
            assign_todo(&before, item.offset, None).unwrap(),
            "## Todos\n- [ ] Task\n  See [docs](https://example.com).\n"
        );
    }

    #[test]
    fn profile_links_round_trip() {
        let key = "b".repeat(64);
        assert_eq!(profile_key(&target(&key)), Some(key));
        assert_eq!(profile_key("nostr:npub1invalid"), None);
        assert_eq!(profile_target("bad"), None);
    }
}
