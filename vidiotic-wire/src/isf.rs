//! Wire views of vidiotic's ISF parameter model.
//!
//! The three ISF types are not mirrored here anymore: they carry
//! `SerJson`/`DeJson` at their home in `vidiotic-core::isf`, and this crate
//! re-exports them under the wire names so the protocol's JSON shapes are
//! exactly the derived forms. Only [`WireParam`] — a wire-only named pair — is
//! defined locally.

pub use vidiotic_core::isf::{
    IsfInput as WireIsfInput, IsfInputKind as WireIsfInputKind, IsfValue as WireIsfValue,
};

use nanoserde::{DeJson, SerJson};

/// A named ISF parameter override on a chain slot. The wire form of vidiotic's
/// `(Arc<str>, IsfValue)` pair.
#[derive(Clone, Debug, PartialEq, SerJson, DeJson)]
pub struct WireParam {
    /// The ISF input's uniform name.
    pub name: String,
    /// The overriding value.
    pub value: WireIsfValue,
}
