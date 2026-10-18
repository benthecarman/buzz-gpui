//! Composer tools (`conversation.registerTool`, `ComposerTools.tsx`):
//! buttons a plugin adds to every message composer's toolbar, such as
//! Projects' "Issues and pull requests" picker.
//!
//! The composer makes one view per tool for each composer, so a tool owns
//! state for its conversation. Tools sit between the emoji and voice note
//! buttons, ordered by `order` and then by key (buzz-app sorts the bundled
//! emoji tool at 0 and voice notes at 10, so plugins at 0 follow emoji).

use super::composer::Composer;
use crate::community::Community;
use gpui_kit::*;
use std::rc::Rc;

/// What a tool knows about the composer it sits in.
#[derive(Clone)]
pub struct ToolContext {
    pub community: Entity<Community>,
    pub channel: String,
    /// The thread the composer replies in, as hex.
    pub thread: Option<String>,
    pub composer: WeakEntity<Composer>,
}

#[derive(Clone)]
pub struct ComposerTool {
    /// `plugin/tool`, such as `buzz.projects/resources`.
    pub key: &'static str,
    pub order: i32,
    /// An accessory (`registerAccessory`): shown above the composer, beside
    /// the typing indicator, rather than in the toolbar.
    pub accessory: bool,
    pub build: BuildTool,
}

/// Makes a tool's view for one composer.
pub type BuildTool = Rc<dyn Fn(ToolContext, &mut Window, &mut App) -> AnyView>;

/// The tools, and a count of changes so open composers can catch up.
#[derive(Default)]
struct Tools(Vec<ComposerTool>, u64);

impl Global for Tools {}

/// Adds a tool, replacing any earlier one with the same key.
pub fn register(tool: ComposerTool, cx: &mut App) {
    let tools = &mut cx.default_global::<Tools>().0;
    tools.retain(|existing| existing.key != tool.key);
    tools.push(tool);
    tools.sort_by(|a, b| a.order.cmp(&b.order).then_with(|| a.key.cmp(b.key)));
    cx.default_global::<Tools>().1 += 1;
}

/// Every tool as `(key, order, accessory)`, in toolbar order.
pub fn listed(cx: &App) -> Vec<(&'static str, i32, bool)> {
    cx.try_global::<Tools>().map_or_else(Vec::new, |tools| {
        tools
            .0
            .iter()
            .map(|tool| (tool.key, tool.order, tool.accessory))
            .collect()
    })
}

/// Changes whenever a tool is added or removed.
pub fn generation(cx: &App) -> u64 {
    cx.try_global::<Tools>().map_or(0, |tools| tools.1)
}

/// Removes the tool `key`, as when the plugin that added it stops.
pub fn unregister(key: &str, cx: &mut App) {
    let tools = cx.default_global::<Tools>();
    tools.0.retain(|existing| existing.key != key);
    tools.1 += 1;
}

/// One view per registered toolbar tool for a new composer.
pub fn build(context: ToolContext, window: &mut Window, cx: &mut App) -> Vec<AnyView> {
    build_where(false, context, window, cx)
}

/// One view per registered accessory for a new composer.
pub fn build_accessories(context: ToolContext, window: &mut Window, cx: &mut App) -> Vec<AnyView> {
    build_where(true, context, window, cx)
}

fn build_where(
    accessory: bool,
    context: ToolContext,
    window: &mut Window,
    cx: &mut App,
) -> Vec<AnyView> {
    let tools = cx
        .try_global::<Tools>()
        .map(|tools| tools.0.clone())
        .unwrap_or_default();
    // A switched-off plugin's tools stay out of new composers.
    tools
        .into_iter()
        .filter(|tool| tool.accessory == accessory && crate::plugins::enabled_key(tool.key))
        .map(|tool| (tool.build)(context.clone(), window, cx))
        .collect()
}

/// The editor's `maxLength`.
const MAX_CHARS: usize = 16_000;
/// At most this many resource links in one message.
const MAX_RESOURCES: usize = 32;

/// `composerResource`: the label as one line of at most 120 characters,
/// escaped into the Markdown link the message carries. `None` when the
/// label is empty or the URI is not a Buzz or web link.
pub fn resource_source(uri: &str, label: &str) -> Option<(String, String)> {
    let mut text = String::new();
    let mut gap = false;
    for c in label.chars() {
        let space = c.is_control()
            || c.is_whitespace()
            || ('\u{202a}'..='\u{202e}').contains(&c)
            || ('\u{2066}'..='\u{2069}').contains(&c);
        if space {
            gap = true;
        } else {
            if gap && !text.is_empty() {
                text.push(' ');
            }
            gap = false;
            text.push(c);
        }
    }
    let text: String = text
        .chars()
        .take(120)
        .collect::<String>()
        .trim_end()
        .to_string();
    let linkable = buzz_relay_client::projects::EntityRoute::parse_link(uri).is_some()
        || buzz_relay_client::parse_buzz_link(uri).is_some()
        || matches!(url::Url::parse(uri), Ok(url) if matches!(url.scheme(), "https" | "http"));
    if text.is_empty() || !linkable || uri.contains(char::is_whitespace) || uri.contains(')') {
        return None;
    }
    let mut escaped = String::new();
    for c in text.chars() {
        if "\\`[]*_~<>&|".contains(c) {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    Some((format!("[{escaped}]({uri})"), text))
}

/// Where the selection sits in the draft's Markdown.
#[derive(Debug, PartialEq, Eq)]
enum Place {
    Code,
    Link,
}

/// Whether `selection` is in code, or cuts into a link (Markdown links
/// cannot nest).
fn place(text: &str, selection: std::ops::Range<usize>) -> Option<Place> {
    use markdown::mdast::Node;
    let tree = markdown::to_mdast(text, &markdown::ParseOptions::gfm()).ok()?;
    let (start, end) = (selection.start, selection.end);
    let mut found = None;
    let mut stack = vec![&tree];
    while let Some(node) = stack.pop() {
        if let Some(position) = node.position() {
            let (first, last) = (position.start.offset, position.end.offset);
            match node {
                Node::InlineCode(_) if first < end && last > start => return Some(Place::Code),
                Node::Code(_) => {
                    let block = text[first..last].trim_end();
                    let closed = block.lines().count() > 1
                        && (block.ends_with("```") || block.ends_with("~~~"));
                    if first < end && (start < last || (!closed && start == last)) {
                        return Some(Place::Code);
                    }
                }
                Node::Link(_) | Node::LinkReference(_)
                    if first < end && last > start && !(start <= first && end >= last) =>
                {
                    found = Some(Place::Link);
                }
                _ => {}
            }
        }
        if let Some(children) = node.children() {
            stack.extend(children.iter());
        }
    }
    found
}

/// `insertResource`: puts a link to `uri`, labelled `label`, at the
/// composer's caret as one atom followed by a space. The error is the
/// reason the tool shows.
pub fn insert_resource(
    composer: &Entity<Composer>,
    uri: &str,
    label: &str,
    window: &mut Window,
    cx: &mut App,
) -> Result<(), &'static str> {
    use gpui_kit::component::input::InlineToken;
    if composer.read(cx).sending() {
        return Err("The message can't be edited right now");
    }
    let (source, label) = resource_source(uri, label).ok_or("This link can't be added")?;
    let input = composer.read(cx).input();
    let (text, selection, links) = {
        let state = input.read(cx);
        let links = state
            .tokens()
            .iter()
            .filter(|span| span.token().id().starts_with("buzz:"))
            .count();
        (state.value().to_string(), state.selected_range(), links)
    };
    match place(&text, selection.clone()) {
        Some(Place::Code) => return Err("Links can't be added inside code"),
        Some(Place::Link) => return Err("Links can't be added inside another link"),
        None => {}
    }
    if links >= MAX_RESOURCES {
        return Err("Add at most 32 links to one message");
    }
    // A preceding `!` would make an image; `\` would escape the bracket.
    let gap = text[..selection.start].ends_with(['!', '\\']);
    let length = text[..selection.start].chars().count()
        + text[selection.end..].chars().count()
        + source.chars().count()
        + 1
        + usize::from(gap);
    if length > MAX_CHARS {
        return Err("Message is too long to add this link");
    }
    let token = InlineToken::new(uri.to_string(), source).with_label(label);
    let inserted = input.update(cx, |state, cx| {
        let mut at = selection.clone();
        if gap {
            state.set_selected_range(at.clone(), cx);
            state.replace(" ", window, cx);
            at = at.start + 1..at.start + 1;
        }
        let inserted = state
            .replace_range_with_token(at, token, window, cx)
            .is_ok();
        if inserted {
            state.insert(" ", window, cx);
        }
        state.focus(window, cx);
        inserted
    });
    if inserted {
        Ok(())
    } else {
        Err("This link can't be added")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn resource_labels_are_one_escaped_line() {
        let uri = format!(
            "buzz://issue?id={}&owner={}&d=repo",
            "c".repeat(64),
            "a".repeat(64)
        );
        let (source, label) = resource_source(&uri, "  Fix *the*\n[reader]\u{202e} ").unwrap();
        assert_eq!(label, "Fix *the* [reader]");
        assert_eq!(source, format!("[Fix \\*the\\* \\[reader\\]]({uri})"));
        let long = "x".repeat(200);
        assert_eq!(resource_source(&uri, &long).unwrap().1.chars().count(), 120);
        assert_eq!(resource_source(&uri, " \n "), None);
        assert_eq!(resource_source("javascript:alert(1)", "x"), None);
        assert_eq!(resource_source("buzz://repo?owner=x", "x"), None);
    }

    #[test]
    fn finds_code_and_links_around_the_selection() {
        assert_eq!(place("see `co de` here", 7..7), Some(Place::Code));
        assert_eq!(place("see `code` here", 11..11), None);
        assert_eq!(place("```\nfn x\n", 7..7), Some(Place::Code));
        assert_eq!(place("```\nfn x\n```\nafter", 18..18), None);
        assert_eq!(
            place("a [label](https://x.test) b", 5..5),
            Some(Place::Link)
        );
        // Replacing a whole link is not nesting.
        assert_eq!(place("a [label](https://x.test) b", 2..25), None);
        assert_eq!(place("plain text", 3..3), None);
    }
}
