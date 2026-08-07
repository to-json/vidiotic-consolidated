//! The query half of the protocol: [`WireQuery`] names one slice of engine
//! state to fetch. Queries are selective so a client only pays the
//! serialization cost of what it asks for (the 512-bin spectrum is only in
//! `Levels`).

use nanoserde::{DeJson, SerJson};

/// One read-only slice of engine state a client can fetch. Each variant is
/// answered by the same-named [`crate::reply::WireReply`] variant.
///
/// Serializes as a bare JSON string, e.g. `"Status"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, SerJson, DeJson)]
pub enum WireQuery {
    /// Session identity: project path, epoch, wire version, mode flags.
    Status,
    /// Clock state: tempo, beat/phase, signature, cadences, sync source.
    Transport,
    /// Clip pool: clip banks, clips (with duration/fps), cameras.
    Pool,
    /// Cue banks and the edit bank's full cue views.
    Cues,
    /// The shader pool, including ISF input schemas.
    Shaders,
    /// Audio input devices and the current selection.
    Audio,
    /// Audio analysis: band levels, linear spectrum, overall level.
    Levels,
}

#[cfg(test)]
mod tests {
    use nanoserde::{DeJson, SerJson};

    use super::*;

    #[test]
    fn every_query_round_trips_and_is_a_bare_string() {
        let all = [
            WireQuery::Status,
            WireQuery::Transport,
            WireQuery::Pool,
            WireQuery::Cues,
            WireQuery::Shaders,
            WireQuery::Audio,
            WireQuery::Levels,
        ];
        for q in all {
            let json = q.serialize_json();
            assert!(json.starts_with('"') && json.ends_with('"'), "{json}");
            assert_eq!(WireQuery::deserialize_json(&json).unwrap(), q, "{json}");
        }
    }
}
