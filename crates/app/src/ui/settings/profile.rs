//! Settings → Profile (§5b.5 item 1): the selected community's profile,
//! identity details and sign-out.

use super::{
    Modal, SettingsPage, avatar, button, field, form_group, header, inline_header, note,
    preference_row, rows_group, section,
};
use crate::community::{AppState, Community, IdentityState};
use crate::ui::media;
use buzz_relay_client::ProfileEdit;
use buzz_relay_client::account::{ABOUT_LIMIT, picture_error};
use buzz_relay_client::upload;
use buzz_ui::{
    ActiveTheme, AlertDialog, Avatar, AvatarSize, Button, ButtonVariant, IconButton,
    IconButtonSize, IconName, Surface, TextArea, TextField, TypeStyled,
};
use gpui_kit::component::input::{InputEvent, InputState, TextareaState};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

/// The profile editor's fields and what they were loaded from.
pub(super) struct ProfileForm {
    community: Option<EntityId>,
    name: Entity<InputState>,
    about: Entity<TextareaState>,
    picture: Entity<InputState>,
    /// The saved profile the fields started from.
    baseline: ProfileEdit,
    saving: bool,
    uploading: bool,
    error: Option<SharedString>,
    _subscriptions: Vec<Subscription>,
}

impl ProfileForm {
    pub(super) fn new(window: &mut Window, cx: &mut Context<SettingsPage>) -> Self {
        let name = cx.new(|cx| InputState::new(window, cx));
        let about = cx.new(|cx| TextareaState::new(window, cx));
        let picture = cx.new(|cx| InputState::new(window, cx).placeholder("https://"));
        let changed = |this: &mut SettingsPage, cx: &mut Context<SettingsPage>| {
            this.profile.error = None;
            cx.notify();
        };
        let _subscriptions = vec![
            cx.subscribe(&name, move |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    changed(this, cx);
                }
            }),
            cx.subscribe(&about, move |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    changed(this, cx);
                }
            }),
            cx.subscribe(&picture, move |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    changed(this, cx);
                }
            }),
        ];
        Self {
            community: None,
            name,
            about,
            picture,
            baseline: ProfileEdit::default(),
            saving: false,
            uploading: false,
            error: None,
            _subscriptions,
        }
    }

    fn values(&self, cx: &App) -> ProfileEdit {
        ProfileEdit {
            name: self.name.read(cx).value().to_string(),
            picture: self.picture.read(cx).value().to_string(),
            about: self.about.read(cx).value().to_string(),
        }
    }

    fn dirty(&self, cx: &App) -> bool {
        self.values(cx) != self.baseline
    }

    /// Loads the viewer's profile from `community`, unless the user is
    /// editing it there.
    pub(super) fn sync(
        &mut self,
        community: Option<&Entity<Community>>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let id = community.map(Entity::entity_id);
        let switched = id != self.community;
        if !switched && (self.saving || self.dirty(cx)) {
            return;
        }
        let saved = community
            .map(|community| saved_profile(community, cx))
            .unwrap_or_default();
        if !switched && saved == self.baseline {
            return;
        }
        self.community = id;
        self.error = None;
        self.set(saved, window, cx);
    }

    fn set(&mut self, values: ProfileEdit, window: &mut Window, cx: &mut App) {
        self.name.update(cx, |state, cx| {
            state.set_value(values.name.clone(), window, cx)
        });
        self.about.update(cx, |state, cx| {
            state.set_value(values.about.clone(), window, cx)
        });
        self.picture.update(cx, |state, cx| {
            state.set_value(values.picture.clone(), window, cx)
        });
        self.baseline = values;
    }
}

/// The viewer's saved profile in `community`, as the editor's fields.
fn saved_profile(community: &Entity<Community>, cx: &App) -> ProfileEdit {
    let community = community.read(cx);
    let viewer = community.session.viewer();
    community
        .snapshot
        .profiles
        .get(viewer)
        .map(|profile| ProfileEdit {
            name: if profile.name_is_fallback {
                String::new()
            } else {
                profile.name.clone()
            },
            picture: profile.picture.clone().unwrap_or_default(),
            about: profile.about.clone().unwrap_or_default(),
        })
        .unwrap_or_default()
}

impl SettingsPage {
    fn cancel_profile(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let baseline = self.profile.baseline.clone();
        self.profile.error = None;
        self.profile.set(baseline, window, cx);
        cx.notify();
    }

    fn save_profile(&mut self, window: &Window, cx: &mut Context<Self>) {
        let Some(community) = AppState::current(cx) else {
            return;
        };
        let edit = self.profile.values(cx);
        if let Err(error) = buzz_relay_client::account::validate(&edit) {
            self.profile.error = Some(error.to_string().into());
            cx.notify();
            return;
        }
        self.profile.saving = true;
        self.profile.error = None;
        let session = community.read(cx).session.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = session.publish_profile(edit.clone()).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.profile.saving = false;
                match result {
                    Ok(()) => {
                        let saved = buzz_relay_client::account::validate(&edit).unwrap_or(edit);
                        this.profile.set(saved, window, cx);
                        this.show_toast("Profile updated", cx);
                    }
                    Err(error) => this.profile.error = Some(error.into()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Picks an image, prepares it and uploads it to the community, then
    /// puts its URL in the avatar field.
    fn upload_avatar(&mut self, window: &Window, cx: &mut Context<Self>) {
        let Some(community) = AppState::current(cx) else {
            return;
        };
        let session = community.read(cx).session.clone();
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose an avatar".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                this.profile.uploading = true;
                this.profile.error = None;
                cx.notify();
            });
            let file = cx
                .background_executor()
                .spawn(async move { avatar::read(&path) })
                .await;
            let result = match file {
                Ok(file) => upload::upload(session.media(), file)
                    .await
                    .map_err(|error| error.to_string())
                    .and_then(|blob| {
                        if blob.mime.starts_with("image/") {
                            Ok(blob.url)
                        } else {
                            Err("The server did not return an avatar image.".to_string())
                        }
                    }),
                Err(error) => Err(error),
            };
            let _ = this.update_in(cx, |this, window, cx| {
                this.profile.uploading = false;
                match result {
                    Ok(url) => this
                        .profile
                        .picture
                        .update(cx, |state, cx| state.set_value(url, window, cx)),
                    Err(error) => this.profile.error = Some(error.into()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn profile_section(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let c = *cx.colors();
        let community = AppState::current(cx);
        let subtitle = if community.is_some() {
            "Update this community’s profile. Other community profiles stay unchanged."
        } else {
            "Profiles belong to communities. Add a community to set up yours."
        };
        let mut parts = vec![header("Profile", Some(subtitle.into()), cx).into_any_element()];
        let identity = match &cx.global::<AppState>().identity {
            IdentityState::Ready(identity) => Some(identity.clone()),
            _ => None,
        };
        let Some(identity) = identity else {
            parts.push(note("Your identity is unavailable.", cx).into_any_element());
            return section(parts);
        };
        if let Some(community) = &community {
            parts.push(self.profile_form(community, window, cx).into_any_element());
        }

        // Identity details.
        let hex = identity.pubkey_hex();
        let npub = {
            use nostr::nips::nip19::ToBech32;
            identity.public_key().to_bech32().unwrap_or_default()
        };
        let copy_row = |id: &'static str,
                        label: &'static str,
                        what: &'static str,
                        value: String,
                        cx: &mut Context<Self>| {
            let copied = value.clone();
            preference_row(
                None,
                label,
                Some(
                    div()
                        .text_mono()
                        .text_size(px(12.))
                        .text_color(c.text_subtle)
                        .child(value)
                        .into_any_element(),
                ),
                vec![
                    IconButton::new(id, IconName::Copy)
                        .size(IconButtonSize::Sm)
                        .variant(ButtonVariant::Ghost)
                        .tooltip(format!("Copy {what}"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(copied.clone()));
                            this.show_toast(format!("Copied {what}"), cx);
                        }))
                        .into_any_element(),
                ],
                cx,
            )
            .into_any_element()
        };
        let rows = vec![
            copy_row("copy-hex", "Public key (hex)", "public key", hex, cx),
            copy_row(
                "copy-npub",
                "Nostr address (npub)",
                "Nostr address",
                npub,
                cx,
            ),
        ];
        let secret = identity.clone();
        parts.push(
            div()
                .flex()
                .flex_col()
                .child(inline_header(
                    "Identity details",
                    Some(
                        "Your identity is shared across communities. Public keys are safe to share."
                            .into(),
                    ),
                    Vec::new(),
                    cx,
                ))
                .child(rows_group(rows, cx))
                .child(
                    div().mt(rems(1.5)).child(rows_group(
                        vec![
                            preference_row(
                                None,
                                "Private key",
                                Some(
                                    "Your nsec signs you in on another device. Anyone who has it \
                                     can act as you, so keep it secret."
                                        .into_any_element(),
                                ),
                                vec![
                                    button("copy-nsec", "Copy private key")
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            cx.write_to_clipboard(ClipboardItem::new_string(
                                                secret.nsec().to_string(),
                                            ));
                                            this.show_toast("Copied private key", cx);
                                        }))
                                        .into_any_element(),
                                ],
                                cx,
                            )
                            .into_any_element(),
                            preference_row(
                                None,
                                "Sign out of Buzz",
                                Some(
                                    "Remove your private key from this device. buzz-app uses \
                                     the same key, so it signs out too."
                                        .into_any_element(),
                                ),
                                vec![
                                    button("sign-out", "Sign out of Buzz")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.modal = Some(Modal::SignOut { error: None });
                                            cx.notify();
                                        }))
                                        .into_any_element(),
                                ],
                                cx,
                            )
                            .into_any_element(),
                        ],
                        cx,
                    )),
                )
                .into_any_element(),
        );
        section(parts)
    }

    fn profile_form(
        &mut self,
        community: &Entity<Community>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let c = *cx.colors();
        let form = &self.profile;
        let values = form.values(cx);
        let dirty = form.dirty(cx);
        let busy = form.saving || form.uploading;
        let media = community.read(cx).session.media().clone();
        let name = values.name.trim().to_string();
        let about_len = values.about.encode_utf16().count();
        let about_error = (about_len > ABOUT_LIMIT).then(|| {
            SharedString::from(format!(
                "Shorten the description to {ABOUT_LIMIT} characters before saving."
            ))
        });
        let picture_error =
            picture_error(values.picture.trim()).map(|error| SharedString::from(error.to_string()));

        // Compact AvatarEditor: an 80px avatar with the add button over its
        // lower right edge.
        let mut avatar = Avatar::new(if name.is_empty() { "?" } else { &name }.to_string())
            .size(AvatarSize::Px(80.));
        let picture = values.picture.trim();
        if picture.starts_with("https://") && picture_error.is_none() {
            avatar = avatar.image(media::avatar(picture, &media));
        }
        let editor = div()
            .relative()
            .flex_none()
            .size(rems(5.))
            .child(avatar)
            .child(
                div()
                    .absolute()
                    .right(rems(-0.25))
                    .bottom(rems(-0.25))
                    .child(
                        IconButton::new("avatar-upload", IconName::Plus)
                            .size(IconButtonSize::Sm)
                            .variant(ButtonVariant::Prominent)
                            .shape(buzz_ui::IconButtonShape::Round)
                            .disabled(busy)
                            .tooltip(if form.uploading {
                                "Uploading…"
                            } else {
                                "Upload an image"
                            })
                            .on_click(
                                cx.listener(|this, _, window, cx| this.upload_avatar(window, cx)),
                            ),
                    ),
            );
        let preview = div()
            .flex()
            .items_center()
            .gap(rems(1.))
            .child(editor)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .min_w_0()
                    .gap(rems(0.25))
                    .child(div().text_label_sm().child(if name.is_empty() {
                        "Your profile".to_string()
                    } else {
                        name.clone()
                    }))
                    .when(!values.about.trim().is_empty(), |this| {
                        this.child(
                            div()
                                .text_body_sm()
                                .text_color(c.text_subtle)
                                .child(values.about.trim().to_string()),
                        )
                    }),
            );
        let actions = dirty.then(|| {
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .justify_end()
                .gap(rems(0.75))
                .mt(rems(0.5))
                .child(
                    button("profile-cancel", "Cancel").disabled(busy).on_click(
                        cx.listener(|this, _, window, cx| this.cancel_profile(window, cx)),
                    ),
                )
                .child(
                    button("profile-save", "Save")
                        .prominent()
                        .loading(form.saving)
                        .disabled(
                            form.uploading
                                || buzz_relay_client::account::validate(&values).is_err(),
                        )
                        .on_click(cx.listener(|this, _, window, cx| this.save_profile(window, cx))),
                )
        });
        form_group(
            vec![
                preview.into_any_element(),
                field(
                    "Display name",
                    TextField::new(&form.name).disabled(form.saving),
                    None,
                    None,
                    cx,
                )
                .into_any_element(),
                field(
                    "Profile description (optional)",
                    TextArea::new(&form.about)
                        .rows(3)
                        .invalid(about_error.is_some())
                        .disabled(form.saving),
                    Some(format!("{about_len} of {ABOUT_LIMIT} characters").into()),
                    about_error,
                    cx,
                )
                .into_any_element(),
                field(
                    "Avatar URL (optional)",
                    TextField::new(&form.picture)
                        .invalid(picture_error.is_some())
                        .disabled(busy),
                    Some(if form.uploading {
                        "Uploading your image…".into()
                    } else {
                        "An HTTPS image URL, or upload one with the + button.".into()
                    }),
                    picture_error,
                    cx,
                )
                .into_any_element(),
            ]
            .into_iter()
            .chain(form.error.clone().map(|error| {
                div()
                    .text_body_sm()
                    .text_color(c.text_danger)
                    .child(error)
                    .into_any_element()
            }))
            .chain(actions.map(IntoElement::into_any_element))
            .collect(),
            cx,
        )
    }

    pub(super) fn sign_out_dialog(
        &self,
        error: Option<SharedString>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        sign_out_dialog(
            error,
            cx.listener(|this, _, _, cx| this.close_modal(cx)),
            cx.listener(|this, _, _, cx| {
                if let Err(error) = AppState::sign_out(cx) {
                    this.modal = Some(Modal::SignOut {
                        error: Some(error.into()),
                    });
                    cx.notify();
                } else {
                    this.modal = None;
                    cx.notify();
                }
            }),
            cx,
        )
    }
}

/// The sign-out confirmation, shared with the account menu. The key is
/// buzz-app's too, so the copy says that signing out here signs out there.
pub fn sign_out_dialog(
    error: Option<SharedString>,
    on_cancel: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    on_confirm: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> AnyElement {
    let c = cx.colors();
    AlertDialog::new("Sign out of Buzz?")
        .description(
            "This deletes your private key from this computer’s keyring. buzz-app uses the \
             same key, so it is signed out too. Copy your private key from Settings → \
             Profile first if you want to sign back in with it.",
        )
        .children(error.map(|error| div().text_body_sm().text_color(c.text_danger).child(error)))
        .action(
            Button::new("sign-out-cancel", "Cancel")
                .surface(Surface::Floating)
                .on_click(on_cancel),
        )
        .action(
            Button::new("sign-out-confirm", "Sign out")
                .destructive()
                .on_click(on_confirm),
        )
        .into_any_element()
}
