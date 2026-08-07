#!/usr/bin/env bash
# Set the tempo over IPC, then read it back — proving read-your-writes with
# nothing but nc and jq.
#
# Usage: ./ipc-tap-tempo.sh [BPM]   (default 128)
set -euo pipefail

SOCK="${VIDIOTIC_SOCK:-${TMPDIR:-/tmp}/vidiotic-latest.sock}"
BPM="${1:-128}"

if [ ! -S "$SOCK" ]; then
  echo "no vidiotic socket at $SOCK (is the app running? is IPC enabled?)" >&2
  exit 1
fi

# Send: SetBpm(BPM) then get Transport. The server replies once per line, in
# order, preceded by the greeting — so we read 3 lines and inspect the last.
{
  printf '{"id":1,"req":{"cmd":{"SetBpm":[%s]}}}\n' "$BPM"
  printf '{"id":2,"req":{"get":"Transport"}}\n'
  sleep 0.2   # give the engine a tick to answer before nc closes the socket
} | nc -U "$SOCK" | {
  read -r greeting
  read -r ack
  read -r transport
  echo "greeting : $greeting"
  echo "ack      : $ack"
  echo "bpm now  : $(printf '%s' "$transport" | jq '.ok.Transport.bpm')"
}
