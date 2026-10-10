//! Renderer registries (`conversation/contracts.ts`, `MessageBody.tsx`,
//! `MessageLink.tsx`, `InlineText.tsx`, `AttachmentView.tsx`). buzz-app's
//! plugins contribute message, link, inline and attachment renderers; this
//! port compiles its bundled features in, so the registries are internal
//! extension points they register through.
//!
//! Among the renderers that match, the lowest order wins, earliest
//! registration first on ties. A renderer that declines (`None`) or
//! panics leaves the host rendering in place (`ContributionBoundary`).

use super::markdown::{self, Directory};
use super::media;
use buzz_relay_client::Media;
use buzz_relay_client::fold::{Attachment, CustomEmoji, MessageRow};
use gpui_kit::*;
use std::cell::RefCell;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;
use std::sync::Arc;

/// Replaces a message's body (diffs use one).
pub trait MessageRenderer {
    fn key(&self) -> &str;
    fn order(&self, _: &MessageRow) -> i32 {
        0
    }
    fn matches(&self, row: &MessageRow) -> bool;
    fn render(&self, row: &Arc<MessageRow>, directory: &Directory, cx: &App) -> Option<AnyElement>;
}

/// Draws a link chip's contents for the URLs it matches.
pub trait LinkRenderer {
    fn key(&self) -> &str;
    fn order(&self, url: &str) -> i32;
    fn matches(&self, url: &str) -> bool;
    fn render(&self, url: &str, label: Option<&str>, cx: &App) -> Option<AnyElement>;
}

/// Draws a piece of inline text, such as a custom emoji's `:shortcode:`.
pub trait InlineRenderer {
    fn key(&self) -> &str;
    fn render(
        &self,
        text: &str,
        emoji: &[CustomEmoji],
        media: &Media,
        cx: &App,
    ) -> Option<AnyElement>;
}

/// A message action (`MessageActionContribution`): a more-menu item, and
/// optionally a marker after the byline, such as a reminder's.
pub trait MessageAction {
    fn key(&self) -> &str;
    fn title(&self) -> &str;
    fn icon(&self) -> Option<buzz_ui::IconName> {
        None
    }
    fn matches(&self, row: &MessageRow) -> bool;
    fn run(&self, row: &Arc<MessageRow>, window: &mut Window, cx: &mut App);
    fn marker(&self, _: &MessageRow, _: &App) -> Option<AnyElement> {
        None
    }
}

/// Replaces an attachment's player (voice notes use one).
pub trait AttachmentRenderer {
    fn key(&self) -> &str;
    fn matches(&self, attachment: &Attachment) -> bool;
    fn render(
        &self,
        attachment: &Attachment,
        id: SharedString,
        media: &Media,
        cx: &App,
    ) -> Option<AnyElement>;
}

#[derive(Default)]
struct Registry {
    messages: Vec<Rc<dyn MessageRenderer>>,
    links: Vec<Rc<dyn LinkRenderer>>,
    inline: Vec<Rc<dyn InlineRenderer>>,
    attachments: Vec<Rc<dyn AttachmentRenderer>>,
    actions: Vec<Rc<dyn MessageAction>>,
}

thread_local! {
    static REGISTRY: RefCell<Registry> = RefCell::new(bundled());
}

/// The bundled features: diffs, voice notes and custom emoji.
fn bundled() -> Registry {
    Registry {
        messages: vec![Rc::new(Diffs)],
        links: Vec::new(),
        inline: vec![Rc::new(Emoji)],
        attachments: vec![Rc::new(VoiceNotes)],
        actions: Vec::new(),
    }
}

// Extension points: the bundled features register above; nothing else
// registers yet.
#[allow(dead_code)]
pub fn register_message(renderer: Rc<dyn MessageRenderer>) {
    REGISTRY.with_borrow_mut(|r| r.messages.push(renderer));
}

#[allow(dead_code)]
pub fn register_link(renderer: Rc<dyn LinkRenderer>) {
    REGISTRY.with_borrow_mut(|r| r.links.push(renderer));
}

#[allow(dead_code)]
pub fn register_inline(renderer: Rc<dyn InlineRenderer>) {
    REGISTRY.with_borrow_mut(|r| r.inline.push(renderer));
}

#[allow(dead_code)]
pub fn register_attachment(renderer: Rc<dyn AttachmentRenderer>) {
    REGISTRY.with_borrow_mut(|r| r.attachments.push(renderer));
}

#[allow(dead_code)]
pub fn register_action(action: Rc<dyn MessageAction>) {
    REGISTRY.with_borrow_mut(|r| r.actions.push(action));
}

/// The actions that apply to `row`; a matcher that fails leaves the menu
/// usable.
pub fn actions(row: &MessageRow) -> Vec<Rc<dyn MessageAction>> {
    let all = REGISTRY.with_borrow(|r| r.actions.clone());
    all.into_iter()
        .filter(|a| catch_unwind(AssertUnwindSafe(|| a.matches(row))).unwrap_or(false))
        .collect()
}

/// Runs the action `key` on `row`.
pub fn run_action(key: &str, row: &Arc<MessageRow>, window: &mut Window, cx: &mut App) {
    if let Some(action) = actions(row).into_iter().find(|a| a.key() == key) {
        let _ = guarded(action.key(), || {
            action.run(row, window, cx);
            Some(())
        });
    }
}

/// The byline markers actions draw for `row`.
pub fn markers(row: &MessageRow, cx: &App) -> Vec<AnyElement> {
    actions(row)
        .into_iter()
        .filter_map(|a| guarded(a.key(), || a.marker(row, cx)))
        .collect()
}

/// Runs a contribution; a panic counts as declining.
fn guarded<T>(key: &str, run: impl FnOnce() -> Option<T>) -> Option<T> {
    match catch_unwind(AssertUnwindSafe(run)) {
        Ok(result) => result,
        Err(_) => {
            tracing::warn!(renderer = key, "a renderer failed; showing the default");
            None
        }
    }
}

/// `resolveMatch`: the lowest order among matching entries.
fn resolve<T: ?Sized>(
    entries: &[Rc<T>],
    matches: impl Fn(&T) -> bool,
    order: impl Fn(&T) -> i32,
) -> Vec<Rc<T>> {
    let mut found: Vec<(i32, usize, Rc<T>)> = entries
        .iter()
        .enumerate()
        .filter(|(_, e)| catch_unwind(AssertUnwindSafe(|| matches(e))).unwrap_or(false))
        .map(|(i, e)| (order(e), i, e.clone()))
        .collect();
    found.sort_by_key(|(order, index, _)| (*order, *index));
    found.into_iter().map(|(_, _, e)| e).collect()
}

/// A row's body: the winning message renderer, else Markdown.
pub fn message_body(row: &Arc<MessageRow>, directory: &Directory, cx: &App) -> AnyElement {
    let candidates =
        REGISTRY.with_borrow(|r| resolve(&r.messages, |m| m.matches(row), |m| m.order(row)));
    for renderer in candidates {
        if let Some(element) = guarded(renderer.key(), || renderer.render(row, directory, cx)) {
            return element;
        }
    }
    markdown::body(row, directory, cx)
}

/// A contributed link chip, when one outranks the bundled links.
pub fn link(url: &str, label: Option<&str>, cx: &App) -> Option<AnyElement> {
    let candidates =
        REGISTRY.with_borrow(|r| resolve(&r.links, |l| l.matches(url), |l| l.order(url)));
    // The bundled links renderer sits at 0 for known kinds, 100 for the web.
    let bundled = match super::links::link_kind(url) {
        Some(super::links::LinkKind::Web) => 100,
        Some(_) => 0,
        None => i32::MAX,
    };
    candidates
        .into_iter()
        .filter(|renderer| renderer.order(url) < bundled)
        .find_map(|renderer| guarded(renderer.key(), || renderer.render(url, label, cx)))
}

/// Inline text through the inline renderers, first that takes it.
pub fn inline(text: &str, emoji: &[CustomEmoji], media: &Media, cx: &App) -> Option<AnyElement> {
    let renderers = REGISTRY.with_borrow(|r| r.inline.clone());
    renderers
        .into_iter()
        .find_map(|renderer| guarded(renderer.key(), || renderer.render(text, emoji, media, cx)))
}

/// An attachment through the attachment renderers, if one takes it.
pub fn attachment(
    attachment: &Attachment,
    id: SharedString,
    media: &Media,
    cx: &App,
) -> Option<AnyElement> {
    let renderers = REGISTRY.with_borrow(|r| r.attachments.clone());
    renderers
        .into_iter()
        .filter(|renderer| renderer.matches(attachment))
        .find_map(|renderer| {
            guarded(renderer.key(), || {
                renderer.render(attachment, id.clone(), media, cx)
            })
        })
}

// ---------------------------------------------------------------------------
// Bundled renderers

struct Diffs;

impl MessageRenderer for Diffs {
    fn key(&self) -> &str {
        "diffs"
    }

    fn matches(&self, row: &MessageRow) -> bool {
        row.diff.is_some()
    }

    fn render(&self, row: &Arc<MessageRow>, _: &Directory, _: &App) -> Option<AnyElement> {
        super::diff::card(row)
    }
}

struct VoiceNotes;

impl AttachmentRenderer for VoiceNotes {
    fn key(&self) -> &str {
        "voice-notes"
    }

    fn matches(&self, attachment: &Attachment) -> bool {
        attachment.voice_note
    }

    fn render(
        &self,
        attachment: &Attachment,
        id: SharedString,
        media: &Media,
        cx: &App,
    ) -> Option<AnyElement> {
        Some(media::voice_note(id, attachment, media, cx))
    }
}

/// A lone `:shortcode:` naming one of the message's emoji, as an image.
struct Emoji;

impl InlineRenderer for Emoji {
    fn key(&self) -> &str {
        "emoji"
    }

    fn render(
        &self,
        text: &str,
        emoji: &[CustomEmoji],
        media: &Media,
        _: &App,
    ) -> Option<AnyElement> {
        let code = text
            .strip_prefix(':')?
            .strip_suffix(':')?
            .to_ascii_lowercase();
        let entry = emoji.iter().find(|e| e.shortcode == code)?;
        Some(
            img(media::image(&entry.url, media))
                .size(px(14.))
                .object_fit(ObjectFit::Contain)
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    struct Named(&'static str, i32);

    impl LinkRenderer for Named {
        fn key(&self) -> &str {
            self.0
        }
        fn order(&self, _: &str) -> i32 {
            self.1
        }
        fn matches(&self, url: &str) -> bool {
            url.starts_with("https://")
        }
        fn render(&self, _: &str, _: Option<&str>, _: &App) -> Option<AnyElement> {
            None
        }
    }

    struct Broken;

    impl LinkRenderer for Broken {
        fn key(&self) -> &str {
            "broken"
        }
        fn order(&self, _: &str) -> i32 {
            -1
        }
        fn matches(&self, _: &str) -> bool {
            panic!("broken matcher")
        }
        fn render(&self, _: &str, _: Option<&str>, _: &App) -> Option<AnyElement> {
            None
        }
    }

    #[test]
    fn lowest_order_wins_and_broken_matchers_drop_out() {
        let entries: Vec<Rc<dyn LinkRenderer>> = vec![
            Rc::new(Named("late", 5)),
            Rc::new(Broken),
            Rc::new(Named("early", 1)),
            Rc::new(Named("tie", 1)),
        ];
        let order = resolve(
            &entries,
            |l| l.matches("https://x"),
            |l| l.order("https://x"),
        );
        let keys: Vec<&str> = order.iter().map(|l| l.key()).collect();
        assert_eq!(keys, ["early", "tie", "late"]);
        assert_eq!(guarded::<()>("x", || panic!("boom")), None);
    }
}
