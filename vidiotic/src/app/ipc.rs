//! Draining the IPC engine and answering parked queries.

use super::*;

impl App {
    /// Drain a capped batch of IPC requests (step 1b). Commands validate
    /// against the current mirror then apply via [`Self::apply_command_inner`]
    /// — never the UI wrapper, so no save-picker pops for a scripted session;
    /// queries are parked for [`Self::answer_ipc_queries`]. Each request gets
    /// exactly one reply: `ack`, the query's `ok` payload, or `err`.
    pub(super) fn drain_ipc(&mut self) {
        let requests = match self.ipc.as_mut() {
            Some(ipc) => {
                ipc.pump();
                ipc.take_requests(crate::ipc::PER_TICK_CAP)
            }
            None => return,
        };
        for (conn, req) in requests {
            // Re-read per request: a `LoadProject`/`SetClipDir` earlier in this
            // same batch bumps the epoch, and a later request in the batch must
            // be checked against the current value, not a batch-start snapshot.
            if let Some(want) = req.epoch {
                let epoch = self.epoch.load(std::sync::atomic::Ordering::Relaxed);
                if want != epoch {
                    self.reply_ipc(
                        conn,
                        req.id,
                        ReplyResult::Err(format!("stale epoch {want} (session is at {epoch})")),
                    );
                    continue;
                }
            }
            match req.req {
                ReqBody::Get(query) => {
                    if let Some(ipc) = self.ipc.as_mut() {
                        ipc.park(conn, req.id, query);
                    }
                }
                ReqBody::Cmd(wire_cmd) => {
                    let cmd = crate::ipc::to_command(wire_cmd);
                    if let Some(reason) = crate::ipc::reject_reason(&cmd, &self.mirror) {
                        self.reply_ipc(conn, req.id, ReplyResult::Err(reason));
                    } else {
                        // `dispatch`, not `dispatch_ui`: an IPC command carries
                        // explicit paths and must never pop a save picker on
                        // the performer's screen with nobody driving.
                        self.dispatch(cmd);
                        self.reply_ipc(conn, req.id, ReplyResult::Ack);
                    }
                }
            }
        }
    }

    /// Answer the queries parked this tick from the freshly built mirror
    /// (step 8b).
    pub(super) fn answer_ipc_queries(&mut self) {
        let parked = match self.ipc.as_mut() {
            Some(ipc) => ipc.take_parked(),
            None => return,
        };
        let epoch = self.epoch.load(std::sync::atomic::Ordering::Relaxed);
        for (conn, id, query) in parked {
            let reply = crate::ipc::build_reply(&query, &self.mirror, epoch);
            self.reply_ipc(conn, id, ReplyResult::Ok(reply));
        }
    }

    /// Send one reply to an IPC connection, stamped with the current epoch.
    pub(super) fn reply_ipc(&mut self, conn: crate::ipc::ConnId, id: u64, result: ReplyResult) {
        let epoch = self.epoch.load(std::sync::atomic::Ordering::Relaxed);
        if let Some(ipc) = self.ipc.as_mut() {
            ipc.send(conn, &Reply { id, epoch, result });
        }
    }
}
