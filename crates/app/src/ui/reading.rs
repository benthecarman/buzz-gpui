//! What a message list shows, for automatic reading (`use-reading.ts`):
//! only rows wholly in view count, only while the window is active and
//! focus is in the list or its composer, and only reports that changed
//! are sent, so a redraw does not restart the session's dwell.

use buzz_relay_client::EventId;
use gpui_kit::*;

/// Most rows one report names.
const MOST: usize = 128;

/// What a list shows after its last layout.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct View {
    /// Rows wholly inside the viewport, top first.
    pub visible: Vec<EventId>,
    /// Rows at least partly inside it.
    pub shown: Vec<EventId>,
    /// The list sits at its end.
    pub at_bottom: bool,
}

/// Reads `list` after layout. `id` names the message at a list index, or
/// `None` for rows that are not read: edges, notes and membership rows.
pub fn view(list: &ListState, id: impl Fn(usize) -> Option<EventId>) -> View {
    let viewport = list.viewport_bounds();
    let mut view = View {
        at_bottom: at_bottom(list),
        ..View::default()
    };
    if viewport.size.height <= px(0.) {
        return view;
    }
    for ix in list.logical_scroll_top().item_ix..list.item_count() {
        let Some(bounds) = list.bounds_for_item(ix) else {
            break;
        };
        if bounds.top() >= viewport.bottom() {
            break;
        }
        let Some(id) = id(ix) else {
            continue;
        };
        if bounds.bottom() <= viewport.top() || bounds.size.height <= px(0.) {
            continue;
        }
        view.shown.push(id);
        if bounds.top() >= viewport.top()
            && bounds.bottom() <= viewport.bottom()
            && view.visible.len() < MOST
        {
            view.visible.push(id);
        }
    }
    view
}

/// Within a pixel of the end, as the reference's catch-up requires.
pub fn at_bottom(list: &ListState) -> bool {
    if list.is_following_tail() {
        return true;
    }
    let max = list.max_offset_for_scrollbar().y;
    let offset = -list.scroll_px_offset_for_scrollbar().y;
    max - offset <= px(1.)
}

/// How far the list is from its end.
pub fn distance_from_bottom(list: &ListState) -> Pixels {
    if list.is_following_tail() {
        return px(0.);
    }
    let max = list.max_offset_for_scrollbar().y;
    let offset = -list.scroll_px_offset_for_scrollbar().y;
    (max - offset).max(px(0.))
}

/// The last report sent, so only changes go out.
#[derive(Default)]
pub struct Reporter {
    last: Option<(Vec<EventId>, bool)>,
}

impl Reporter {
    /// What to report now, if it differs from the last report. An inactive
    /// surface reports nothing in view and not at the bottom.
    pub fn next(&mut self, view: &View, active: bool) -> Option<(Vec<EventId>, bool)> {
        let report = if active {
            (view.visible.clone(), view.at_bottom)
        } else {
            (Vec::new(), false)
        };
        if self.last.as_ref() == Some(&report) {
            return None;
        }
        self.last = Some(report.clone());
        Some(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn reports_only_changes_and_nothing_while_inactive() {
        let id = EventId::from_byte_array([1; 32]);
        let view = View {
            visible: vec![id],
            shown: vec![id],
            at_bottom: true,
        };
        let mut reporter = Reporter::default();
        assert_eq!(reporter.next(&view, true), Some((vec![id], true)));
        assert_eq!(reporter.next(&view, true), None);
        assert_eq!(reporter.next(&view, false), Some((Vec::new(), false)));
        assert_eq!(reporter.next(&view, false), None);
        assert_eq!(reporter.next(&view, true), Some((vec![id], true)));
    }
}
