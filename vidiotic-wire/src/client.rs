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
        Ok(Self {
            stream,
            reader,
            next_id: 1,
            epoch: greeting.vidiotic.epoch,
        })
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
            ReplyResult::Ok(_) => Err(ClientError::Protocol(
                "command answered with a query payload".into(),
            )),
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
            ReplyResult::Ack => Err(ClientError::Protocol(
                "query answered with a bare ack".into(),
            )),
        }
    }

    /// Write one request and read replies until the matching id comes back
    /// (id-less push frames, a future addition, are skipped).
    fn exchange(&mut self, body: ReqBody) -> Result<ReplyResult, ClientError> {
        let id = self.next_id;
        self.next_id += 1;
        let req = Request {
            id,
            epoch: None,
            req: body,
        };
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::{GreetingInfo, Reply};
    use std::io::BufRead;
    use std::os::unix::net::UnixListener;

    /// A stand-in server on a real `UnixStream`, so the framing under test is the
    /// framing that runs: a greeting, then one scripted reply per request read.
    ///
    /// The thread hands back the request lines it saw, so a test can assert what
    /// went out as well as what came back.
    struct FakeServer {
        path: std::path::PathBuf,
        join: Option<std::thread::JoinHandle<Vec<String>>>,
    }

    impl FakeServer {
        /// `greeting` is written first; then for each request line read, the next
        /// entry of `replies` is written back verbatim. Extra `replies` beyond the
        /// requests are written after the last one — which is how the
        /// skip-unmatched-id path gets exercised.
        fn start(greeting: String, replies: Vec<Vec<String>>) -> Self {
            // A path under the temp dir, unique per server in this process.
            // Unix socket paths are length-limited, so nothing elaborate — and a
            // counter rather than a name, because the tests run concurrently.
            static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "vidiotic-wire-test-{}-{n}.sock",
                std::process::id()
            ));
            let _ = std::fs::remove_file(&path);
            let listener = UnixListener::bind(&path).expect("bind");
            let join = std::thread::spawn(move || {
                let (stream, _) = listener.accept().expect("accept");
                let mut writer = stream.try_clone().expect("clone");
                writeln!(writer, "{greeting}").expect("greeting");
                let mut reader = BufReader::new(stream);
                let mut seen = Vec::new();
                for batch in replies {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        break;
                    }
                    seen.push(line.trim().to_string());
                    for reply in batch {
                        writeln!(writer, "{reply}").expect("reply");
                    }
                }
                seen
            });
            Self {
                path,
                join: Some(join),
            }
        }

        fn connect(&self) -> Result<WireClient, ClientError> {
            WireClient::connect(&self.path)
        }

        /// The request lines the server saw.
        fn finish(mut self) -> Vec<String> {
            self.join
                .take()
                .expect("joined once")
                .join()
                .expect("thread")
        }
    }

    impl Drop for FakeServer {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    fn greeting(epoch: u64) -> String {
        Greeting {
            vidiotic: GreetingInfo {
                wire: crate::WIRE_VERSION,
                epoch,
            },
        }
        .to_json_line()
    }

    fn ack(id: u64, epoch: u64) -> String {
        Reply {
            id,
            epoch,
            result: ReplyResult::Ack,
        }
        .to_json_line()
    }

    #[test]
    fn connect_reads_the_epoch_out_of_the_greeting() {
        let server = FakeServer::start(greeting(7), vec![]);
        let client = server.connect().expect("connect");
        assert_eq!(client.epoch(), 7);
    }

    #[test]
    fn a_first_line_that_is_not_a_greeting_is_a_protocol_error() {
        let server = FakeServer::start("hello?".to_string(), vec![]);
        match server.connect() {
            Err(ClientError::Protocol(m)) => assert!(m.contains("greeting"), "{m}"),
            Err(other) => panic!("expected a protocol error, got {other:?}"),
            Ok(_) => panic!("connected to something that is not a wire endpoint"),
        }
    }

    #[test]
    fn ids_increment_and_the_reply_updates_the_epoch() {
        let server = FakeServer::start(greeting(1), vec![vec![ack(1, 1)], vec![ack(2, 9)]]);
        let mut client = server.connect().expect("connect");
        client.command(WireCommand::TapTempo).expect("first");
        assert_eq!(client.epoch(), 1);
        client.command(WireCommand::TapTempo).expect("second");
        assert_eq!(client.epoch(), 9, "a reply's epoch is adopted");

        let seen = server.finish();
        assert_eq!(seen.len(), 2);
        assert!(seen[0].contains("\"id\":1"), "{}", seen[0]);
        assert!(seen[1].contains("\"id\":2"), "{}", seen[1]);
    }

    /// The documented skip: a reply whose id is not ours — a stale one, or a
    /// future id-less push frame — is read past rather than mistaken for the
    /// answer. Without this the client would return the wrong reply to every
    /// request after the first stray line.
    #[test]
    fn a_reply_with_another_id_is_skipped() {
        let server = FakeServer::start(
            greeting(1),
            // Two strays before ours, one of which is an *answer* shape rather
            // than an ack, so a client matching on shape instead of id fails too.
            vec![vec![
                ack(99, 1),
                Reply {
                    id: 98,
                    epoch: 1,
                    result: ReplyResult::Err("not yours".into()),
                }
                .to_json_line(),
                ack(1, 4),
            ]],
        );
        let mut client = server.connect().expect("connect");
        client.command(WireCommand::TapTempo).expect("ours acked");
        assert_eq!(client.epoch(), 4, "only our reply's epoch counts");
    }

    #[test]
    fn a_rejection_comes_back_as_rejected_with_its_message() {
        let server = FakeServer::start(
            greeting(1),
            vec![vec![Reply {
                id: 1,
                epoch: 1,
                result: ReplyResult::Err("no such cue".into()),
            }
            .to_json_line()]],
        );
        let mut client = server.connect().expect("connect");
        match client.command(WireCommand::TapTempo) {
            Err(ClientError::Rejected(m)) => assert_eq!(m, "no such cue"),
            other => panic!("expected a rejection, got {other:?}"),
        }
    }

    /// A command answered with a query payload (or a query answered with a bare
    /// ack) is the server disagreeing with the protocol, not a rejection.
    #[test]
    fn a_mismatched_reply_kind_is_a_protocol_error() {
        let server = FakeServer::start(greeting(1), vec![vec![ack(1, 1)]]);
        let mut client = server.connect().expect("connect");
        match client.query(WireQuery::Transport) {
            Err(ClientError::Protocol(m)) => assert!(m.contains("ack"), "{m}"),
            other => panic!("expected a protocol error, got {other:?}"),
        }
    }

    #[test]
    fn an_unparseable_reply_line_is_a_protocol_error() {
        let server = FakeServer::start(greeting(1), vec![vec!["{not json".to_string()]]);
        let mut client = server.connect().expect("connect");
        match client.command(WireCommand::TapTempo) {
            Err(ClientError::Protocol(m)) => assert!(m.contains("reply"), "{m}"),
            other => panic!("expected a protocol error, got {other:?}"),
        }
    }

    /// A server that hangs up mid-exchange is EOF, not a silent hang: the client
    /// blocks on `read_line`, and a zero-length read is the only signal it gets.
    #[test]
    fn a_closed_connection_mid_exchange_is_an_io_error() {
        let server = FakeServer::start(greeting(1), vec![vec![]]);
        let mut client = server.connect().expect("connect");
        match client.command(WireCommand::TapTempo) {
            Err(ClientError::Io(e)) => {
                assert_eq!(e.kind(), std::io::ErrorKind::UnexpectedEof, "{e}");
            }
            other => panic!("expected an io error, got {other:?}"),
        }
    }
}
