//! Completions plugins add to the composer (`conversation.registerCompletion`
//! in `features/conversation/contracts.ts`): typing a plugin's trigger
//! character, such as `/`, at the start of a word opens a popup of its
//! suggestions, and picking one replaces the trigger and query with its
//! text. `@`, `#` and `:` stay the composer's own.
//!
//! A plugin answers later than the composer paints, so a query reads
//! `None` until its answer is in; [`generation`] changes when one arrives
//! and the composer asks again.

use gpui_kit::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// One suggestion.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub label: SharedString,
    pub detail: Option<SharedString>,
    /// What a pick puts in place of the trigger and query.
    pub text: String,
}

/// The suggestions for a query typed in a channel; `None` while waiting.
pub type Query = Rc<dyn Fn(&str, &str, &App) -> Option<Vec<Item>>>;

pub struct PluginCompletion {
    pub key: &'static str,
    pub trigger: char,
    /// Of completions on one trigger, the lowest order, then the lowest
    /// key, answers it.
    pub order: i32,
    pub query: Query,
}

thread_local! {
    static REGISTRY: RefCell<Vec<Rc<PluginCompletion>>> = RefCell::default();
    static GENERATION: Cell<u64> = const { Cell::new(0) };
}

/// Adds a completion, replacing one with the same key.
pub fn register(completion: PluginCompletion) {
    REGISTRY.with_borrow_mut(|registry| {
        registry.retain(|existing| existing.key != completion.key);
        registry.push(Rc::new(completion));
        registry.sort_by(|a, b| a.order.cmp(&b.order).then_with(|| a.key.cmp(b.key)));
    });
    changed();
}

pub fn unregister(key: &str) {
    REGISTRY.with_borrow_mut(|registry| registry.retain(|existing| existing.key != key));
    changed();
}

/// The triggers and the completion each opens: the first by order.
pub fn triggers() -> Vec<(char, &'static str)> {
    REGISTRY.with_borrow(|registry| {
        let mut triggers: Vec<(char, &'static str)> = Vec::new();
        for completion in registry.iter() {
            if !triggers
                .iter()
                .any(|(trigger, _)| *trigger == completion.trigger)
            {
                triggers.push((completion.trigger, completion.key));
            }
        }
        triggers
    })
}

pub fn get(key: &str) -> Option<Rc<PluginCompletion>> {
    REGISTRY.with_borrow(|registry| {
        registry
            .iter()
            .find(|completion| completion.key == key)
            .cloned()
    })
}

/// Changes when a completion is added or removed, or an answer arrives.
pub fn generation() -> u64 {
    GENERATION.get()
}

pub fn changed() {
    GENERATION.set(GENERATION.get() + 1);
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn the_lowest_order_answers_a_trigger() {
        let add = |key: &'static str, order| {
            register(PluginCompletion {
                key,
                trigger: '/',
                order,
                query: Rc::new(|_, _, _| None),
            })
        };
        add("b.plugin/commands", 0);
        add("a.plugin/commands", 0);
        assert_eq!(triggers(), [('/', "a.plugin/commands")]);
        add("c.plugin/commands", -1);
        assert_eq!(triggers(), [('/', "c.plugin/commands")]);
        for key in [
            "a.plugin/commands",
            "b.plugin/commands",
            "c.plugin/commands",
        ] {
            unregister(key);
        }
        assert!(triggers().is_empty());
    }
}
