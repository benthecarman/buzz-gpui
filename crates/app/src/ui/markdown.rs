//! Message bodies (ui.md §4.4 body text, §4.6), after buzz-app's
//! `MessageMarkdown.tsx`.
//!
//! Blocks are laid out here, to the spec's margins, quote rule, list
//! gutters, code block and table frames. gpui-kit's `TextView` hard-codes
//! those, so it only lays out inline text: each paragraph, heading and list
//! line is re-serialized from the parsed tree into a one-paragraph Markdown
//! source for a `TextView`, which wraps it, keeps it selectable and places
//! links, mentions, custom emoji, spoilers and code spans as native inline
//! elements through a [`MarkdownPlugin`].
//!
//! Preparing a body binds names to signed recipients, finds custom emoji
//! and spoilers in prose only (never in code or link labels), turns single
//! newlines into hard breaks (remark-breaks) and drops raw HTML
//! (`skipHtml`). Bodies over the size or depth limits render as plain text.

use super::media;
use super::navigation;
use crate::deep_links::{self, Link};
use buzz_relay_client::fold::{CustomEmoji, MessageRow, message_markdown};
use buzz_relay_client::{ChannelSummary, ChannelType, Media, Profiles};
use buzz_ui::{ActiveTheme, Icon, IconName, TypeStyled, tokens::font};
use gpui_kit::base::text::{
    InlineElement, InlineRenderContext, MarkdownExtensions, MarkdownNode, MarkdownParseContext,
    MarkdownPlugin, TextView, TextViewStyle,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use markdown::mdast::{self, AlignKind, Node};
use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;

/// Everything outside the row a body needs to render.
#[derive(Clone)]
pub struct Directory {
    pub profiles: Profiles,
    pub channels: Arc<[ChannelSummary]>,
    pub media: Media,
}

/// Renders a row's body.
pub fn body(row: &Arc<MessageRow>, directory: &Directory, cx: &App) -> AnyElement {
    let document = prepared(row, &directory.profiles);
    let c = cx.colors();
    let objects = Objects {
        directory: directory.clone(),
        emoji: row.emoji.clone().into(),
        row: row.id.to_hex().into(),
    };
    match &*document {
        Document::Plain(text) => div()
            .text_body_sm()
            .text_color(c.text_standard)
            .child(text.clone())
            .into_any_element(),
        Document::Emoji(parts) => jumbo(parts, &directory.media),
        Document::Blocks(blocks) => {
            let mut out = Out {
                objects,
                cx,
                quote: false,
            };
            column(blocks, Container::Root, "b", &mut out).into_any_element()
        }
    }
}

// ---------------------------------------------------------------------------
// Preparation

#[derive(Debug, PartialEq)]
enum Document {
    Plain(String),
    /// Emoji only (`emoji-size.ts`): drawn at 42px.
    Emoji(Vec<EmojiPart>),
    Blocks(Vec<Block>),
}

#[derive(Debug, PartialEq)]
enum EmojiPart {
    Text(String),
    Custom(CustomEmoji),
}

#[derive(Debug, PartialEq)]
enum Block {
    /// One-paragraph Markdown for a `TextView`.
    Paragraph(SharedString),
    Heading(u8, SharedString),
    Quote(Vec<Block>),
    List {
        ordered: bool,
        start: u32,
        items: Vec<Item>,
    },
    Code(String),
    Table {
        align: Vec<AlignKind>,
        rows: Vec<Vec<Vec<Span>>>,
    },
    Rule,
}

#[derive(Debug, PartialEq)]
struct Item {
    checked: Option<bool>,
    blocks: Vec<Block>,
}

/// A styled run of a table cell. Cells stay on one line, so they are drawn
/// as plain styled text.
#[derive(Debug, Default, PartialEq, Clone)]
struct Span {
    text: String,
    strong: bool,
    emphasis: bool,
    delete: bool,
    code: bool,
    link: bool,
}

thread_local! {
    /// Prepared bodies by their inputs, so a redraw does not reparse.
    static PREPARED: RefCell<HashMap<u64, Arc<Document>>> = RefCell::new(HashMap::new());
}
const PREPARED_LIMIT: usize = 2048;

fn prepared(row: &MessageRow, profiles: &Profiles) -> Arc<Document> {
    let identities = identities(row);
    let mut hasher = DefaultHasher::new();
    row.content.hash(&mut hasher);
    row.edited.hash(&mut hasher);
    row.attachment_seams.hash(&mut hasher);
    row.emoji.hash(&mut hasher);
    for key in &identities {
        key.hash(&mut hasher);
        profiles.get(key).map(|p| &p.name).hash(&mut hasher);
    }
    let key = hasher.finish();
    if let Some(document) = PREPARED.with_borrow(|cache| cache.get(&key).cloned()) {
        return document;
    }
    let document = Arc::new(prepare(row, profiles, &identities));
    PREPARED.with_borrow_mut(|cache| {
        if cache.len() >= PREPARED_LIMIT {
            cache.clear();
        }
        cache.insert(key, document.clone());
    });
    document
}

fn identities(row: &MessageRow) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    for key in row.mentions.iter().chain(&row.mention_references) {
        let hex = key.to_hex();
        if !keys.contains(&hex) {
            keys.push(hex);
        }
    }
    keys
}

fn prepare(row: &MessageRow, profiles: &Profiles, identities: &[String]) -> Document {
    let content = &row.content;
    if is_emoji_only(content, &row.emoji) {
        return Document::Emoji(emoji_parts(content.trim(), &row.emoji));
    }
    let Some(tree) = message_markdown(content) else {
        return Document::Plain(content.clone());
    };
    // `profileMentionParts`: exact known names of signed recipients, only
    // in an unedited body.
    let mut names: Vec<(String, Vec<String>)> = Vec::new();
    if !row.edited {
        for key in identities {
            let Some(name) = profiles.get(key).map(|p| p.name.clone()) else {
                continue;
            };
            if name.is_empty() || name.contains(['\r', '\n']) {
                continue;
            }
            match names.iter_mut().find(|(n, _)| *n == name) {
                Some((_, keys)) => keys.push(key.clone()),
                None => names.push((name, vec![key.clone()])),
            }
        }
    }
    names.sort_by_key(|(name, _)| std::cmp::Reverse(name.len()));
    let mut definitions = HashMap::new();
    collect_definitions(&tree, &mut definitions);
    let mut prep = Prep {
        content,
        emoji: &row.emoji,
        names,
        seams: &row.attachment_seams,
        definitions,
        spoilers: 0,
    };
    Document::Blocks(prep.blocks(tree.children().map_or(&[][..], Vec::as_slice)))
}

fn collect_definitions(node: &Node, out: &mut HashMap<String, String>) {
    if let Node::Definition(definition) = node {
        out.entry(definition.identifier.to_lowercase())
            .or_insert_with(|| definition.url.clone());
    }
    for child in node.children().into_iter().flatten() {
        collect_definitions(child, out);
    }
}

struct Prep<'a> {
    content: &'a str,
    emoji: &'a [CustomEmoji],
    names: Vec<(String, Vec<String>)>,
    seams: &'a [usize],
    definitions: HashMap<String, String>,
    spoilers: usize,
}

impl Prep<'_> {
    fn blocks(&mut self, nodes: &[Node]) -> Vec<Block> {
        nodes.iter().filter_map(|node| self.block(node)).collect()
    }

    fn block(&mut self, node: &Node) -> Option<Block> {
        Some(match node {
            Node::Paragraph(paragraph) => {
                let text = self.inline(&paragraph.children);
                if text.trim().is_empty() {
                    return None;
                }
                Block::Paragraph(text.into())
            }
            Node::Heading(heading) => {
                Block::Heading(heading.depth, self.inline(&heading.children).into())
            }
            Node::Blockquote(quote) => Block::Quote(self.blocks(&quote.children)),
            Node::List(list) => Block::List {
                ordered: list.ordered,
                start: list.start.unwrap_or(1),
                items: list
                    .children
                    .iter()
                    .filter_map(|item| match item {
                        Node::ListItem(item) => Some(Item {
                            checked: item.checked,
                            blocks: self.blocks(&item.children),
                        }),
                        _ => None,
                    })
                    .collect(),
            },
            Node::Code(code) => Block::Code(code.value.clone()),
            Node::Table(table) => Block::Table {
                align: table.align.clone(),
                rows: table
                    .children
                    .iter()
                    .map(|row| {
                        row.children()
                            .into_iter()
                            .flatten()
                            .map(|cell| {
                                let mut spans = Vec::new();
                                let children = cell.children().map_or(&[][..], Vec::as_slice);
                                self.spans(children, &Span::default(), &mut spans);
                                spans
                            })
                            .collect()
                    })
                    .collect(),
            },
            Node::ThematicBreak(_) => Block::Rule,
            // Raw HTML is skipped; definitions only resolve references.
            _ => return None,
        })
    }

    /// One-paragraph Markdown for `nodes`, every ASCII punctuation mark of
    /// prose escaped so nothing but the structure written here parses.
    fn inline(&mut self, nodes: &[Node]) -> String {
        let mut out = String::new();
        for node in nodes {
            self.inline_node(node, &mut out);
        }
        out.trim_end_matches(['\\', '\n']).to_string()
    }

    fn inline_node(&mut self, node: &Node, out: &mut String) {
        match node {
            Node::Text(text) => self.text(&text.value, node, out),
            Node::Emphasis(e) => self.wrapped("*", &e.children, out),
            Node::Strong(s) => self.wrapped("**", &s.children, out),
            Node::Delete(d) => self.wrapped("~~", &d.children, out),
            Node::InlineCode(code) => code_span(&code.value, out),
            Node::Break(_) => out.push_str("\\\n"),
            Node::Link(link) => link_md(&plain_text(&link.children), &link.url, out),
            Node::LinkReference(link) => {
                match self.definitions.get(&link.identifier.to_lowercase()) {
                    Some(url) => link_md(&plain_text(&link.children), &url.clone(), out),
                    None => escape(&plain_text(&link.children), out),
                }
            }
            Node::Image(image) => image_md(&image.alt, &image.url, out),
            Node::ImageReference(image) => {
                let url = self
                    .definitions
                    .get(&image.identifier.to_lowercase())
                    .cloned()
                    .unwrap_or_default();
                image_md(&image.alt, &url, out);
            }
            Node::FootnoteReference(note) => escape(&format!("[^{}]", note.identifier), out),
            Node::Html(_) => {}
            other => escape(&other.to_string(), out),
        }
    }

    fn wrapped(&mut self, marker: &str, children: &[Node], out: &mut String) {
        let mut inner = String::new();
        for child in children {
            self.inline_node(child, &mut inner);
        }
        if inner.is_empty() {
            return;
        }
        out.push_str(marker);
        out.push_str(&inner);
        out.push_str(marker);
    }

    /// Prose: bound mentions, custom emoji, spoilers and hard breaks.
    fn text(&mut self, value: &str, node: &Node, out: &mut String) {
        // Seams index the body; map them when the node's source is its value.
        let start = node.position().and_then(|position| {
            let source = self
                .content
                .get(position.start.offset..position.end.offset)?;
            (source == value).then_some(position.start.offset)
        });
        let spans_seam = |from: usize, to: usize| match start {
            Some(base) => self
                .seams
                .iter()
                .any(|seam| base + from < *seam && *seam < base + to),
            None => !self.seams.is_empty(),
        };
        let mut i = 0;
        while i < value.len() {
            let rest = &value[i..];
            let previous = value[..i].chars().next_back();
            if rest.starts_with('@')
                && !previous
                    .is_some_and(|c| c.is_alphanumeric() || matches!(c, '_' | '@' | '/' | '\\'))
                && let Some((name, keys)) = self.names.iter().find(|(name, _)| {
                    rest[1..].starts_with(name.as_str())
                        && !rest[1 + name.len()..]
                            .chars()
                            .next()
                            .is_some_and(|c| c.is_alphanumeric() || c == '_')
                        && !spans_seam(i, i + 1 + name.len())
                })
            {
                let end = i + 1 + name.len();
                // A legacy namesake qualifier names someone else.
                let qualified = value[end..].strip_prefix(" (").is_some_and(|q| {
                    q.len() >= 65
                        && q.as_bytes()[..64].iter().all(u8::is_ascii_hexdigit)
                        && q.as_bytes()[64] == b')'
                });
                if keys.len() == 1 && !qualified {
                    link_md(
                        &format!("@{name}"),
                        &format!("buzz-mention:{}", keys[0]),
                        out,
                    );
                } else {
                    escape(&value[i..end], out);
                }
                i = end;
                continue;
            }
            if rest.starts_with(':')
                && let Some((index, length)) = self.custom_emoji(rest)
            {
                image_md(&rest[..length], &format!("buzz-emoji:{index}"), out);
                i += length;
                continue;
            }
            if rest.starts_with("||")
                && let Some(close) = rest[2..].find("||")
                && !rest[2..2 + close].trim().is_empty()
            {
                let inner = &rest[2..2 + close];
                link_md(inner, &format!("buzz-spoiler:{}", self.spoilers), out);
                self.spoilers += 1;
                i += close + 4;
                continue;
            }
            let c = rest.chars().next().expect("not at the end");
            match c {
                '\r' if rest.starts_with("\r\n") => {
                    out.push_str("\\\n");
                    i += 2;
                    continue;
                }
                '\n' | '\r' => out.push_str("\\\n"),
                _ => escape_char(c, out),
            }
            i += c.len_utf8();
        }
    }

    /// `:shortcode:` naming one of the row's own emoji: its index and the
    /// match length.
    fn custom_emoji(&self, rest: &str) -> Option<(usize, usize)> {
        let body = &rest[1..];
        let end = body.find(':')?;
        let code = &body[..end];
        let valid = (1..=64).contains(&code.len())
            && code
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'));
        if !valid {
            return None;
        }
        let code = code.to_ascii_lowercase();
        let index = self.emoji.iter().position(|e| e.shortcode == code)?;
        Some((index, end + 2))
    }

    fn spans(&self, nodes: &[Node], style: &Span, out: &mut Vec<Span>) {
        for node in nodes {
            let mut style = style.clone();
            let text = match node {
                Node::Text(text) => text.value.clone(),
                Node::InlineCode(code) => {
                    style.code = true;
                    code.value.clone()
                }
                Node::Break(_) => " ".into(),
                Node::Image(image) => image.alt.clone(),
                Node::Html(_) => continue,
                _ => {
                    match node {
                        Node::Strong(_) => style.strong = true,
                        Node::Emphasis(_) => style.emphasis = true,
                        Node::Delete(_) => style.delete = true,
                        Node::Link(_) | Node::LinkReference(_) => style.link = true,
                        _ => {}
                    }
                    self.spans(node.children().map_or(&[][..], Vec::as_slice), &style, out);
                    continue;
                }
            };
            if text.is_empty() {
                continue;
            }
            style.text = text;
            out.push(style);
        }
    }
}

/// The visible text of inline nodes, as a link label.
fn plain_text(nodes: &[Node]) -> String {
    let mut out = String::new();
    for node in nodes {
        match node {
            Node::Text(text) => out.push_str(&text.value),
            Node::InlineCode(code) => out.push_str(&code.value),
            Node::Image(image) => out.push_str(&image.alt),
            Node::Break(_) => out.push(' '),
            Node::Html(_) => {}
            other => out.push_str(&plain_text(other.children().map_or(&[][..], Vec::as_slice))),
        }
    }
    out
}

fn escape_char(c: char, out: &mut String) {
    if c.is_ascii_punctuation() {
        out.push('\\');
    }
    out.push(c);
}

fn escape(text: &str, out: &mut String) {
    for c in text.chars() {
        match c {
            '\n' | '\r' => out.push(' '),
            c => escape_char(c, out),
        }
    }
}

/// `<…>` destinations allow any character but these.
fn destination(url: &str) -> String {
    url.replace('<', "%3C")
        .replace('>', "%3E")
        .replace('\n', "%0A")
        .replace('\r', "%0D")
}

fn link_md(label: &str, url: &str, out: &mut String) {
    out.push('[');
    escape(label, out);
    out.push_str("](<");
    out.push_str(&destination(url));
    out.push_str(">)");
}

fn image_md(alt: &str, url: &str, out: &mut String) {
    out.push_str("![");
    escape(alt, out);
    out.push_str("](<");
    out.push_str(&destination(url));
    out.push_str(">)");
}

fn code_span(value: &str, out: &mut String) {
    let mut longest = 0;
    let mut run = 0;
    for c in value.chars() {
        run = if c == '`' { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    let fence = "`".repeat(longest + 1);
    let pad = value.starts_with('`') || value.ends_with('`') || value.starts_with(' ');
    out.push_str(&fence);
    if pad {
        out.push(' ');
    }
    out.push_str(value);
    if pad {
        out.push(' ');
    }
    out.push_str(&fence);
}

// ---------------------------------------------------------------------------
// Emoji-only bodies (`emoji-size.ts`)

/// Unicode pictographs, flags and keycaps, plus the row's own
/// `:shortcodes:`, with whitespace between them and nothing else.
fn is_emoji_only(content: &str, emoji: &[CustomEmoji]) -> bool {
    let parts = emoji_parts(content.trim(), emoji);
    if parts.is_empty() {
        return false;
    }
    let mut any = false;
    for part in &parts {
        match part {
            EmojiPart::Custom(_) => any = true,
            EmojiPart::Text(text) => {
                let chars: Vec<char> = text.chars().collect();
                for (index, &c) in chars.iter().enumerate() {
                    if c.is_whitespace() || is_emoji_modifier(c) {
                        continue;
                    }
                    if is_pictographic(c) {
                        any = true;
                        continue;
                    }
                    // Keycaps: [0-9#*] FE0F? 20E3.
                    let keycap = (c.is_ascii_digit() || matches!(c, '#' | '*'))
                        && match chars.get(index + 1) {
                            Some('\u{20E3}') => true,
                            Some('\u{FE0F}') => chars.get(index + 2) == Some(&'\u{20E3}'),
                            _ => false,
                        };
                    if keycap {
                        any = true;
                        continue;
                    }
                    return false;
                }
            }
        }
    }
    any
}

/// Characters that only modify an emoji sequence.
fn is_emoji_modifier(c: char) -> bool {
    matches!(c as u32,
        0x200D | 0xFE0E | 0xFE0F | 0x20E3 | 0x1F3FB..=0x1F3FF | 0xE0020..=0xE007F)
}

/// `\p{Extended_Pictographic}` and regional indicators, by the ranges that
/// hold them.
fn is_pictographic(c: char) -> bool {
    matches!(c as u32,
        0x00A9 | 0x00AE | 0x203C | 0x2049 | 0x2122 | 0x2139 | 0x2194..=0x2199 | 0x21A9..=0x21AA
        | 0x231A..=0x231B | 0x2328 | 0x2388 | 0x23CF | 0x23E9..=0x23F3 | 0x23F8..=0x23FA
        | 0x24C2 | 0x25AA..=0x25AB | 0x25B6 | 0x25C0 | 0x25FB..=0x25FE | 0x2600..=0x27BF
        | 0x2934..=0x2935 | 0x2B05..=0x2B07 | 0x2B1B..=0x2B1C | 0x2B50 | 0x2B55 | 0x3030
        | 0x303D | 0x3297 | 0x3299 | 0x1F000..=0x1FAFF | 0x1FC00..=0x1FFFD)
}

fn emoji_parts(content: &str, emoji: &[CustomEmoji]) -> Vec<EmojiPart> {
    let prep = Prep {
        content,
        emoji,
        names: Vec::new(),
        seams: &[],
        definitions: HashMap::new(),
        spoilers: 0,
    };
    let mut parts = Vec::new();
    let mut text = String::new();
    let mut i = 0;
    while i < content.len() {
        let rest = &content[i..];
        if rest.starts_with(':')
            && let Some((index, length)) = prep.custom_emoji(rest)
        {
            if !text.is_empty() {
                parts.push(EmojiPart::Text(std::mem::take(&mut text)));
            }
            parts.push(EmojiPart::Custom(emoji[index].clone()));
            i += length;
            continue;
        }
        let c = rest.chars().next().expect("not at the end");
        text.push(c);
        i += c.len_utf8();
    }
    if !text.is_empty() {
        parts.push(EmojiPart::Text(text));
    }
    parts
}

fn jumbo(parts: &[EmojiPart], media: &Media) -> AnyElement {
    div()
        .flex()
        .flex_wrap()
        .items_center()
        .mt(px(4.))
        .min_h(px(48.))
        .text_size(px(42.))
        .line_height(px(42.))
        .children(parts.iter().map(|part| match part {
            EmojiPart::Text(text) => div().child(text.clone()).into_any_element(),
            EmojiPart::Custom(entry) => custom_emoji(entry, 42., media).into_any_element(),
        }))
        .into_any_element()
}

fn custom_emoji(entry: &CustomEmoji, size: f32, media: &Media) -> impl IntoElement {
    let fallback = format!(":{}:", entry.shortcode);
    img(media::image(&entry.url, media))
        .size(px(size))
        .flex_none()
        .object_fit(ObjectFit::Contain)
        .with_fallback(move || {
            div()
                .text_size(px(12.))
                .line_height(px(16.))
                .child(fallback.clone())
                .into_any_element()
        })
}

// ---------------------------------------------------------------------------
// Layout

struct Out<'a> {
    objects: Objects,
    cx: &'a App,
    /// Inside a quote, text is subtle.
    quote: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum Container {
    /// The body: first and last margins drop (`.text > :first-child`).
    Root,
    /// Quotes keep their children's margins inside their padding.
    Quote,
    /// List items space their blocks 4px apart.
    Item,
}

/// CSS block margins (top, bottom), which collapse between siblings.
fn margins(block: &Block) -> (f32, f32) {
    match block {
        Block::Heading(..) => (16., 6.),
        _ => (12., 12.),
    }
}

fn column(blocks: &[Block], container: Container, path: &str, out: &mut Out) -> Div {
    let mut column = div().flex().flex_col().w_full().min_w_0();
    let mut previous: Option<f32> = None;
    for (index, block) in blocks.iter().enumerate() {
        let (top, bottom) = match container {
            Container::Item => (4., 4.),
            _ => margins(block),
        };
        let gap = match previous {
            Some(previous) => previous.max(top),
            None if container == Container::Quote => top,
            None => 0.,
        };
        let element = render_block(block, &format!("{path}-{index}"), out);
        column = column.child(div().pt(px(gap)).child(element));
        previous = Some(bottom);
    }
    if container == Container::Quote
        && let Some(bottom) = previous
    {
        column = column.pb(px(bottom));
    }
    column
}

fn render_block(block: &Block, path: &str, out: &mut Out) -> AnyElement {
    let c = *out.cx.colors();
    let text = if out.quote {
        c.text_subtle
    } else {
        c.text_standard
    };
    match block {
        Block::Paragraph(source) => inline(source, path, text, out),
        Block::Heading(depth, source) => {
            let (size, line) = match depth {
                1 => (24., 26.4),
                2 => (20., 28.),
                3 => (16., 24.),
                _ => (14., 20.),
            };
            div()
                .text_size(px(size))
                .line_height(px(line))
                .font_weight(FontWeight::MEDIUM)
                .child(inline(source, path, text, out))
                .into_any_element()
        }
        Block::Quote(blocks) => {
            let quoted = std::mem::replace(&mut out.quote, true);
            let inner = column(blocks, Container::Quote, path, out);
            out.quote = quoted;
            div()
                .py(px(2.))
                .pl(px(12.))
                .border_l_1()
                .border_color(c.border_prominent)
                .text_color(c.text_subtle)
                .child(inner)
                .into_any_element()
        }
        Block::List {
            ordered,
            start,
            items,
        } => {
            let mut list = div().flex().flex_col().w_full();
            for (index, item) in items.iter().enumerate() {
                let marker = match (item.checked, ordered) {
                    (Some(checked), _) => checkbox(checked, &c).into_any_element(),
                    (None, true) => div()
                        .pr(px(4.))
                        .child(format!("{}.", *start as usize + index))
                        .into_any_element(),
                    (None, false) => div().pr(px(10.)).child("•").into_any_element(),
                };
                let content = column(
                    &item.blocks,
                    Container::Item,
                    &format!("{path}-{index}"),
                    out,
                );
                list = list.child(
                    div()
                        .flex()
                        .when(index > 0, |d| d.mt(px(4.)))
                        .child(
                            div()
                                .flex_none()
                                .w(px(24.))
                                .flex()
                                .justify_end()
                                .text_color(text)
                                .child(marker),
                        )
                        .child(div().flex_1().min_w_0().child(content)),
                );
            }
            list.into_any_element()
        }
        Block::Code(code) => div()
            .id(SharedString::from(format!("code{path}")))
            .w_full()
            .p(px(12.))
            .border_1()
            .border_color(c.border_standard)
            .rounded(px(8.))
            .bg(c.surface_panel)
            .overflow_x_scroll()
            .font_family(font::MONO)
            .text_size(px(12.))
            .line_height(px(16.))
            .text_color(text)
            .child(
                div()
                    .whitespace_nowrap()
                    .child(code.trim_end_matches('\n').to_string()),
            )
            .into_any_element(),
        Block::Table { align, rows } => table(rows, align, path, text, &c),
        Block::Rule => div()
            .h(px(1.))
            .w_full()
            .bg(c.border_standard)
            .into_any_element(),
    }
}

fn checkbox(checked: bool, c: &buzz_ui::Colors) -> impl IntoElement {
    div()
        .mr(px(6.))
        .mt(px(3.))
        .size(px(14.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(3.))
        .border_1()
        .border_color(if checked {
            c.affordance_prominent
        } else {
            c.border_prominent
        })
        .when(checked, |d| {
            d.bg(c.affordance_prominent)
                .child(Icon::new(IconName::Check).size(10.).color(c.text_inverse))
        })
}

/// Cells are a column per table column, so columns share a width without
/// a grid; cells stay on one line (`nowrap`), so rows line up.
fn table(
    rows: &[Vec<Vec<Span>>],
    align: &[AlignKind],
    path: &str,
    text: Hsla,
    c: &buzz_ui::Colors,
) -> AnyElement {
    let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
    let mut frame = div()
        .flex()
        .border_1()
        .border_color(c.border_standard)
        .rounded(px(12.))
        .overflow_hidden()
        .text_color(text);
    for column in 0..columns {
        let mut cells = div().flex().flex_col().flex_none();
        for (index, row) in rows.iter().enumerate() {
            let spans = row.get(column).map_or(&[][..], Vec::as_slice);
            let head = index == 0;
            let cell = div()
                .h(px(28.))
                .px(px(8.))
                .py(px(4.))
                .whitespace_nowrap()
                .flex()
                .when(column + 1 < columns, |d| {
                    d.border_r_1().border_color(c.border_standard)
                })
                .when(index + 1 < rows.len(), |d| {
                    d.border_b_1().border_color(c.border_standard)
                })
                .when(head, |d| {
                    d.bg(c.affordance_subtle).font_weight(FontWeight::MEDIUM)
                })
                .map(|d| match align.get(column) {
                    Some(AlignKind::Center) => d.justify_center(),
                    Some(AlignKind::Right) => d.justify_end(),
                    _ => d,
                })
                .child(styled_spans(spans, c));
            cells = cells.child(cell);
        }
        frame = frame.child(cells);
    }
    div()
        .id(SharedString::from(format!("table{path}")))
        .max_w_full()
        .overflow_x_scroll()
        .child(div().flex().child(frame))
        .into_any_element()
}

fn styled_spans(spans: &[Span], c: &buzz_ui::Colors) -> StyledText {
    let mut text = String::new();
    let mut highlights = Vec::new();
    for span in spans {
        let start = text.len();
        text.push_str(&span.text);
        let style = HighlightStyle {
            font_weight: span.strong.then_some(FontWeight::MEDIUM),
            font_style: span.emphasis.then_some(FontStyle::Italic),
            strikethrough: span.delete.then_some(StrikethroughStyle {
                thickness: px(1.),
                color: None,
            }),
            background_color: span.code.then_some(c.affordance_subtle),
            color: span.link.then_some(c.text_link),
            ..Default::default()
        };
        if style != HighlightStyle::default() {
            highlights.push((start..text.len(), style));
        }
    }
    StyledText::new(text).with_highlights(highlights)
}

/// A paragraph's inline content in a selectable `TextView`.
fn inline(source: &SharedString, path: &str, color: Hsla, out: &Out) -> AnyElement {
    let c = out.cx.colors();
    let style = TextViewStyle::default()
        .with_foreground(color)
        .with_muted_foreground(c.text_subtle)
        .with_link(c.text_link)
        .with_border(c.border_standard)
        .with_code_background(c.affordance_subtle)
        .with_selection(c.text_link.opacity(0.25))
        .with_paragraph_gap(rems(0.));
    let extensions = MarkdownExtensions::default()
        .parser_revision(PARSER_REVISION)
        .plugin(out.objects.clone());
    TextView::markdown(SharedString::from(format!("t{path}")), source.clone())
        .style(style)
        .markdown_extensions(extensions)
        .w_full()
        .into_any_element()
}

// ---------------------------------------------------------------------------
// Inline objects

/// Bumped when [`Objects::parse`] changes.
const PARSER_REVISION: u64 = 0xb022_0001;
/// Longer code spans stay in the text flow so they can wrap; shorter ones
/// get the padded chip.
const CODE_CHIP_MAX: usize = 40;
/// Raw URLs show at most this many characters (`InlineLink.tsx`).
const RAW_URL_MAX: usize = 44;

#[derive(Clone)]
struct Objects {
    directory: Directory,
    emoji: Arc<[CustomEmoji]>,
    row: SharedString,
}

enum Object {
    Link { url: String, label: String },
    Image { url: String, alt: String },
    Code(String),
}

impl MarkdownPlugin for Objects {
    fn name(&self) -> &str {
        "buzz"
    }

    fn parse(&self, node: &mdast::Node, _: &MarkdownParseContext<'_>) -> Option<MarkdownNode> {
        let (object, text) = match node {
            Node::Link(link) => {
                let label = plain_text(&link.children);
                (
                    Object::Link {
                        url: link.url.clone(),
                        label: label.clone(),
                    },
                    label,
                )
            }
            Node::Image(image) => (
                Object::Image {
                    url: image.url.clone(),
                    alt: image.alt.clone(),
                },
                image.alt.clone(),
            ),
            Node::InlineCode(code) if code.value.chars().count() <= CODE_CHIP_MAX => {
                (Object::Code(code.value.clone()), code.value.clone())
            }
            _ => return None,
        };
        Some(MarkdownNode::new("buzz", object).text(text))
    }

    fn render_inline(
        &self,
        node: &MarkdownNode,
        context: &InlineRenderContext,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<InlineElement> {
        let c = *cx.colors();
        let size = context.font_size();
        let line = context.line_height();
        // Inter's ascent and descent are 0.969 and 0.241 em.
        let baseline = (line - size * 1.21) / 2. + size * 0.969;
        match node.data::<Object>()? {
            Object::Code(code) => {
                // JetBrains Mono 12/16 with 2px padding; its ascent is 1.02 em.
                let chip_baseline = px(2. + (16. - 15.84) / 2. + 12.24);
                Some(
                    InlineElement::new(
                        div()
                            .px(px(6.))
                            .py(px(2.))
                            .rounded(px(4.))
                            .bg(c.affordance_subtle)
                            .font_family(font::MONO)
                            .text_size(px(12.))
                            .line_height(px(16.))
                            .whitespace_nowrap()
                            .child(code.clone()),
                    )
                    .with_baseline(chip_baseline),
                )
            }
            Object::Image { url, alt } => {
                if let Some(index) = url.strip_prefix("buzz-emoji:") {
                    let entry = self.emoji.get(index.parse::<usize>().ok()?)?;
                    // 22px, `vertical-align: middle`: centred on half the
                    // x-height (0.546 em) above the baseline.
                    return Some(
                        InlineElement::new(custom_emoji(entry, 22., &self.directory.media))
                            .with_baseline(px(11.) + size * 0.273),
                    );
                }
                // Markdown images are never loaded: the alt text stands in.
                if alt.is_empty() {
                    return None;
                }
                Some(
                    InlineElement::new(
                        div()
                            .italic()
                            .text_color(c.text_subtle)
                            .line_height(line)
                            .child(alt.clone()),
                    )
                    .with_baseline(baseline),
                )
            }
            Object::Link { url, label } => {
                let element = self.link(url, label, size, line, &c, window, cx)?;
                Some(InlineElement::new(element).with_baseline(baseline))
            }
        }
    }
}

impl Objects {
    #[allow(clippy::too_many_arguments)]
    fn link(
        &self,
        url: &str,
        label: &str,
        size: Pixels,
        line: Pixels,
        c: &buzz_ui::Colors,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        let reference = |id: String, icon: Option<IconName>, gap: f32, text: String| {
            div()
                .id(SharedString::from(id))
                .flex()
                .items_center()
                .gap(px(gap))
                .px(px(2.))
                .mx(px(-2.))
                .rounded(px(4.))
                .line_height(line)
                .whitespace_nowrap()
                .text_color(c.text_link)
                .hover(|style| style.bg(c.affordance_link_hover))
                .when_some(icon, |d, icon| {
                    d.child(Icon::new(icon).size(f32::from(size)).color(c.text_link))
                })
                .child(text)
        };
        if let Some(index) = url.strip_prefix("buzz-spoiler:") {
            return Some(self.spoiler(index, label, line, c, window, cx));
        }
        let mention = url
            .strip_prefix("buzz-mention:")
            .map(str::to_string)
            .or_else(|| buzz_relay_client::profile_key(url).map(|key| key.to_hex()));
        if let Some(key) = mention {
            let profile = self.directory.profiles.get(&key);
            let agent = profile.is_some_and(|p| p.is_agent);
            let name = profile
                .map(|p| p.name.clone())
                .unwrap_or_else(|| label.strip_prefix('@').unwrap_or(label).to_string());
            let (icon, gap) = if agent {
                (IconName::Robot, 3.)
            } else {
                (IconName::At, 0.)
            };
            return Some(
                reference(format!("mention-{key}"), Some(icon), gap, name)
                    .cursor_pointer()
                    .on_click(move |event, window, cx| {
                        cx.stop_propagation();
                        navigation::show_profile(key.clone(), event.position(), window, cx)
                    })
                    .into_any_element(),
            );
        }
        if let Some(link) = buzz_relay_client::parse_buzz_link(url) {
            let channel = self
                .directory
                .channels
                .iter()
                .find(|channel| channel.id == link.channel);
            let icon = match (&link.message, &link.thread, channel) {
                (_, Some(_), _) => IconName::ChatsCircle,
                (Some(_), None, _) => IconName::ChatCircle,
                (None, None, Some(channel)) if channel.channel_type == Some(ChannelType::Dm) => {
                    IconName::ChatCircle
                }
                (None, None, Some(channel)) if channel.private => IconName::Lock,
                _ => IconName::Hash,
            };
            let text = match channel {
                Some(channel) => channel.name.clone(),
                None if !label.is_empty() && label != url => label.to_string(),
                None => "Private channel".to_string(),
            };
            let target = url.to_string();
            return Some(
                reference(format!("ref-{url}"), Some(icon), 3., text)
                    .cursor_pointer()
                    .on_click(move |_, window, cx| navigation::open_url(&target, window, cx))
                    .into_any_element(),
            );
        }
        if let Some(Link::Join { relay, .. }) = deep_links::parse(url) {
            let target = url.to_string();
            let host = relay.websocket().split("://").nth(1).unwrap_or_default();
            let text = if label.is_empty() || label == url {
                format!("Join {host}")
            } else {
                label.to_string()
            };
            return Some(
                reference(format!("join-{url}"), Some(IconName::Globe), 3., text)
                    .cursor_pointer()
                    .on_click(move |_, window, cx| navigation::open_url(&target, window, cx))
                    .into_any_element(),
            );
        }
        let safe = url::Url::parse(url).ok().filter(|parsed| {
            matches!(parsed.scheme(), "http" | "https")
                && parsed.username().is_empty()
                && parsed.password().is_none()
        })?;
        let text = if label.is_empty() || label == url {
            let mut shown: String = url.chars().take(RAW_URL_MAX).collect();
            if url.chars().count() > RAW_URL_MAX {
                shown.push('…');
            }
            shown
        } else {
            label.to_string()
        };
        let target = safe.to_string();
        Some(
            reference(format!("link-{url}"), Some(link_icon(&safe)), 3., text)
                .cursor_pointer()
                .on_click(move |_, _, cx| cx.open_url(&target))
                .into_any_element(),
        )
    }

    /// `||text||`: masked until clicked; a click toggles it.
    fn spoiler(
        &self,
        index: &str,
        label: &str,
        line: Pixels,
        c: &buzz_ui::Colors,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let id = SharedString::from(format!("spoiler-{}-{index}", self.row));
        let revealed = window.use_keyed_state(id.clone(), cx, |_, _| false);
        let shown = *revealed.read(cx);
        div()
            .id(id)
            .px(px(2.))
            .rounded(px(4.))
            .line_height(line)
            .whitespace_nowrap()
            .cursor_pointer()
            .map(|d| {
                if shown {
                    d.bg(c.affordance_subtle)
                } else {
                    // The reference draws twinkling dots; a flat mask keeps
                    // the text hidden the same way.
                    d.bg(c.text_subtle.opacity(0.35))
                        .text_color(transparent_black())
                        .hover(|style| style.bg(c.text_subtle.opacity(0.5)))
                }
            })
            .on_click(move |_, _, cx| {
                revealed.update(cx, |shown, cx| {
                    *shown = !*shown;
                    cx.notify();
                })
            })
            .child(label.to_string())
            .into_any_element()
    }
}

/// The icon in front of a web link, by host (`InlineLink.tsx`).
fn link_icon(url: &url::Url) -> IconName {
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    let on = |domain: &str| host == domain || host.ends_with(&format!(".{domain}"));
    let path = url.path();
    if on("github.com") {
        IconName::GithubLogo
    } else if host == "docs.google.com" && path.starts_with("/spreadsheets") {
        IconName::Table
    } else if host == "docs.google.com" && path.starts_with("/presentation") {
        IconName::Presentation
    } else if host == "docs.google.com" {
        IconName::FileText
    } else if on("drive.google.com") {
        IconName::GoogleDriveLogo
    } else if on("figma.com") {
        IconName::FigmaLogo
    } else if on("notion.so") || on("notion.site") {
        IconName::NotionLogo
    } else if on("slack.com") {
        IconName::SlackLogo
    } else if on("dropbox.com") {
        IconName::DropboxLogo
    } else if on("gitlab.com") {
        IconName::GitlabLogo
    } else if on("youtube.com") || on("youtu.be") {
        IconName::YoutubeLogo
    } else if on("loom.com") {
        IconName::VideoCamera
    } else if on("zoom.us") {
        IconName::VideoConference
    } else if on("teams.microsoft.com") || on("teams.live.com") {
        IconName::MicrosoftTeamsLogo
    } else {
        IconName::Globe
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    // gpui's `test` macro comes in with the glob above.
    use core::prelude::v1::test;

    fn blocks(content: &str) -> Vec<Block> {
        let row = row(content);
        match prepare(&row, &Profiles::default(), &[]) {
            Document::Blocks(blocks) => blocks,
            other => panic!("not blocks: {other:?}"),
        }
    }

    pub(in crate::ui) fn row(content: &str) -> MessageRow {
        MessageRow {
            id: nostr::prelude::EventId::from_byte_array([1; 32]),
            channel: Arc::from("c"),
            kind: 9,
            author: buzz_identity::Identity::ephemeral().public_key(),
            signer: None,
            workflow_owner: None,
            created_at: 0,
            created_at_ms: 0,
            content: content.into(),
            source_content: None,
            edited: false,
            attachment_seams: Vec::new(),
            attachment_source_id: None,
            attachments: Vec::new(),
            emoji: Vec::new(),
            mentions: Vec::new(),
            mention_references: Vec::new(),
            reactions: Vec::new(),
            reply_count: 0,
            participants: Vec::new(),
            thread_root_id: None,
            reply_parent_id: None,
            sent_from_thread: None,
            diff: None,
            membership: None,
            delivery: None,
        }
    }

    fn paragraph(content: &str) -> String {
        match blocks(content).as_slice() {
            [Block::Paragraph(text)] => text.to_string(),
            other => panic!("not one paragraph: {other:?}"),
        }
    }

    #[test]
    fn binds_signed_recipients_by_exact_name() {
        let sam = buzz_identity::Identity::ephemeral().public_key();
        let profile = buzz_relay_client::Profile {
            name: "Sam Rivera".into(),
            ..buzz_relay_client::Profile::fallback(&sam.to_hex())
        };
        let profiles: Profiles = Arc::new(HashMap::from([(sam.to_hex(), Arc::new(profile))]));
        let mut row = row("@Sam Rivera could you `@Sam Rivera` look, @Sam Riveras?");
        row.mentions = vec![sam];
        let Document::Blocks(blocks) = prepare(&row, &profiles, &identities(&row)) else {
            panic!("not blocks");
        };
        let target = format!("[\\@Sam Rivera](<buzz-mention:{}>)", sam.to_hex());
        assert_eq!(
            blocks,
            vec![Block::Paragraph(
                format!("{target} could you `@Sam Rivera` look\\, \\@Sam Riveras\\?").into()
            )]
        );
        // Edited bodies stay unbound.
        row.edited = true;
        let Document::Blocks(blocks) = prepare(&row, &profiles, &identities(&row)) else {
            panic!("not blocks");
        };
        assert!(!format!("{blocks:?}").contains("buzz-mention"));
    }

    #[test]
    fn finds_emoji_and_spoilers_in_prose_only() {
        let mut row = row("A :Party: ||secret|| `:party:` [:party:](https://x.test)");
        row.emoji = vec![CustomEmoji {
            shortcode: "party".into(),
            url: "https://x.test/p.png".into(),
        }];
        let Document::Blocks(blocks) = prepare(&row, &Profiles::default(), &[]) else {
            panic!("not blocks");
        };
        assert_eq!(
            blocks,
            vec![Block::Paragraph(
                "A ![\\:Party\\:](<buzz-emoji:0>) [secret](<buzz-spoiler:0>) `:party:` \
                 [\\:party\\:](<https://x.test>)"
                    .into()
            )]
        );
    }

    #[test]
    fn drops_raw_html_and_keeps_structure() {
        assert_eq!(paragraph("a <b>b</b> c"), "a b c");
        assert_eq!(
            blocks("# T\n\n> q\n\n- [x] done\n\n```\nfn main() {}\n```\n\n---"),
            vec![
                Block::Heading(1, "T".into()),
                Block::Quote(vec![Block::Paragraph("q".into())]),
                Block::List {
                    ordered: false,
                    start: 1,
                    items: vec![Item {
                        checked: Some(true),
                        blocks: vec![Block::Paragraph("done".into())],
                    }],
                },
                Block::Code("fn main() {}".into()),
                Block::Rule,
            ]
        );
    }

    #[test]
    fn detects_emoji_only_bodies() {
        let party = CustomEmoji {
            shortcode: "party".into(),
            url: "https://x.test/p.png".into(),
        };
        assert!(is_emoji_only("🎉 🚀", &[]));
        assert!(is_emoji_only("👍🏽 ❤️ 1️⃣ 🇺🇸 👨‍👩‍👧", &[]));
        assert!(is_emoji_only(" :party: 🎉", std::slice::from_ref(&party)));
        assert!(!is_emoji_only(":party:", &[]));
        assert!(!is_emoji_only("🎉 yay", &[]));
        assert!(!is_emoji_only("1", &[]));
        assert!(!is_emoji_only("", &[]));
    }

    #[test]
    fn falls_back_to_plain_text_when_too_deep() {
        let body = "> ".repeat(200) + "deep";
        assert_eq!(
            prepare(&row(&body), &Profiles::default(), &[]),
            Document::Plain(body)
        );
    }

    #[test]
    fn paragraphs_keep_breaks_and_escape_prose() {
        assert_eq!(
            blocks("a *b* 1.\nc"),
            vec![Block::Paragraph("a *b* 1\\.\\\nc".into())]
        );
    }
}
