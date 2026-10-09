//! Bundled fonts: Inter (static 400/500/600/700, roman and italic) and
//! JetBrains Mono 400, both SIL OFL 1.1 (see `assets/fonts/*/OFL.txt`).
//!
//! buzz-app loads Inter as a variable font. GPUI's Linux text stack picks
//! faces by weight from static fonts reliably, so the static instances of
//! the same Inter 4.1 release are bundled instead.

use gpui_kit::App;
use std::borrow::Cow;

const FONTS: &[&[u8]] = &[
    include_bytes!("../../../assets/fonts/inter/Inter-Regular.ttf"),
    include_bytes!("../../../assets/fonts/inter/Inter-Italic.ttf"),
    include_bytes!("../../../assets/fonts/inter/Inter-Medium.ttf"),
    include_bytes!("../../../assets/fonts/inter/Inter-MediumItalic.ttf"),
    include_bytes!("../../../assets/fonts/inter/Inter-SemiBold.ttf"),
    include_bytes!("../../../assets/fonts/inter/Inter-Bold.ttf"),
    include_bytes!("../../../assets/fonts/inter/Inter-BoldItalic.ttf"),
    include_bytes!("../../../assets/fonts/jetbrains-mono/JetBrainsMono-Regular.ttf"),
];

/// Registers the bundled fonts with GPUI's text system.
pub fn register(cx: &mut App) {
    let fonts = FONTS.iter().map(|bytes| Cow::Borrowed(*bytes)).collect();
    if let Err(err) = cx.text_system().add_fonts(fonts) {
        eprintln!("buzz-ui: failed to register bundled fonts: {err:#}");
    }
}
