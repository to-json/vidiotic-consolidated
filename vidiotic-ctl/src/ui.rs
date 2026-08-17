//! Shared binding-table widgets, in the phosphor buffer idiom.
//!
//! Three editors draw the same table over different maps and vocabularies: the
//! ctl bin (any `.vmap`, every action), `vidiotic-prep`'s inspector (the
//! project's player layer, embedded in the `.viproj`), and prep's own
//! `prep.vmap`. `catalog` is what lets one widget serve all three — an editor
//! must not offer a verb the target app can't run.
//!
//! These take `&mut ControlMap` and report what the user asked for, rather than
//! touching an app struct: this crate has no idea what a `CtlApp` or a
//! `PrepApp` is, and shouldn't. Callers own learn state, dirty tracking, and
//! persistence.

use phosphor::icon;
use phosphor::theme::palette;
use phosphor::widgets;

use crate::event::source_key;
use crate::model::{Action, ControlMap, ControlSource, PrepVerb};

/// What a row's controls asked for this frame. The caller applies it — only
/// the caller knows how learn sessions and persistence work.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TableEvent {
    /// Bind the next actuation to this row.
    Learn(usize),
    Remove(usize),
    Add,
}

/// Draw an editable table over `map.bindings`.
///
/// `learning` is the row currently capturing a source, drawn highlighted.
/// `catalog` bounds which actions the picker offers. Sets `changed` if an
/// action was edited in place (the source can only change via a learn
/// session, which the caller drives).
pub fn binding_table(
    ui: &mut egui::Ui,
    map: &mut ControlMap,
    learning: Option<usize>,
    catalog: &[Action],
    changed: &mut bool,
) -> Option<TableEvent> {
    let mut event = None;
    let p = palette();

    for i in 0..map.bindings.len() {
        ui.push_id(i, |ui| {
            let is_learning = learning == Some(i);
            let border = if is_learning { p.accent } else { p.border };
            egui::Frame::group(ui.style())
                .stroke(egui::Stroke::new(1.0, border))
                .corner_radius(egui::CornerRadius::ZERO)
                .show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        let label = if is_learning {
                            "(learning…)".to_string()
                        } else {
                            source_key(&map.bindings[i].source)
                        };
                        ui.label(
                            egui::RichText::new(label)
                                .monospace()
                                .color(if is_learning { p.accent } else { p.fg_primary }),
                        );
                        if widgets::bracket_button(
                            ui,
                            "learn",
                            if is_learning { Some(p.accent) } else { None },
                            0.0,
                        )
                        .on_hover_text("bind the next MIDI/key/gamepad actuation")
                        .clicked()
                        {
                            event = Some(TableEvent::Learn(i));
                        }
                        if widgets::bracket_button(ui, icon::DELETE, Some(p.error), 0.0)
                            .on_hover_text("remove binding")
                            .clicked()
                        {
                            event = Some(TableEvent::Remove(i));
                        }
                    });
                    ui.horizontal_wrapped(|ui| {
                        *changed |= action_picker(ui, &mut map.bindings[i].action, catalog);
                    });
                });
        });
        ui.add_space(4.0);
    }

    ui.add_space(8.0);
    if widgets::bracket_button(ui, &format!("{} add binding", icon::ADD), None, 0.0).clicked() {
        event = Some(TableEvent::Add);
    }
    event
}

/// The namespaces `catalog` spans, in catalog order. `Action::Nothing` has no
/// namespace — it is the universal mask and belongs to all of them.
fn namespaces_of(catalog: &[Action]) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    for ns in catalog.iter().filter_map(Action::namespace) {
        if !out.contains(&ns) {
            out.push(ns);
        }
    }
    out
}

/// Two-level picker over `catalog`: a namespace row ([`Action::namespace`]),
/// then that namespace's verbs, then the selected verb's params as
/// `DragValue`s. The namespace row is omitted when `catalog` spans only one —
/// a single-app editor shouldn't pay for a choice it doesn't have.
///
/// Returns true if `action` changed.
pub fn action_picker(ui: &mut egui::Ui, action: &mut Action, catalog: &[Action]) -> bool {
    let mut changed = false;
    let namespaces = namespaces_of(catalog);

    // `Nothing` names no namespace, so fall back to the first — it's offered
    // under every namespace anyway, so the choice only affects which verbs
    // are listed alongside it.
    let active_ns = action.namespace().or_else(|| namespaces.first().copied());

    if namespaces.len() > 1 {
        let selected = active_ns.and_then(|ns| namespaces.iter().position(|x| *x == ns));
        if let Some(i) = widgets::segmented(ui, "action_ns", &namespaces, selected) {
            let picked = namespaces[i];
            if Some(picked) != action.namespace() {
                // Land on that namespace's first verb rather than keeping an
                // action the newly-selected list doesn't contain.
                if let Some(first) = catalog.iter().find(|a| a.namespace() == Some(picked)) {
                    *action = *first;
                    changed = true;
                }
            }
        }
    }

    // The verbs on offer: the active namespace's, plus the universal mask.
    let ns = action.namespace().or_else(|| namespaces.first().copied());
    let verbs: Vec<&Action> = catalog
        .iter()
        .filter(|a| a.namespace().is_none() || a.namespace() == ns)
        .collect();
    let labels: Vec<&str> = verbs.iter().map(|a| a.label()).collect();
    let current = verbs.iter().position(|a| a.same_kind(action));
    if let Some(i) = widgets::segmented(ui, "action_kind", &labels, current) {
        *action = *verbs[i];
        changed = true;
    }

    changed |= action_params(ui, action);
    changed
}

/// The selected action's params, edited in place. Separate from the kind
/// picker so switching kinds resets to the catalog's placeholder params.
fn action_params(ui: &mut egui::Ui, action: &mut Action) -> bool {
    let mut changed = false;
    let mut drag = |ui: &mut egui::Ui, label: &str, w: egui::DragValue<'_>| {
        ui.label(label);
        changed |= ui.add(w).changed();
    };
    match action {
        Action::BpmDelta { amount } => drag(ui, "amount", egui::DragValue::new(amount).speed(0.1)),
        Action::NudgeBpm { ratio } => drag(ui, "ratio", egui::DragValue::new(ratio).speed(0.001)),
        Action::CycleLiveBank { delta } => drag(ui, "delta", egui::DragValue::new(delta)),
        Action::SetLiveBank { index } | Action::SetEditBank { index } => {
            drag(ui, "index", egui::DragValue::new(index));
        }
        Action::SetBpm { min, max } => {
            drag(ui, "min", egui::DragValue::new(min).range(1.0..=999.0));
            drag(ui, "max", egui::DragValue::new(max).range(1.0..=999.0));
        }
        Action::Prep(PrepVerb::Shuttle { dir }) => {
            drag(ui, "dir", egui::DragValue::new(dir).range(-1..=1));
        }
        Action::Prep(PrepVerb::Step { frames }) => {
            drag(ui, "frames", egui::DragValue::new(frames));
        }
        Action::Prep(PrepVerb::ZoomView { factor }) => {
            drag(
                ui,
                "factor",
                egui::DragValue::new(factor).speed(0.05).range(0.05..=8.0),
            );
        }
        Action::BpmDigit { digit } => {
            drag(ui, "digit", egui::DragValue::new(digit).range(0..=9));
        }
        Action::Nothing
        | Action::TapDownbeat
        | Action::TapTempo
        | Action::SoftReset
        | Action::HardReset
        | Action::CaptureShader
        | Action::ToggleFullscreen
        | Action::SaveProject
        | Action::ToggleCommandPalette
        | Action::Quit
        | Action::BpmCommit
        | Action::BpmClear
        | Action::Prep(_) => {}
    }
    changed
}

/// A read-only listing of `map`, dimming entries whose source is already bound
/// in `covered_by` — those never fire, because a matching binding in the layer
/// above wins outright. Returns a source the user asked to mask.
pub fn readonly_map(
    ui: &mut egui::Ui,
    map: &ControlMap,
    covered_by: &[ControlSource],
) -> Option<ControlSource> {
    let p = palette();
    let mut mask = None;
    for binding in &map.bindings {
        let shadowed = covered_by.contains(&binding.source);
        ui.horizontal_wrapped(|ui| {
            let color = if shadowed { p.fg_muted } else { p.fg_secondary };
            ui.label(
                egui::RichText::new(source_key(&binding.source))
                    .monospace()
                    .color(color),
            );
            ui.label(egui::RichText::new(binding.action.label()).color(color));
            if shadowed {
                ui.label(egui::RichText::new("(shadowed)").color(p.fg_muted));
            } else if widgets::bracket_button(ui, "mask", None, 0.0)
                .on_hover_text("add a binding here that suppresses this one")
                .clicked()
            {
                mask = Some(binding.source.clone());
            }
        });
    }
    mask
}
