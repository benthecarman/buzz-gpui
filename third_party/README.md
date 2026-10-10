# Vendored crates

## gpui-pre-linux 0.3.8

GPUI's Linux platform, from crates.io (Apache-2.0, Zed Industries). The
first commit that adds it here is the published crate unchanged (without
its `Cargo.lock`), so `git log -p third_party/gpui-pre-linux` shows every
change made since. The workspace uses it through `[patch.crates-io]`.
Drop it when upstream has equivalents.

Patches:

- `set_activation_token`: the next window activation uses an
  xdg-activation token the app was handed, so GNOME and other Wayland
  compositors raise the window when a notification is clicked or a second
  launch hands over. Without it they refuse, since the app isn't focused.
- Pasting files: a `text/uri-list` clipboard offer of local files reads as
  `ClipboardEntry::ExternalPaths`, followed by the paths as text, on Wayland
  and X11. Other URI lists still read as text.

Run its tests on their own, since it isn't a workspace member:

```sh
cd third_party/gpui-pre-linux
CARGO_TARGET_DIR=../../target/vendored cargo test --features gpui/test-support --lib
rm Cargo.lock
```

## gpui-base 0.7.1

gpui-kit's base crate, which holds its text input, from crates.io
(Apache-2.0, Longbridge). The first commit that adds it here is the
published crate unchanged (without its `Cargo.lock`), so
`git log -p third_party/gpui-base` shows every change made since. The
workspace uses it through `[patch.crates-io]`. Drop it when upstream has
equivalents.

Patches:

- Decorations in every input mode: the text and range decoration stores
  move from the code editor's extras into the shared `InputBaseState`, and
  `create_decorations_collection`, `create_range_decorations_collection`
  and their collection handles work for `InputState` and `TextareaState`
  too. The handles take the mode as a type parameter that defaults to
  `EditorMode`, so existing code still compiles. The composer uses this to
  style Markdown as it is typed. Before, only `EditorState` could hold
  styled ranges. The renderer already drew decorations in every mode.
- Backgrounds beside inline tokens: a line with a token (a chip) now
  paints the background colors of its text runs. Its layout never marked
  them, so a highlighted background vanished from any line with a chip.

Run its tests on their own, since it isn't a workspace member:

```sh
cd third_party/gpui-base
CARGO_TARGET_DIR=../../target/vendored cargo test --features test-support --lib
rm Cargo.lock
```
