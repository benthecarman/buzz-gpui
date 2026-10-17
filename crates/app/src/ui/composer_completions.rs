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
    pub query: Query,
}

thread_local! {
    static REGISTRY: RefCell<Vec<Rc<PluginCompletion>>> = RefCell::default();
    static GENERATION: Cell<u64> = const { Cell::new(0) };
}

/// Adds a completion, replacing one with the same key or trigger.
pub fn register(completion: PluginCompletion) {
    REGISTRY.with_borrow_mut(|registry| {
        registry.retain(|existing| {
            existing.key != completion.key && existing.trigger != completion.trigger
        });
        registry.push(Rc::new(completion));
    });
    changed();
}

pub fn unregister(key: &str) {
    REGISTRY.with_borrow_mut(|registry| registry.retain(|existing| existing.key != key));
    changed();
}

/// The triggers and the completion each opens.
pub fn triggers() -> Vec<(char, &'static str)> {
    REGISTRY.with_borrow(|registry| {
        registry
            .iter()
            .map(|completion| (completion.trigger, completion.key))
            .collect()
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
