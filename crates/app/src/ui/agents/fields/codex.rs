//! Codex's Default or Advanced settings: buzz-app's
//! `CodexConfigurationFields`. Discovery asks the host's Codex CLI for its
//! models and their effort levels; it never sends a prompt.

use super::{Fields, Open, Owner, Pick, field, note};
use crate::hosts::models::Lookup;
use crate::ui::agents::draft::{Configuration, Draft, Effort};
use buzz_host::api::{Catalog, CodexEffort, ModelAction, ModelOption, ModelRequest};
use buzz_host::harness::CODEX;
use buzz_ui::{ActiveTheme, Button, Surface, TypeStyled};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::rc::Rc;

/// The last Codex lookup and what it found.
#[derive(Default)]
pub(super) struct State {
    /// The context and whether Advanced was chosen; a change starts over.
    key: Option<(String, bool)>,
    catalog: Option<Catalog>,
    status: String,
    lookup: Option<Lookup>,
}

/// What a catalog belongs to. Typed workspace and environment edits apply
/// on Refresh, not per keystroke.
fn context_key(fields: &Fields) -> String {
    serde_json::to_string(&(
        &fields.saved,
        fields.draft.revision,
        &fields.draft.integration,
        &fields.draft.command,
    ))
    .unwrap_or_default()
}

fn advanced(fields: &Fields) -> bool {
    matches!(
        fields.draft.codex_configuration(),
        Some(Configuration::Advanced(_))
    )
}

fn cancel<T>(fields: &mut Fields, cx: &mut Context<T>) {
    if let Some(lookup) = fields.codex.lookup.take() {
        fields
            .host
            .update(cx, |host, cx| host.cancel_models(&lookup, cx));
    }
}

/// Starts over when the context or mode changes, loading Advanced's
/// choices at once. Call before each render.
pub(super) fn sync<T: Owner>(fields: &mut Fields, cx: &mut Context<T>) {
    if !fields.draft.is_codex() {
        if fields.codex.key.is_some() {
            cancel(fields, cx);
            fields.codex = State::default();
        }
        return;
    }
    let key = (context_key(fields), advanced(fields));
    if fields.codex.key.as_ref() == Some(&key) {
        return;
    }
    cancel(fields, cx);
    let advanced = key.1;
    fields.codex = State {
        key: Some(key),
        ..State::default()
    };
    if advanced {
        let model = Some(fields.draft.model.clone()).filter(|model| !model.is_empty());
        discover(fields, model, false, cx);
    }
}

fn request(fields: &Fields, selected: Option<&str>) -> Result<ModelRequest, String> {
    let mut draft = fields.draft.clone();
    if let Some(model) = selected {
        draft.model = model.to_string();
    }
    Ok(ModelRequest {
        id: fields.saved.as_ref().map(|(id, _)| id.clone()),
        expected_revision: fields.saved.as_ref().map(|_| fields.draft.revision),
        edit: Some(super::draft::edit(&draft, true)?),
        action: ModelAction::Refresh,
        integration: Some(CODEX.into()),
        selected_model: selected.map(str::to_string),
        ..Default::default()
    })
}

/// Lists Codex's models, with `selected`'s effort levels. When the
/// selection fails, lists them without it. A `changed` selection then fits
/// the saved effort to the levels it reports.
fn discover<T: Owner>(
    fields: &mut Fields,
    selected: Option<String>,
    changed: bool,
    cx: &mut Context<T>,
) {
    if fields.codex.lookup.is_some() {
        return;
    }
    fields.codex.status.clear();
    let first = match request(fields, selected.as_deref()) {
        Ok(request) => request,
        Err(error) => {
            fields.codex.status = error;
            return;
        }
    };
    let key = context_key(fields);
    let (task, lookup) = fields.host.update(cx, |host, cx| host.models(first, cx));
    fields.codex.lookup = Some(lookup.clone());
    cx.spawn(async move |this, cx| {
        let mut lookup = lookup;
        let mut result = task.await;
        let mut confirmed = true;
        let mut fallback = None;
        if let (Err(error), Some(_)) = (&result, &selected) {
            let retry = this
                .update(cx, |this, cx| {
                    let fields = this.fields();
                    let current = fields
                        .codex
                        .lookup
                        .as_ref()
                        .is_some_and(|current| current.same(&lookup));
                    if !current || lookup.cancelled() || context_key(fields) != key {
                        return None;
                    }
                    let request = request(fields, None).ok()?;
                    let (task, next) = fields.host.update(cx, |host, cx| host.models(request, cx));
                    fields.codex.lookup = Some(next.clone());
                    Some((task, next))
                })
                .ok()
                .flatten();
            if let Some((task, next)) = retry {
                fallback = Some(format!(
                    "{error} Choose a model from the refreshed catalog."
                ));
                lookup = next;
                result = task.await;
                confirmed = false;
            }
        }
        let _ = this.update(cx, |this, cx| {
            let fields = this.fields();
            if !fields
                .codex
                .lookup
                .as_ref()
                .is_some_and(|current| current.same(&lookup))
            {
                return;
            }
            fields.codex.lookup = None;
            cx.notify();
            if lookup.cancelled() || context_key(fields) != key {
                return;
            }
            match result {
                Ok(catalog) => {
                    if let Some(fallback) = fallback {
                        fields.codex.status = fallback;
                    }
                    let selected = selected.filter(|_| changed && confirmed);
                    found(fields, catalog, selected);
                }
                Err(error) => fields.codex.status = error,
            }
        });
    })
    .detach();
}

/// Shows a catalog, fitting the draft to it.
fn found(fields: &mut Fields, catalog: Catalog, selected: Option<String>) {
    let before = fields.draft.configuration.clone();
    if let Some(status) = fit(&mut fields.draft, &catalog, selected.as_deref()) {
        fields.codex.status = status.into();
    }
    if fields.draft.configuration != before {
        fields.dirty = true;
        fields.error = None;
    }
    fields.codex.catalog = Some(catalog);
}

/// After `selected` was chosen, keeps the saved effort only when that
/// model still reports it. Returns what to tell the user.
fn fit(draft: &mut Draft, catalog: &Catalog, selected: Option<&str>) -> Option<&'static str> {
    let codex = catalog.codex.clone().unwrap_or_default();
    if !codex.models_known {
        return Some(
            "Codex did not report a model catalog. Your saved choices are unchanged; refresh \
             after updating the selected tools.",
        );
    }
    if catalog.models.is_empty() {
        return Some("Codex reported no available models. Your saved choices are unchanged.");
    }
    let selected = selected?;
    let Some(effort) = codex.effort.filter(|effort| effort.model == selected) else {
        return Some(
            "Codex did not report effort choices for this model. Your saved effort is unchanged.",
        );
    };
    let Some(Configuration::Advanced(current)) = draft.codex_configuration() else {
        return None;
    };
    let next = if effort.options.is_empty() {
        Effort::Unsupported
    } else {
        match &current {
            Effort::Value(value)
                if value.is_empty() || effort.options.iter().any(|o| o.id == *value) =>
            {
                return None;
            }
            _ => Effort::Value(String::new()),
        }
    };
    if next != current {
        draft.set_codex_configuration(Configuration::Advanced(next));
    }
    None
}

/// The levels reported for the draft's model.
fn effort(fields: &Fields) -> Option<&CodexEffort> {
    fields
        .codex
        .catalog
        .as_ref()?
        .codex
        .as_ref()?
        .effort
        .as_ref()
        .filter(|effort| effort.model == fields.draft.model)
}

impl Fields {
    /// `CodexConfigurationFields`.
    pub(super) fn codex_fields<T: Owner>(
        &self,
        disabled: bool,
        window: &Window,
        cx: &mut Context<T>,
    ) -> Div {
        let c = *cx.colors();
        let configuration = self.draft.codex_configuration();
        let loading = self.codex.lookup.is_some();
        let mode = |configuration: Configuration| -> Pick<T> {
            Rc::new(move |this: &mut T, window, cx| {
                let configuration = configuration.clone();
                let default = configuration == Configuration::Default;
                this.fields().patch(
                    |draft| {
                        if default {
                            draft.model.clear();
                        }
                        draft.set_codex_configuration(configuration);
                    },
                    window,
                    cx,
                );
            })
        };
        let is_default = configuration == Some(Configuration::Default);
        let is_advanced = matches!(configuration, Some(Configuration::Advanced(_)));
        let modes = vec![
            (
                "Default".to_string(),
                is_default,
                false,
                mode(Configuration::Default),
            ),
            (
                "Advanced".to_string(),
                is_advanced,
                false,
                mode(Configuration::Advanced(Effort::Value(String::new()))),
            ),
        ];
        let mut section = div().flex().flex_col().gap(rems(1.)).child(field(
            "Codex configuration",
            Some(if is_default {
                "Codex chooses the model and effort from its effective configuration each time \
                 this agent starts."
                    .into()
            } else {
                "Choose an advertised model and one of that model’s reported effort levels.".into()
            }),
            self.select(
                Open::CodexMode,
                "codex-mode-select",
                match configuration {
                    Some(Configuration::Default) => "Default".into(),
                    Some(Configuration::Advanced(_)) => "Advanced".into(),
                    None => String::new(),
                },
                disabled,
                modes,
                window,
                cx,
            ),
            &c,
        ));
        let Some(Configuration::Advanced(saved)) = configuration else {
            return section;
        };

        let catalog = self.codex.catalog.as_ref();
        let known = catalog
            .and_then(|catalog| catalog.codex.as_ref())
            .is_some_and(|codex| codex.models_known);
        let mut models: Vec<ModelOption> = catalog
            .filter(|_| known)
            .map(|catalog| catalog.models.clone())
            .unwrap_or_default();
        let model = self.draft.model.clone();
        if !model.is_empty() && !models.iter().any(|option| option.id == model) {
            models.insert(
                0,
                ModelOption {
                    id: model.clone(),
                    name: format!("{model} (saved)"),
                },
            );
        }
        let choose_model = |value: String| -> Pick<T> {
            Rc::new(move |this: &mut T, window, cx| {
                let value = value.clone();
                let fields = this.fields();
                fields.patch(|draft| draft.model = value.clone(), window, cx);
                if !value.is_empty() {
                    discover(fields, Some(value), true, cx);
                }
            })
        };
        let mut model_items = vec![(
            "Choose a model".to_string(),
            model.is_empty(),
            false,
            choose_model(String::new()),
        )];
        model_items.extend(models.iter().map(|option| {
            (
                option.name.clone(),
                option.id == model,
                false,
                choose_model(option.id.clone()),
            )
        }));
        let model_label = model_items
            .iter()
            .find(|(_, checked, ..)| *checked)
            .map(|(label, ..)| label.clone())
            .unwrap_or_default();
        section = section.child(field(
            "Codex model",
            None,
            self.select(
                Open::CodexModel,
                "codex-model-select",
                model_label,
                disabled || loading || !known,
                model_items,
                window,
                cx,
            ),
            &c,
        ));

        let effort = effort(self);
        let saved_effort = match &saved {
            Effort::Value(value) if !value.is_empty() => Some(value.clone()),
            _ => None,
        };
        if !model.is_empty() && effort.is_some_and(|effort| effort.options.is_empty()) {
            section = section.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(rems(0.5))
                    .child(note(
                        match &saved_effort {
                            Some(saved) => {
                                format!(
                                    "The saved {saved} effort is no longer supported by this model."
                                )
                            }
                            None => "This model reports no effort control. Codex will use its \
                                     model configuration."
                                .into(),
                        },
                        &c,
                    ))
                    .when(saved_effort.is_some(), |this| {
                        this.child(
                            div().flex().child(
                                Button::new(
                                    "codex-use-model-configuration",
                                    "Use model configuration",
                                )
                                .surface(Surface::Floating)
                                .disabled(disabled || loading)
                                .on_click(cx.listener(
                                    |this: &mut T, _, window, cx| {
                                        this.fields().patch(
                                            |draft| {
                                                draft.set_codex_configuration(
                                                    Configuration::Advanced(Effort::Unsupported),
                                                )
                                            },
                                            window,
                                            cx,
                                        );
                                        cx.notify();
                                    },
                                )),
                            ),
                        )
                    }),
            );
        } else {
            let mut options: Vec<ModelOption> = effort
                .map(|effort| effort.options.clone())
                .unwrap_or_default();
            if let Some(saved) = &saved_effort
                && !options.iter().any(|option| option.id == *saved)
            {
                options.insert(
                    0,
                    ModelOption {
                        id: saved.clone(),
                        name: format!("{saved} (saved)"),
                    },
                );
            }
            let value = match &saved {
                Effort::Value(value) => value.clone(),
                Effort::Unsupported => "unsupported".into(),
            };
            let choose_effort = |level: String| -> Pick<T> {
                Rc::new(move |this: &mut T, window, cx| {
                    let level = level.clone();
                    this.fields().patch(
                        |draft| {
                            draft.set_codex_configuration(Configuration::Advanced(Effort::Value(
                                level,
                            )))
                        },
                        window,
                        cx,
                    );
                })
            };
            let mut items = vec![(
                "Choose an effort level".to_string(),
                value.is_empty(),
                false,
                choose_effort(String::new()),
            )];
            items.extend(options.iter().map(|option| {
                (
                    option.name.clone(),
                    option.id == value,
                    false,
                    choose_effort(option.id.clone()),
                )
            }));
            let label = items
                .iter()
                .find(|(_, checked, ..)| *checked)
                .map(|(label, ..)| label.clone())
                .unwrap_or_default();
            section = section.child(field(
                "Codex effort",
                None,
                self.select(
                    Open::CodexEffort,
                    "codex-effort-select",
                    label,
                    disabled || loading || effort.is_none(),
                    items,
                    window,
                    cx,
                ),
                &c,
            ));
        }
        if loading {
            section = section.child(note("Loading Codex models and effort choices…", &c));
        }
        if !self.codex.status.is_empty() {
            section = section.child(note(self.codex.status.clone(), &c));
        }
        let stale = saved_effort.as_ref().is_some_and(|saved| {
            effort.is_some_and(|effort| !effort.options.iter().any(|o| o.id == *saved))
        });
        if stale {
            section = section.child(div().text_body_sm().text_color(c.text_warning).child(
                "The saved effort is no longer reported for this model. Choose an \
                         available effort level before saving.",
            ));
        }
        section.child(div().flex().flex_wrap().gap(rems(0.5)).child(if loading {
            Button::new("codex-cancel-models", "Cancel model lookup")
                .surface(Surface::Floating)
                .disabled(disabled)
                .on_click(cx.listener(|this: &mut T, _, _, cx| {
                    cancel(this.fields(), cx);
                    cx.notify();
                }))
        } else {
            Button::new("codex-refresh-models", "Refresh Codex models")
                .surface(Surface::Floating)
                .disabled(disabled)
                .on_click(cx.listener(|this: &mut T, _, _, cx| {
                    let fields = this.fields();
                    let model = Some(fields.draft.model.clone()).filter(|m| !m.is_empty());
                    discover(fields, model, false, cx);
                    cx.notify();
                }))
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use buzz_host::api::CodexCatalog;
    use core::prelude::v1::test;

    fn catalog(models: &[&str], effort: Option<(&str, &[&str])>) -> Catalog {
        let option = |id: &str| ModelOption {
            id: id.into(),
            name: id.into(),
        };
        Catalog {
            models: models.iter().map(|id| option(id)).collect(),
            codex: Some(CodexCatalog {
                models_known: true,
                effort: effort.map(|(model, levels)| CodexEffort {
                    model: model.into(),
                    current: None,
                    options: levels.iter().map(|id| option(id)).collect(),
                }),
            }),
            ..Default::default()
        }
    }

    /// [`fit`] on a draft whose saved effort is `effort`.
    fn after(effort: Effort, catalog: Catalog, selected: Option<&str>) -> (Effort, String) {
        let mut draft = Draft {
            integration: Some(CODEX.into()),
            model: "gpt-5.5".into(),
            ..Default::default()
        };
        draft.set_codex_configuration(Configuration::Advanced(effort));
        let status = fit(&mut draft, &catalog, selected).unwrap_or_default();
        let Some(Configuration::Advanced(effort)) = draft.codex_configuration() else {
            panic!("still Advanced");
        };
        (effort, status.into())
    }

    #[test]
    fn a_new_model_keeps_only_an_effort_it_reports() {
        let levels = catalog(&["gpt-5.5"], Some(("gpt-5.5", &["low", "high"])));
        let (effort, _) = after(
            Effort::Value("high".into()),
            levels.clone(),
            Some("gpt-5.5"),
        );
        assert_eq!(effort, Effort::Value("high".into()));
        let (effort, _) = after(Effort::Value("max".into()), levels.clone(), Some("gpt-5.5"));
        assert_eq!(effort, Effort::Value(String::new()));
        let (effort, _) = after(Effort::Unsupported, levels, Some("gpt-5.5"));
        assert_eq!(effort, Effort::Value(String::new()));
        let none = catalog(&["gpt-5.5"], Some(("gpt-5.5", &[])));
        let (effort, _) = after(Effort::Value("high".into()), none, Some("gpt-5.5"));
        assert_eq!(effort, Effort::Unsupported);
    }

    #[test]
    fn missing_reports_leave_the_saved_effort() {
        let (effort, status) = after(
            Effort::Value("high".into()),
            catalog(&["gpt-5.5"], None),
            Some("gpt-5.5"),
        );
        assert_eq!(effort, Effort::Value("high".into()));
        assert_eq!(
            status,
            "Codex did not report effort choices for this model. Your saved effort is unchanged."
        );
        let (_, status) = after(Effort::Unsupported, catalog(&[], None), None);
        assert_eq!(
            status,
            "Codex reported no available models. Your saved choices are unchanged."
        );
        let unknown = Catalog::default();
        let (_, status) = after(Effort::Unsupported, unknown, None);
        assert_eq!(
            status,
            "Codex did not report a model catalog. Your saved choices are unchanged; refresh \
             after updating the selected tools."
        );
    }
}
