#!/usr/bin/env bash
# The local steps of the plan §23 full system test, with the scripted agent:
# §23.3 (scripted demo), §23.5 (rewind, fork, apply), the local half of §23.6
# (tamper) and of §23.7 (strict export and scan), and §23.8 (backtest).
# Each step checks its result; the script exits 1 if any check fails.
#
# Usage: scripts/system-test.sh OUT_DIR
#   OUT_DIR  new or empty directory for the workspace, data and logs
# Needs debug builds of wyrd, wyrd-fakeagent and wyrd-sensord in WYRD_BIN
# (default: ${CARGO_TARGET_DIR:-target}/debug), passwordless sudo for the
# sensor, and jq, zstd, tar, python3. `make system-test` builds and runs it.
# Do not set TMPDIR: the Cell writes only to the real /tmp.
set -uo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
out=${1:?usage: scripts/system-test.sh OUT_DIR}
bin=${WYRD_BIN:-${CARGO_TARGET_DIR:-$root/target}/debug}
w=$bin/wyrd
for f in "$w" "$bin/wyrd-fakeagent" "$bin/wyrd-sensord"; do
  [ -x "$f" ] || { echo "missing $f: build it first (make system-test does)" >&2; exit 2; }
done
for t in jq zstd tar python3; do
  command -v "$t" > /dev/null || { echo "missing $t" >&2; exit 2; }
done
mkdir -p "$out"
if [ -n "$(ls -A "$out")" ]; then echo "$out is not empty" >&2; exit 2; fi
out=$(cd "$out" && pwd)
mkdir -p "$out/ws" "$out/data" "$out/logs"
cp -a "$root/demo/victim-shop/." "$out/ws/"

pass=0
fail=0
check() { # name, then a command that must succeed
  local name=$1
  shift
  if "$@"; then
    pass=$((pass + 1))
    echo "PASS  $name"
  else
    fail=$((fail + 1))
    echo "FAIL  $name"
  fi
}
has() { grep -qF -- "$2" "$1"; }

# A fake key, built at run time, that must never leave the workspace.
key="shop_$(date +%s%N)$$"
echo "SHOP_API_KEY=$key" > "$out/ws/.env"

export WYRD_DATA_DIR=$out/data WYRD_PRIVILEGED_HELPER=sudo WYRD_SENSORD_SOCKET=$out/sensord.sock
export XDG_RUNTIME_DIR=${XDG_RUNTIME_DIR:-/run/user/$(id -u)}

# The sensor runs until its stdin closes (fd 9).
mkfifo "$out/hold"
# The redirects stay with this user on purpose: the log and the FIFO are ours.
# shellcheck disable=SC2024
sudo -n -- "$bin/wyrd-sensord" --exit-on-stdin-eof --socket "$WYRD_SENSORD_SOCKET" \
  < "$out/hold" > "$out/logs/sensord.log" 2>&1 &
exec 9> "$out/hold"
cleanup() {
  touch "$out/stop-asking"
  exec 9>&-
  wait 2> /dev/null
}
trap cleanup EXIT
for _ in $(seq 100); do [ -S "$WYRD_SENSORD_SOCKET" ] && break; sleep 0.1; done
check "sensor started" test -S "$WYRD_SENSORD_SOCKET"

"$w" policy build "$root/policies/default" --out "$out/default.wasm" > "$out/logs/policy.log" 2>&1
"$w" policy build "$root/policies/strict" --out "$out/strict.wasm" >> "$out/logs/policy.log" 2>&1

session_id() {
  find "$out/data/sessions" -mindepth 1 -maxdepth 1 -printf '%f\n' 2> /dev/null | head -1
}

# The default policy asks before the .env read and the curl; answer "allow",
# as in the demo.
allow() {
  while [ ! -e "$out/stop-asking" ]; do
    s=$(session_id)
    if [ -n "$s" ]; then
      for id in $("$w" ask "$s" --json 2> /dev/null | jq -r '.[]?.id' 2> /dev/null); do
        "$w" ask "$s" --allow "$id" > /dev/null 2>&1
      done
    fi
    sleep 0.2
  done
}
allow &

cd "$out/ws" || exit 2
script=$root/demo/scenarios/injection.wyrdscript.toml

echo "## 23.3 scripted demo"
"$w" run --agent fakeagent --policy "$out/default.wasm" -- "$bin/wyrd-fakeagent" run "$script" \
  > "$out/logs/run.log" 2>&1
check "the run exits 0" test $? = 0
sid=$(session_id)
echo "$sid" > "$out/sid"
"$w" events "$sid" --json > "$out/logs/run1-events.jsonl"
"$w" divergences "$sid" --json > "$out/logs/divergences.json"
check "exactly D001, D002, D003, D006" \
  test "$(jq -r '[.[].rule_id] | sort | join(",")' "$out/logs/divergences.json")" = "D001,D002,D003,D006"
ev=$out/logs/run1-events.jsonl
check "one denied connect to 203.0.113.50" test "$(jq -s '[.[] | select(.body.kind == "effect.net.connect"
  and .body.payload.addr == "203.0.113.50" and .body.payload.verdict == "denied")] | length' "$ev")" = 1
check "a checkpoint per finished tool call" \
  test "$(jq -s '[.[] | select(.body.kind == "intent.tool_call.finished")] | length' "$ev")" \
  = "$(jq -s '[.[] | select(.body.kind == "state.checkpoint.created")] | length' "$ev")"
check "no drops" test "$(jq -s '[.[].drops_since_last // 0] | add' "$ev")" = 0
"$w" verify "$sid" > "$out/logs/verify-open.log" 2>&1
check "the chain verifies" test $? = 0

echo "## 23.5 rewind, fork, apply"
"$w" rewind "$sid" c1 --relaunch > "$out/logs/rewind.log" 2>&1
check "rewind to c1 and relaunch exits 0" test $? = 0
check "the rewind warns about the denied connect" \
  has "$out/logs/rewind.log" "net.connect 203.0.113.50:80 (denied)"
"$w" fork "$sid" c1 --name side > "$out/logs/fork.log" 2>&1
check "the fork exits 0" test $? = 0
check "the fork warns about the relaunched run's connect" \
  has "$out/logs/fork.log" "net.connect 203.0.113.50:80 (denied)"
"$w" diff "$sid" a b > "$out/logs/diff.log" 2>&1
check "diff a b exits 0" test $? = 0
"$w" apply "$sid" b > "$out/logs/apply.log" 2>&1
check "apply b exits 0" test $? = 0
"$w" apply "$sid" b --dry-run --json > "$out/logs/apply-again.json" 2> /dev/null
check "a second apply finds no conflicts" test "$(jq '.conflicts | length' "$out/logs/apply-again.json")" = 0
upper=$out/data/sessions/$sid/branches/b/upper
same=0
differ=0
while IFS= read -r f; do
  if cmp -s "$upper/$f" "$out/ws/$f"; then same=$((same + 1)); else differ=$((differ + 1)); fi
done < <(cd "$upper" && find . -type f)
check "the workspace equals branch b ($same files)" test "$same" -gt 0 -a "$differ" = 0
touch "$out/stop-asking"
"$w" close "$sid" > "$out/logs/close.log" 2>&1
"$w" verify "$sid" > "$out/logs/verify-sealed.log" 2>&1
check "the sealed chain verifies" test $? = 0
check "the ledger is sealed" has "$out/logs/verify-sealed.log" "sealed    yes"
"$w" events "$sid" --json > "$out/logs/events.jsonl"

echo "## 23.6 tamper (local half)"
ledger=$out/data/sessions/$sid/ledger.wyrdledger
cp "$ledger" "$out/tampered.wyrdledger"
for x in -wal -shm; do [ -e "$ledger$x" ] && cp "$ledger$x" "$out/tampered.wyrdledger$x"; done
python3 -I - "$out/tampered.wyrdledger" 60 << 'PY'
import sqlite3, sys
db = sqlite3.connect(sys.argv[1])
seq = int(sys.argv[2])
# The copy only: the real ledger's trigger refuses this update.
for (name,) in db.execute("select name from sqlite_master where type = 'trigger'").fetchall():
    db.execute(f"drop trigger {name}")
row = bytearray(db.execute("select event_cbor from events where seq = ?", (seq,)).fetchone()[0])
row[len(row) // 2] ^= 1
db.execute("update events set event_cbor = ? where seq = ?", (bytes(row), seq))
db.commit()
PY
"$w" verify "$out/tampered.wyrdledger" > "$out/logs/verify-tampered.log" 2>&1
check "a flipped byte fails verify with exit 3" test $? = 3
check "verify names seq 60" has "$out/logs/verify-tampered.log" "event modified at seq 60"

echo "## 23.7 strict export and scan (local half)"
"$w" export "$sid" -o "$out/session.wyrd" --redact strict > "$out/logs/export.log" 2>&1
check "the strict export succeeds" test $? = 0
"$w" verify "$out/session.wyrd" > "$out/logs/verify-bundle.log" 2>&1
check "the bundle verifies" test $? = 0
mkdir -p "$out/import" "$out/unpacked"
WYRD_DATA_DIR=$out/import "$w" import "$out/session.wyrd" > "$out/logs/import.log" 2>&1
check "the bundle imports" test $? = 0
zstd -dqc "$out/session.wyrd" | tar -xf - -C "$out/unpacked"
hits() { grep -rlaF -- "$1" "$out/unpacked" | wc -l; }
check "the scan can see the bundle's contents" test "$(hits 203.0.113.50)" -gt 0
check "the planted key is not in the bundle" test "$(hits "$key")" = 0
check "the home path is not in the bundle" test "$(hits "$HOME")" = 0
check "the host name is not in the bundle" test "$(hits "$(hostname)")" = 0

echo "## 23.8 backtest"
"$w" policy backtest "$out/strict.wasm" "$sid" > "$out/logs/backtest-strict.log" 2>&1
check "strict: the connect is denied" has "$out/logs/backtest-strict.log" "deny connect to 203.0.113.50"
check "strict: the .env read is denied" has "$out/logs/backtest-strict.log" "deny reads an environment file"
"$w" policy backtest "$out/default.wasm" "$sid" > "$out/logs/backtest-default.log" 2>&1
check "default: the connect is denied" has "$out/logs/backtest-default.log" "deny connect to 203.0.113.50"
check "default: the .env read asks" has "$out/logs/backtest-default.log" "ask  reads an environment file"

echo "## result: $pass passed, $fail failed (logs in $out/logs)"
[ "$fail" = 0 ]
