//! Wire mirrors of vidiotic's ISF parameter model: runtime values
//! ([`WireIsfValue`]), input schemas ([`WireIsfInput`] / [`WireIsfInputKind`]),
//! and the named parameter pair ([`WireParam`]) used in effect chains.

use nanoserde::{DeJson, SerJson};

/// A runtime parameter value for an ISF input. Floats are `f32` to match the
/// GPU uniform; integers cover both `long` (dropdown) and `bool` (0/1).
///
/// Mirrors `vidiotic::isf::IsfValue`.
#[derive(Clone, Copy, Debug, PartialEq, SerJson, DeJson)]
pub enum WireIsfValue {
    /// A scalar float uniform.
    Float(f32),
    /// A boolean uniform (uploaded as 0/1).
    Bool(bool),
    /// A `long` (dropdown index) uniform.
    Long(i32),
    /// An RGBA color, each channel 0..1.
    Color([f32; 4]),
    /// A 2D point in the shader's coordinate convention.
    Point2D([f32; 2]),
}

/// The declared type + bounds of one ISF `INPUT`.
///
/// Mirrors `vidiotic::isf::IsfInputKind`.
#[derive(Clone, Debug, PartialEq, SerJson, DeJson)]
pub enum WireIsfInputKind {
    /// A float slider with bounds and a default.
    Float {
        /// Minimum slider value.
        min: f32,
        /// Maximum slider value.
        max: f32,
        /// Schema default.
        default: f32,
    },
    /// An on/off checkbox.
    Bool {
        /// Schema default.
        default: bool,
    },
    /// A dropdown of discrete values with display labels.
    Long {
        /// The selectable values, parallel to `labels`.
        values: Vec<i32>,
        /// Display labels, parallel to `values`.
        labels: Vec<String>,
        /// Schema default (one of `values`).
        default: i32,
    },
    /// An RGBA color picker.
    Color {
        /// Schema default color.
        default: [f32; 4],
    },
    /// A bounded 2D point.
    Point2D {
        /// Per-axis minimum.
        min: [f32; 2],
        /// Per-axis maximum.
        max: [f32; 2],
        /// Schema default point.
        default: [f32; 2],
    },
    /// A momentary trigger; treated as a bool in the uniform.
    Event,
    /// A generic image input; aliases to the effect's stage input.
    Image,
    /// An `audio` waveform input, bound from the app's audio waveform row.
    Audio,
    /// An `audioFFT` spectrum input, bound from the app's audio FFT row.
    AudioFft,
}

/// One declared ISF `INPUT`: its uniform name, optional display label, and
/// typed schema.
///
/// Mirrors `vidiotic::isf::IsfInput`.
#[derive(Clone, Debug, PartialEq, SerJson, DeJson)]
pub struct WireIsfInput {
    /// The uniform / JSON name the shader declares.
    pub name: String,
    /// Human-readable label, if the header provides one.
    pub label: Option<String>,
    /// The declared type and bounds.
    pub kind: WireIsfInputKind,
}

/// A named ISF parameter override on a chain slot. The wire form of vidiotic's
/// `(Arc<str>, IsfValue)` pair.
#[derive(Clone, Debug, PartialEq, SerJson, DeJson)]
pub struct WireParam {
    /// The ISF input's uniform name.
    pub name: String,
    /// The overriding value.
    pub value: WireIsfValue,
}
