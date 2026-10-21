#!/usr/bin/env bash
# Tests for scripts/sensord.sh with sudo faked: no system changes.
# Run: bash integrations/github-action/tests/sensord.test.sh
set -uo pipefail

here=$(cd "$(dirname "$0")" && pwd)
sensord_sh="$here/../scripts/sensord.sh"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
pass=0
fail=0

mkdir -p "$tmp/bin" "$tmp/wyrd"
# Fake sudo: logs every command; `systemctl restart` opens the socket unless
# FAKE_NO_SOCKET; any command containing $FAKE_FAIL fails.
cat >"$tmp/bin/sudo" <<'SH'
#!/bin/sh
[ "$1" = -n ] && shift
echo "$*" >>"$FAKE_LOG"
case " $* " in *" ${FAKE_FAIL:-@none@} "*) exit 1 ;; esac
case "$*" in
  "systemctl restart"*)
    [ -n "${FAKE_NO_SOCKET:-}" ] || python3 -c 'import socket,sys; socket.socket(socket.AF_UNIX).bind(sys.argv[1])' "$WYRD_SENSORD_SOCKET" ;;
  "systemctl is-active"*) [ -z "${FAKE_DEAD:-}" ] ;;
esac
SH
printf '#!/bin/sh\necho "doctor report: btf ok"\n' >"$tmp/wyrd/wyrd"
printf '#!/bin/sh\n' >"$tmp/wyrd/wyrd-sensord"
chmod +x "$tmp/bin/sudo" "$tmp/wyrd/"*

# expect <0|1> <name> [VAR=value ...]
expect() {
  local want=$1 name=$2
  shift 2
  rm -f "$tmp/sock" "$tmp/log" "$tmp/out" "$tmp/env"
  env PATH="$tmp/bin:$PATH" WYRD_BIN_DIR="$tmp/wyrd" WYRD_SENSORD_UNIT=/dev/null \
    WYRD_SENSORD_SOCKET="$tmp/sock" WYRD_SENSORD_TIMEOUT=2 FAKE_LOG="$tmp/log" \
    GITHUB_OUTPUT="$tmp/out" GITHUB_ENV="$tmp/env" "$@" bash "$sensord_sh" >"$tmp/stdout" 2>&1
  local got=$?
  if { [ "$want" = 0 ] && [ "$got" = 0 ]; } || { [ "$want" = 1 ] && [ "$got" != 0 ]; }; then
    pass=$((pass + 1))
    echo "ok   $name"
  else
    fail=$((fail + 1))
    echo "FAIL $name (exit $got)"
    sed 's/^/     /' "$tmp/stdout"
  fi
}

contains() {
  if [ -f "$1" ] && grep -q -- "$2" "$1"; then
    pass=$((pass + 1))
    echo "ok   ↳ $3"
  else
    fail=$((fail + 1))
    echo "FAIL ↳ $3: no '$2' in $1"
    [ -f "$1" ] && sed 's/^/     /' "$1"
  fi
}

expect 0 "sensor starts: service installed, socket up"
contains "$tmp/out" "sensor=running" "output sensor=running"
contains "$tmp/log" "install -m 0755 $tmp/wyrd/wyrd-sensord /usr/local/bin/wyrd-sensord" "binary installed"
contains "$tmp/log" "install -m 0644 /dev/null /etc/systemd/system/wyrd-sensord.service" "unit installed"
contains "$tmp/log" "systemctl restart wyrd-sensord.service" "service (re)started"

expect 1 "service fails to start: step fails with the doctor report" FAKE_FAIL=restart
contains "$tmp/stdout" "doctor report: btf ok" "doctor report printed"
contains "$tmp/stdout" "Not running the agent unprotected" "refusal explained"

expect 1 "service dies before the socket appears: step fails" FAKE_NO_SOCKET=1 FAKE_DEAD=1
contains "$tmp/stdout" "exited during start-up" "reason given"

expect 1 "socket never appears: step fails after the timeout" FAKE_NO_SOCKET=1
contains "$tmp/stdout" "within 2s" "timeout reported"

expect 0 "allow-degraded: warning, run goes ahead" FAKE_FAIL=restart WYRD_ALLOW_DEGRADED=true
contains "$tmp/out" "sensor=degraded" "output sensor=degraded"
contains "$tmp/stdout" "WITHOUT kernel capture" "warning says what is lost"

echo "sensord.sh: $pass passed, $fail failed"
[ "$fail" = 0 ]
