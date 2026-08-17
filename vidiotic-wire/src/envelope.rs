//! The newline-delimited JSON framing: one object per line, every request
//! answered by exactly one reply. Helpers parse/emit single lines without the
//! trailing `\n` — the transport does the framing.
//!
//! # Wire shapes
//!
//! These exact layouts are pinned by golden tests; they cannot drift silently.
//!
//! Request — `id` chosen by the client, `epoch` an optional session-generation
//! assertion, and a body that is either a command or a query:
//!
//! ```json
//! {"id":1,"epoch":3,"req":{"cmd":{"SetBpm":[128.0]}}}
//! {"id":2,"req":{"get":"Status"}}
//! ```
//!
//! Command payloads use nanoserde's derived enum layout: unit variants are
//! bare strings (`{"req":{"cmd":"TapTempo"}}`), tuple variants key an
//! argument array (`{"SetCueIn":[3,1.5]}`), struct variants key a field
//! object (`{"RelinkCamera":{"from":"a","to":"b"}}`).
//!
//! Reply — always carries the current `epoch`; exactly one of `ok` / `err`:
//!
//! ```json
//! {"id":2,"epoch":3,"ok":{"Status":{"project_path":"/a.viproj","epoch":3,"wire_version":1,"advanced":false,"grammar_on":false}}}
//! {"id":4,"epoch":3,"ack":true}
//! {"id":1,"epoch":3,"err":"unknown cue 9"}
//! ```
//!
//! A query answers with `ok`, a command with `ack`, a rejected request with
//! `err` — the three are mutually exclusive.
//!
//! (`Option` fields that are `None` — like an unsaved session's
//! `project_path` — are omitted from reply payloads, not written as `null`;
//! both absent and `null` parse back to `None`.)
//!
//! Greeting — the server's first line on connect:
//!
//! ```json
//! {"vidiotic":{"wire":1,"epoch":3}}
//! ```

use nanoserde::{DeJson, DeJsonErr, DeJsonState, SerJson, SerJsonState};

use crate::command::WireCommand;
use crate::query::WireQuery;
use crate::reply::WireReply;

/// The body of a [`Request`]: a state-changing command or a read-only query.
///
/// JSON shape (hand-written impls): `{"cmd":<WireCommand>}` or
/// `{"get":<WireQuery>}`.
#[derive(Clone, Debug, PartialEq)]
pub enum ReqBody {
    /// Apply a command; the reply acks (or rejects) it.
    Cmd(WireCommand),
    /// Fetch one slice of engine state.
    Get(WireQuery),
}

impl SerJson for ReqBody {
    fn ser_json(&self, d: usize, s: &mut SerJsonState) {
        s.out.push('{');
        match self {
            Self::Cmd(cmd) => {
                s.label("cmd");
                s.out.push(':');
                cmd.ser_json(d, s);
            }
            Self::Get(query) => {
                s.label("get");
                s.out.push(':');
                query.ser_json(d, s);
            }
        }
        s.out.push('}');
    }
}

impl DeJson for ReqBody {
    fn de_json(s: &mut DeJsonState, i: &mut core::str::Chars) -> Result<Self, DeJsonErr> {
        s.curly_open(i)?;
        // Copy the key out before advancing: `string()` + `colon()` step the
        // tokenizer onto the value, and a *string* value (a unit-variant
        // command or query) overwrites `strbuf`.
        let key = s.strbuf.clone();
        s.string(i)?;
        s.colon(i)?;
        let r = match key.as_ref() {
            "cmd" => Self::Cmd(DeJson::de_json(s, i)?),
            "get" => Self::Get(DeJson::de_json(s, i)?),
            _ => return Err(s.err_enum(&key)),
        };
        s.curly_close(i)?;
        Ok(r)
    }
}

/// One client request line. `id` is echoed on the reply so a pipelining
/// client can match answers; `epoch` optionally asserts the session
/// generation (the engine rejects the request if it no longer matches).
#[derive(Clone, Debug, PartialEq, SerJson, DeJson)]
pub struct Request {
    /// Client-chosen correlation id, echoed on the reply.
    pub id: u64,
    /// Optional session-generation assertion; omitted = don't care.
    pub epoch: Option<u64>,
    /// The command or query.
    pub req: ReqBody,
}

impl Request {
    /// Parse one request line (without its trailing newline).
    ///
    /// # Errors
    ///
    /// Returns the nanoserde parse error when `line` is not a well-formed
    /// request object (malformed JSON, missing `id`/`req`, an unknown body
    /// key, or a payload that doesn't type-check).
    pub fn from_json_line(line: &str) -> Result<Self, DeJsonErr> {
        Self::deserialize_json(line)
    }

    /// Serialize to one JSON line, without a trailing newline (the caller
    /// frames).
    #[must_use]
    pub fn to_json_line(&self) -> String {
        self.serialize_json()
    }
}

/// The outcome carried by a [`Reply`].
///
/// A query answers with [`Self::Ok`] carrying the requested state; a command
/// answers with [`Self::Ack`] (accepted and dispatched — *not* a success
/// guarantee: the engine applies many commands infallibly, so a client that
/// needs to confirm an effect queries for it). Either kind can answer with
/// [`Self::Err`] when the request is rejected before dispatch (bad id, index
/// out of range, pathless save, stale epoch).
#[derive(Clone, Debug, PartialEq)]
pub enum ReplyResult {
    /// A query succeeded; carries the requested state.
    Ok(WireReply),
    /// A command was accepted and dispatched. Serialized as `"ack":true`.
    Ack,
    /// The request was rejected before dispatch; the message is human-readable.
    Err(String),
}

/// One server reply line. Every request gets exactly one — command acks are
/// the script's barrier/flush primitive. `epoch` always carries the current
/// session generation.
///
/// JSON shape (hand-written impls): `{"id":..,"epoch":..,"ok":<WireReply>}`
/// on success, `{"id":..,"epoch":..,"err":"message"}` on rejection — the
/// result is flattened into the envelope, not nested under a `result` key.
#[derive(Clone, Debug, PartialEq)]
pub struct Reply {
    /// The request's correlation id, echoed back.
    pub id: u64,
    /// The current session generation.
    pub epoch: u64,
    /// Success payload or rejection message.
    pub result: ReplyResult,
}

impl SerJson for Reply {
    fn ser_json(&self, d: usize, s: &mut SerJsonState) {
        s.out.push('{');
        s.label("id");
        s.out.push(':');
        self.id.ser_json(d, s);
        s.out.push(',');
        s.label("epoch");
        s.out.push(':');
        self.epoch.ser_json(d, s);
        s.out.push(',');
        match &self.result {
            ReplyResult::Ok(reply) => {
                s.label("ok");
                s.out.push(':');
                reply.ser_json(d, s);
            }
            ReplyResult::Ack => {
                s.label("ack");
                s.out.push(':');
                true.ser_json(d, s);
            }
            ReplyResult::Err(msg) => {
                s.label("err");
                s.out.push(':');
                msg.ser_json(d, s);
            }
        }
        s.out.push('}');
    }
}

impl DeJson for Reply {
    fn de_json(s: &mut DeJsonState, i: &mut core::str::Chars) -> Result<Self, DeJsonErr> {
        let mut id = None;
        let mut epoch = None;
        let mut result = None;
        s.curly_open(i)?;
        while s.next_str().is_some() {
            match s.strbuf.as_ref() {
                "id" => {
                    s.next_colon(i)?;
                    id = Some(DeJson::de_json(s, i)?);
                }
                "epoch" => {
                    s.next_colon(i)?;
                    epoch = Some(DeJson::de_json(s, i)?);
                }
                "ok" => {
                    s.next_colon(i)?;
                    result = Some(ReplyResult::Ok(DeJson::de_json(s, i)?));
                }
                "ack" => {
                    s.next_colon(i)?;
                    let _: bool = DeJson::de_json(s, i)?;
                    result = Some(ReplyResult::Ack);
                }
                "err" => {
                    s.next_colon(i)?;
                    result = Some(ReplyResult::Err(DeJson::de_json(s, i)?));
                }
                _ => {
                    s.next_colon(i)?;
                    s.whole_field(i)?;
                }
            }
            s.eat_comma_curly(i)?;
        }
        s.curly_close(i)?;
        Ok(Self {
            id: id.ok_or_else(|| s.err_nf("id"))?,
            epoch: epoch.ok_or_else(|| s.err_nf("epoch"))?,
            result: result.ok_or_else(|| s.err_nf("ok"))?,
        })
    }
}

impl Reply {
    /// Serialize to one JSON line, without a trailing newline (the caller
    /// frames).
    #[must_use]
    pub fn to_json_line(&self) -> String {
        self.serialize_json()
    }

    /// Parse one reply line (without its trailing newline).
    ///
    /// # Errors
    ///
    /// Returns the nanoserde parse error when `line` is not a well-formed
    /// reply object (malformed JSON, missing `id`/`epoch`, or neither `ok`
    /// nor `err` present).
    pub fn from_json_line(line: &str) -> Result<Self, DeJsonErr> {
        Self::deserialize_json(line)
    }
}

/// The `"vidiotic"` object inside a [`Greeting`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, SerJson, DeJson)]
pub struct GreetingInfo {
    /// The protocol version the server speaks ([`crate::WIRE_VERSION`]).
    pub wire: u32,
    /// The current session generation.
    pub epoch: u64,
}

/// The server's first line on connect: `{"vidiotic":{"wire":1,"epoch":N}}`.
/// Identifies the socket as a vidiotic wire endpoint and hands the client the
/// protocol version and current epoch before any request is sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, SerJson, DeJson)]
pub struct Greeting {
    /// The endpoint identity and session generation.
    pub vidiotic: GreetingInfo,
}

impl Greeting {
    /// A greeting for the current [`crate::WIRE_VERSION`] at `epoch`.
    #[must_use]
    pub fn new(epoch: u64) -> Self {
        Self {
            vidiotic: GreetingInfo {
                wire: crate::WIRE_VERSION,
                epoch,
            },
        }
    }

    /// Serialize to one JSON line, without a trailing newline (the caller
    /// frames).
    #[must_use]
    pub fn to_json_line(&self) -> String {
        self.serialize_json()
    }

    /// Parse one greeting line (without its trailing newline).
    ///
    /// # Errors
    ///
    /// Returns the nanoserde parse error when `line` is not a well-formed
    /// greeting object.
    pub fn from_json_line(line: &str) -> Result<Self, DeJsonErr> {
        Self::deserialize_json(line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reply::WireStatus;

    #[test]
    fn request_golden_shapes() {
        let cmd = Request {
            id: 1,
            epoch: Some(3),
            req: ReqBody::Cmd(WireCommand::SetBpm(128.0)),
        };
        assert_eq!(
            cmd.to_json_line(),
            r#"{"id":1,"epoch":3,"req":{"cmd":{"SetBpm":[128.0]}}}"#
        );
        let get = Request {
            id: 2,
            epoch: None,
            req: ReqBody::Get(WireQuery::Status),
        };
        assert_eq!(get.to_json_line(), r#"{"id":2,"req":{"get":"Status"}}"#);
        let unit = Request {
            id: 3,
            epoch: None,
            req: ReqBody::Cmd(WireCommand::TapTempo),
        };
        assert_eq!(unit.to_json_line(), r#"{"id":3,"req":{"cmd":"TapTempo"}}"#);
        for r in [cmd, get, unit] {
            assert_eq!(Request::from_json_line(&r.to_json_line()).unwrap(), r);
        }
    }

    #[test]
    fn reply_golden_shapes() {
        let ok = Reply {
            id: 2,
            epoch: 3,
            result: ReplyResult::Ok(WireReply::Status(WireStatus {
                project_path: Some("/a.viproj".into()),
                epoch: 3,
                wire_version: crate::WIRE_VERSION,
                advanced: false,
                grammar_on: false,
            })),
        };
        assert_eq!(
            ok.to_json_line(),
            concat!(
                r#"{"id":2,"epoch":3,"ok":{"Status":{"project_path":"/a.viproj","#,
                r#""epoch":3,"wire_version":1,"advanced":false,"grammar_on":false}}}"#
            )
        );
        let ack = Reply {
            id: 4,
            epoch: 3,
            result: ReplyResult::Ack,
        };
        assert_eq!(ack.to_json_line(), r#"{"id":4,"epoch":3,"ack":true}"#);
        let err = Reply {
            id: 1,
            epoch: 3,
            result: ReplyResult::Err("unknown cue 9".into()),
        };
        assert_eq!(
            err.to_json_line(),
            r#"{"id":1,"epoch":3,"err":"unknown cue 9"}"#
        );
        for r in [ok, ack, err] {
            assert_eq!(Reply::from_json_line(&r.to_json_line()).unwrap(), r);
        }
    }

    #[test]
    fn status_project_path_none_is_omitted_and_parses_back() {
        let status = WireStatus {
            project_path: None,
            epoch: 0,
            wire_version: crate::WIRE_VERSION,
            advanced: false,
            grammar_on: true,
        };
        let json = nanoserde::SerJson::serialize_json(&status);
        assert_eq!(
            json,
            r#"{"epoch":0,"wire_version":1,"advanced":false,"grammar_on":true}"#
        );
        let back: WireStatus = nanoserde::DeJson::deserialize_json(&json).unwrap();
        assert_eq!(back, status);
    }

    #[test]
    fn greeting_golden_shape() {
        let greeting = Greeting::new(5);
        assert_eq!(
            greeting.to_json_line(),
            r#"{"vidiotic":{"wire":1,"epoch":5}}"#
        );
        assert_eq!(
            Greeting::from_json_line(&greeting.to_json_line()).unwrap(),
            greeting
        );
    }

    #[test]
    fn reply_parse_tolerates_whitespace_key_order_and_unknown_fields() {
        let line = r#" { "epoch" : 4 , "extra" : [1, {"x": null}] , "err" : "nope" , "id" : 9 } "#;
        let reply = Reply::from_json_line(line).unwrap();
        assert_eq!(
            reply,
            Reply {
                id: 9,
                epoch: 4,
                result: ReplyResult::Err("nope".into())
            }
        );
    }

    #[test]
    fn reply_missing_result_is_an_error() {
        assert!(Reply::from_json_line(r#"{"id":1,"epoch":0}"#).is_err());
        assert!(Reply::from_json_line(r#"{"epoch":0,"err":"x"}"#).is_err());
        assert!(Request::from_json_line(r#"{"id":1,"req":{"zap":1}}"#).is_err());
        assert!(Request::from_json_line("not json").is_err());
    }

    #[test]
    fn every_command_survives_the_request_envelope() {
        // The command catalog lives in `command::tests`; here it's enough to
        // push a representative of each payload category through the full
        // envelope: unit, tuple, struct-variant, and nested-aux payloads.
        let bodies = [
            ReqBody::Cmd(WireCommand::Quit),
            ReqBody::Cmd(WireCommand::SetCueOut(2, None)),
            ReqBody::Cmd(WireCommand::RelinkCamera {
                from: "a".into(),
                to: "b".into(),
            }),
            ReqBody::Cmd(WireCommand::SetCueParam(
                1,
                crate::command::WireCueParam::CamDelay(crate::command::WireCamDelay {
                    value: 0.5,
                    beats: false,
                    quantize: true,
                }),
            )),
            ReqBody::Get(WireQuery::Levels),
        ];
        for (id, body) in bodies.into_iter().enumerate() {
            let req = Request {
                id: id as u64,
                epoch: Some(1),
                req: body,
            };
            let line = req.to_json_line();
            assert!(
                !line.contains('\n'),
                "line framing must stay single-line: {line}"
            );
            assert_eq!(Request::from_json_line(&line).unwrap(), req, "{line}");
        }
    }
}
