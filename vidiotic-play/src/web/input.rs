//! Browser events → [`egui::RawInput`].
//!
//! This is the job `egui-winit` does natively, and the price of leaving winit
//! out of the browser build (see [`crate::gfx::HeadTarget`]). It is a small
//! price at this scale: the control panel is buttons, a slider and a list, so
//! pointer events and the wheel are the whole vocabulary.
//!
//! Keys take the other road. They do not become `egui::Event`s at all — they
//! become *canonical key names*, because their consumer is
//! [`crate::keymap`] rather than the panel, and the grammar is defined over
//! `vidiotic_ctl::keys` spellings. That module exists for exactly this: it is
//! deliberately winit- and egui-free and canonicalizes both toolkits' spellings
//! onto one name, so the browser is a third caller of an existing contract
//! instead of a third spelling of the same keys.
//!
//! Listeners are attached once and push into a shared queue; the render loop
//! drains it each frame. Nothing here touches the GPU or the engine, so the
//! borrow of the engine stays confined to the frame callback.

use std::cell::RefCell;
use std::rc::Rc;

use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

/// One key press, already canonicalized. Modifiers travel with it because the
/// grammar only claims chord-free presses — see `vidiotic::app::keys`.
pub struct KeyPress {
    pub canon: String,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub meta: bool,
    pub repeat: bool,
}

impl KeyPress {
    /// No modifier held that would make this a chord. Shift is not one: it is
    /// how you type half the canonical names.
    #[must_use]
    pub const fn plain(&self) -> bool {
        !self.ctrl && !self.alt && !self.meta
    }
}

/// Events accumulated since the last frame, shared between the listeners and
/// the render loop.
#[derive(Default)]
pub struct Queue {
    pub events: Vec<egui::Event>,
    /// Last known pointer position in points, for hover state.
    pub pointer: Option<egui::Pos2>,
    /// Key presses since the last frame, in order.
    pub keys: Vec<KeyPress>,
}

pub type Shared = Rc<RefCell<Queue>>;

/// Which mouse button a `MouseEvent::button()` code names.
fn button(code: i16) -> Option<egui::PointerButton> {
    match code {
        0 => Some(egui::PointerButton::Primary),
        1 => Some(egui::PointerButton::Middle),
        2 => Some(egui::PointerButton::Secondary),
        _ => None,
    }
}

fn modifiers(e: &web_sys::MouseEvent) -> egui::Modifiers {
    egui::Modifiers {
        alt: e.alt_key(),
        ctrl: e.ctrl_key(),
        shift: e.shift_key(),
        // On the web, `metaKey` is Command on macOS. egui's `command` is the
        // platform-appropriate one, which is what shortcuts should read.
        mac_cmd: e.meta_key(),
        command: e.meta_key() || e.ctrl_key(),
    }
}

/// Canvas-relative position in *points*.
///
/// `offsetX/Y` is already CSS-pixel-relative to the target element, and egui
/// works in points, so with `pixels_per_point` carried separately in the
/// `ScreenDescriptor` this needs no scaling — which is exactly why the canvas
/// is sized in device pixels and laid out in CSS pixels by the host page.
fn pos(e: &web_sys::MouseEvent) -> egui::Pos2 {
    egui::pos2(e.offset_x() as f32, e.offset_y() as f32)
}

/// Attach the pointer listeners to `canvas`. They live for the page's lifetime.
///
/// # Errors
/// If any `addEventListener` call fails.
pub fn attach(canvas: &web_sys::HtmlCanvasElement, q: &Shared) -> Result<(), JsValue> {
    // `move` closures that outlive this call have to be leaked deliberately;
    // `forget` is the documented way and is correct here because the listeners
    // are meant to last as long as the document does.
    macro_rules! on {
        ($name:literal, $ty:ty, |$ev:ident, $queue:ident| $body:block) => {{
            let shared = q.clone();
            let cb = Closure::<dyn FnMut(_)>::new(move |$ev: $ty| {
                let mut $queue = shared.borrow_mut();
                $body
            });
            canvas.add_event_listener_with_callback($name, cb.as_ref().unchecked_ref())?;
            cb.forget();
        }};
    }

    on!("pointermove", web_sys::PointerEvent, |e, q| {
        let p = pos(&e);
        q.pointer = Some(p);
        q.events.push(egui::Event::PointerMoved(p));
    });

    on!("pointerdown", web_sys::PointerEvent, |e, q| {
        if let Some(b) = button(e.button()) {
            let p = pos(&e);
            q.pointer = Some(p);
            q.events.push(egui::Event::PointerButton {
                pos: p,
                button: b,
                pressed: true,
                modifiers: modifiers(&e),
            });
        }
    });

    on!("pointerup", web_sys::PointerEvent, |e, q| {
        if let Some(b) = button(e.button()) {
            let p = pos(&e);
            q.events.push(egui::Event::PointerButton {
                pos: p,
                button: b,
                pressed: false,
                modifiers: modifiers(&e),
            });
        }
    });

    // Without this the pointer keeps its last in-canvas position forever and
    // whatever it was over stays lit after the cursor has left.
    on!("pointerleave", web_sys::PointerEvent, |_e, q| {
        q.pointer = None;
        q.events.push(egui::Event::PointerGone);
    });

    on!("wheel", web_sys::WheelEvent, |e, q| {
        e.prevent_default();
        q.events.push(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(-e.delta_x() as f32, -e.delta_y() as f32),
            // A `wheel` event carries no phase; egui documents `Move` as the
            // value to use when it is unknown.
            phase: egui::TouchPhase::Move,
            modifiers: egui::Modifiers::default(),
        });
    });

    // The canvas is a drawing surface, not a document: a right-click should
    // reach egui rather than open the browser's menu over the UI.
    on!("contextmenu", web_sys::Event, |e, _q| {
        e.prevent_default();
    });

    Ok(())
}

/// Canonicalize a browser `KeyboardEvent.key` into the `vidiotic_ctl::keys`
/// namespace.
///
/// The browser reports two shapes in one field — the literal character a key
/// produces (`"a"`, `";"`, `"["`) and a name for keys that produce none
/// (`"Escape"`, `"ArrowLeft"`, `"F1"`) — which is precisely the split
/// `vidiotic_ctl::keys` has two entry points for. Length tells them apart,
/// because every name is multi-character and every literal is one.
///
/// Space is the one key the browser spells differently from both toolkits: it
/// reports `" "` where winit and egui both say `Space`. Mapping it here rather
/// than widening the shared table keeps the browser's quirk in the browser's
/// adapter, which is where the other two toolkits keep theirs.
fn canon_key(key: &str) -> String {
    if key == " " {
        return "Space".to_owned();
    }
    if key.chars().count() == 1 {
        vidiotic_ctl::keys::from_character(key)
    } else {
        vidiotic_ctl::keys::from_named(key)
    }
}

/// Attach the key listener to `target` — the document, so keys work wherever
/// the focus sits rather than only over the canvas.
///
/// # Errors
/// If any `addEventListener` call fails.
pub fn attach_keys(target: &web_sys::EventTarget, q: &Shared) -> Result<(), JsValue> {
    let shared = q.clone();
    let cb = Closure::<dyn FnMut(_)>::new(move |e: web_sys::KeyboardEvent| {
        let canon = canon_key(&e.key());
        // The grammar's tokens are unmodified letters, so a plain press of one
        // must not also scroll the page or trigger a browser quick-find.
        if !e.ctrl_key() && !e.meta_key() && !e.alt_key() {
            e.prevent_default();
        }
        shared.borrow_mut().keys.push(KeyPress {
            canon,
            ctrl: e.ctrl_key(),
            alt: e.alt_key(),
            shift: e.shift_key(),
            meta: e.meta_key(),
            repeat: e.repeat(),
        });
    });
    target.add_event_listener_with_callback("keydown", cb.as_ref().unchecked_ref())?;
    cb.forget();
    Ok(())
}

/// Take the key presses accumulated since the last frame.
pub fn take_keys(q: &Shared) -> Vec<KeyPress> {
    std::mem::take(&mut q.borrow_mut().keys)
}

/// Drain a frame's worth of input.
pub fn take(q: &Shared, size_pts: egui::Vec2, time_sec: f64) -> egui::RawInput {
    let mut q = q.borrow_mut();
    egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size_pts)),
        time: Some(time_sec),
        events: std::mem::take(&mut q.events),
        focused: true,
        ..Default::default()
    }
}
