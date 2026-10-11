//! Harness logos and status labels.

use buzz_host::api::{HarnessKind, HarnessStatus};
use buzz_ui::{Brand, BrandMark, Icon, IconName};
use gpui_kit::{AnyElement, IntoElement};

/// The harness logo (Settings → Agents uses 32px; tiles use 14).
pub fn mark(kind: HarnessKind, size: f32) -> AnyElement {
    let brand = match kind {
        HarnessKind::BuzzAgent => return Icon::new(IconName::Robot).size(size).into_any_element(),
        HarnessKind::Goose => BrandMark::Goose,
        HarnessKind::Pi => BrandMark::Pi,
        HarnessKind::Claude => BrandMark::Claude,
        HarnessKind::Hermes => BrandMark::Hermes,
        HarnessKind::Codex => BrandMark::Codex,
    };
    Brand::new(brand).size(size).into_any_element()
}

/// buzz-app's harness status labels; "Unknown" when the host did not say.
pub fn status_label(status: Option<HarnessStatus>) -> &'static str {
    match status {
        Some(HarnessStatus::Ready) => "Ready",
        Some(HarnessStatus::CliNeeded) => "CLI needed",
        Some(HarnessStatus::AdapterNeeded) => "Adapter needed",
        Some(HarnessStatus::CheckNeeded) => "Check needed",
        Some(HarnessStatus::NotEnabled) => "Not enabled",
        None => "Unknown",
    }
}
