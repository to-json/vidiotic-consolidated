//! Identity and effect-chain types: how a cue names the clip it plays and the
//! stack of shaders it plays it through.
//!
//! These sit below the player's `Command` vocabulary rather than inside it. A
//! `.viproj` serializes them (see [`crate::project`]), the span editor edits
//! them, and the engine executes them — so they belong to the model, not to any
//! one front end's input contract.

use std::sync::Arc;

use crate::isf::IsfValue;

/// Identifies a source clip in the pool (its scan index).
pub type ClipId = u32;

/// A compiled shader pinned into the pool. A cue can reference one as an override.
pub type ShaderId = u32;

/// Which shader runs at one position in a cue's effect chain.
///
/// `Builtin` carries the effect's stable name — the persistable handle written
/// into `.viproj`. `Pinned` is a runtime-only pool id (livecoded captures have
/// no stable source, so they are not serialized). `Live` is the current
/// livecoded shader, so it can sit anywhere in the stack. `Isf` carries the ISF
/// shader's file path (project-relative or absolute) — a persistable handle the
/// pool compiles on demand.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SlotRef {
    Live,
    Builtin(Arc<str>),
    Pinned(ShaderId),
    Isf(Arc<str>),
}

/// One entry in a cue's effect chain. `params` holds per-slot ISF input
/// overrides (empty for non-ISF slots, or for ISF inputs left at their schema
/// default); an input value carries an `f32`, so this is `PartialEq` but not `Eq`.
#[derive(Clone, Debug, PartialEq)]
pub struct ChainSlot {
    pub shader: SlotRef,
    pub params: Vec<(Arc<str>, IsfValue)>,
}

/// A pool shader, as shown in the shader picker / cue editor. `builtin` entries
/// are bundled effects addressable by stable name (and persistable); non-builtin
/// entries are livecoded pins (runtime-only).
///
/// Inert data describing what a chain slot can point at, so it sits here beside
/// [`ChainSlot`] rather than in the player's `Command` vocabulary — which is
/// also what lets the renderer read it without depending on that vocabulary.
#[derive(Clone, Debug)]
pub struct ShaderPoolView {
    pub id: ShaderId,
    pub name: Arc<str>,
    pub builtin: bool,
    /// ISF input schema (min/max/default/labels) for the param editor; empty for
    /// non-ISF pool entries.
    pub inputs: Vec<crate::isf::IsfInput>,
}

impl ChainSlot {
    /// A slot referencing `shader` with default (no) parameters.
    pub fn new(shader: SlotRef) -> Self {
        Self { shader, params: Vec::new() }
    }

    /// The current value of an ISF input on this slot, if overridden.
    pub fn param(&self, name: &str) -> Option<&IsfValue> {
        self.params.iter().find(|(n, _)| n.as_ref() == name).map(|(_, v)| v)
    }

    /// Set (or replace) an ISF input override on this slot.
    pub fn set_param(&mut self, name: Arc<str>, value: IsfValue) {
        if let Some(slot) = self.params.iter_mut().find(|(n, _)| *n == name) {
            slot.1 = value;
        } else {
            self.params.push((name, value));
        }
    }
}
