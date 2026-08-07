//! A minimal blocking client for the wire protocol, behind the `client`
//! feature. Sibling tools (`vidiotic-ctl`, `vidiotic-prep`) use this to drive a
//! running `vidiotic` without reimplementing the framing.
//!
//! It is deliberately simple: one request in flight at a time, ids assigned
//! automatically, replies read in order. That matches a script or a control
//! surface issuing commands one after another; a client that needs to pipeline
//! many outstanding requests should talk the protocol directly.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;

use crate::command::WireCommand;
use crate::envelope::{Greeting, Reply, ReplyResult, ReqBody, Request};
use crate::query::WireQuery;
use crate::reply::WireReply;

/// A client error: a transport failure, a malformed line, or a rejection from
/// the server.
#[derive(Debug)]
pub enum ClientError {
    /// Socket I/O failed, or the connection closed mid-exchange.
    Io(std::io::Error),
    /// A line from the server did not parse as the expected message.
    Protocol(String),
    /// The server rejected the request; carries its message.
    Rejected(String),
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "io: {e}"),
            Self::Protocol(m) => write!(f, "protocol: {m}"),
            Self::Rejected(m) => write!(f, "rejected: {m}"),
        }
    }
}

impl std::error::Error for ClientError {}

impl From<std::io::Error> for ClientError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// A blocking connection to a `vidiotic` IPC socket.
pub struct WireClient {
    stream: UnixStream,
    reader: BufReader<UnixStream>,
    next_id: u64,
    epoch: u64,
}

impl WireClient {
    /// Connect to the socket at `path`, consuming the greeting.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError::Io`] if the socket can't be reached and
    /// [`ClientError::Protocol`] if the first line isn't a valid greeting.
    pub fn connect(path: impl AsRef<Path>) -> Result<Self, ClientError> {
        let stream = UnixStream::connect(path)?;
        let mut reader = BufReader::new(stream.try_clone()?);
        let mut line = String::new();
        reader.read_line(&mut line)?;
        let greeting = Greeting::from_json_line(line.trim())
            .map_err(|e| ClientError::Protocol(format!("bad greeting: {e}")))?;
        Ok(Self { stream, reader, next_id: 1, epoch: greeting.vidiotic.epoch })
    }

    /// The session generation last seen (from the greeting or the most recent
    /// reply). A change means clip/cue ids from before may no longer be valid.
    #[must_use]
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Send a command and wait for its ack. Returns `Ok(())` when the engine
    /// accepted and dispatched it.
    ///
    /// # Errors
    ///
    /// [`ClientError::Rejected`] if the server rejected the command (bad id,
    /// out-of-range index, pathless save, stale epoch); [`ClientError::Io`] or
    /// [`ClientError::Protocol`] on transport/parse failure.
    pub fn command(&mut self, cmd: WireCommand) -> Result<(), ClientError> {
        match self.exchange(ReqBody::Cmd(cmd))? {
            ReplyResult::Ack => Ok(()),
            ReplyResult::Err(m) => Err(ClientError::Rejected(m)),
            ReplyResult::Ok(_) => {
                Err(ClientError::Protocol("command answered with a query payload".into()))
            }
        }
    }

    /// Send a query and wait for its answer.
    ///
    /// # Errors
    ///
    /// [`ClientError::Rejected`] if the server rejected the query;
    /// [`ClientError::Io`] or [`ClientError::Protocol`] on transport/parse
    /// failure.
    pub fn query(&mut self, query: WireQuery) -> Result<WireReply, ClientError> {
        match self.exchange(ReqBody::Get(query))? {
            ReplyResult::Ok(reply) => Ok(reply),
            ReplyResult::Err(m) => Err(ClientError::Rejected(m)),
            ReplyResult::Ack => {
                Err(ClientError::Protocol("query answered with a bare ack".into()))
            }
        }
    }

    /// Write one request and read replies until the matching id comes back
    /// (id-less push frames, a future addition, are skipped).
    fn exchange(&mut self, body: ReqBody) -> Result<ReplyResult, ClientError> {
        let id = self.next_id;
        self.next_id += 1;
        let req = Request { id, epoch: None, req: body };
        writeln!(self.stream, "{}", req.to_json_line())?;
        loop {
            let mut line = String::new();
            let n = self.reader.read_line(&mut line)?;
            if n == 0 {
                return Err(ClientError::Io(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "server closed the connection",
                )));
            }
            let reply = Reply::from_json_line(line.trim())
                .map_err(|e| ClientError::Protocol(format!("bad reply: {e}")))?;
            if reply.id != id {
                continue; // not ours (or a stale reply); keep reading
            }
            self.epoch = reply.epoch;
            return Ok(reply.result);
        }
    }
}
