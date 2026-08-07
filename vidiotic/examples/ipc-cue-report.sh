#!/usr/bin/env bash
# Print the edit bank's cues as a table: id, name, in/out, playing role.
# Demonstrates reading structured state back over IPC with jq.
#
# Usage: ./ipc-cue-report.sh
set -euo pipefail

SOCK="${VIDIOTIC_SOCK:-${TMPDIR:-/tmp}/vidiotic-latest.sock}"

if [ ! -S "$SOCK" ]; then
  echo "no vidiotic socket at $SOCK (is the app running? is IPC enabled?)" >&2
  exit 1
fi

# Greeting is line 1; the Cues reply is line 2.
reply="$(
  {
    printf '{"id":1,"req":{"get":"Cues"}}\n'
    sleep 0.2
  } | nc -U "$SOCK" | sed -n '2p'
)"

echo "$reply" | jq -r '
  .ok.Cues as $c
  | "live bank \($c.live_bank)  edit bank \($c.edit_bank)  (\($c.cues|length) cues)",
    (["id","name","in","out","role"] | @tsv),
    ( $c.cues[]
      | [ .id, .name, (.in_sec|tostring),
          (if .out_sec == null then "end" else (.out_sec|tostring) end),
          .role ]
      | @tsv )
' | column -t -s "$(printf '\t')"
