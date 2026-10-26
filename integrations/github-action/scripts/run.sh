#!/usr/bin/env bash
# Run the agent command under `wyrd run`, seal the session, export the
# bundle, and collect what the report step needs. Exits non-zero only when
# no session was recorded; the agent's own exit code is an output, so the
# bundle and the summary exist even when the agent fails.
#
# Env: WYRD_COMMAND (required), WYRD_AGENT, WYRD_POLICY, WYRD_NAME,
#      WYRD_ASK_TIMEOUT, WYRD_REDACT, WYRD_WORKSPACE, WYRD_APPLY,
#      WYRD_SENSOR (running|degraded), WYRD_OUT (output directory).
set -euo pipefail

: "${WYRD_COMMAND:?the command input is required}"
here=$(cd "$(dirname "$0")" && pwd)
out=${WYRD_OUT:-$(mktemp -d "${RUNNER_TEMP:-/tmp}/wyrdline.XXXXXX")}
redact=${WYRD_REDACT:-strict}
workspace=$(cd "${WYRD_WORKSPACE:-.}" && pwd)

case "$redact" in standard | strict) ;; *) echo "::error::redact must be standard or strict" && exit 1 ;; esac

output() { if [ -n "${GITHUB_OUTPUT:-}" ]; then echo "$1" >>"$GITHUB_OUTPUT"; fi; }

mkdir -p "$out"
export WYRD_DATA_DIR="$out/data"
# Ubuntu runners restrict unprivileged user namespaces (AppArmor); the
# runner has passwordless sudo, so stage two of the Cell goes through it
# and drops back to the runner user before the agent starts.
export WYRD_PRIVILEGED_HELPER=sudo

args=(run --ask-timeout "${WYRD_ASK_TIMEOUT:-5}")
if [ -n "${WYRD_NAME:-}" ]; then args+=(--name "$WYRD_NAME"); fi
if [ -n "${WYRD_AGENT:-}" ]; then args+=(--agent "$WYRD_AGENT"); fi
if [ -n "${WYRD_POLICY:-}" ]; then args+=(--policy "$(realpath "$WYRD_POLICY")"); fi
# The agent is the program wyrd starts (adapters add their hook settings
# to its argv); argv.mjs splits the input without expanding anything.
mapfile -d '' -t words < <(node "$here/argv.mjs")
[ "${#words[@]}" -gt 0 ] || exit 1
if [ -z "${WYRD_AGENT:-}" ]; then
  case "$(basename "${words[0]}")" in
    claude) args+=(--agent claude-code) ;;
    codex) args+=(--agent codex) ;;
    gemini) args+=(--agent gemini-cli) ;;
    wyrd-fakeagent) args+=(--agent fakeagent) ;;
  esac
fi
args+=(-- "${words[@]}")

echo "::group::wyrd run"
set +e
# stderr goes to the log and the console; stdout straight to the console.
{ (cd "$workspace" && wyrd "${args[@]}") 2>&1 1>&3 | tee "$out/stderr.log" >&2; } 3>&1
code=${PIPESTATUS[0]}
set -e
echo "::endgroup::"
output "agent-exit-code=$code"

sid=$(wyrd sessions --json | jq -r '.[0].id // empty')
if [ -z "$sid" ]; then
  echo "::error::wyrdline: wyrd run did not record a session (exit $code)"
  exit 1
fi
output "session-id=$sid"

# Capture problems the run reported (sensor lost, rules not enforced).
warnings="$out/warnings.txt"
: >"$warnings"
if [ "${WYRD_SENSOR:-running}" != running ]; then
  echo "kernel capture was off: wyrd-sensord did not start (allow-degraded)" >>"$warnings"
fi
sed -n 's/^wyrd: warning: //p' "$out/stderr.log" >>"$warnings"
output "warnings-file=$warnings"

if [ "${WYRD_APPLY:-false}" = true ]; then
  branch=$(wyrd sessions --json | jq -r --arg id "$sid" '.[] | select(.id == $id) | .current_branch')
  (cd "$workspace" && wyrd apply "$sid" "$branch")
fi

wyrd close "$sid" >/dev/null
problem=$(wyrd show "$sid" --json | jq -r '.verification.problem // empty')
if [ -n "$problem" ]; then echo "ledger verification: $problem" >>"$warnings"; fi
output "complete=$([ -s "$warnings" ] && echo false || echo true)"

wyrd divergences "$sid" --json >"$out/divergences.json"
output "divergences-json=$out/divergences.json"
output "divergence-count=$(jq '[.[] | select((.status // "open") == "open")] | length' "$out/divergences.json")"

bundle="$out/bundle/wyrdline-$sid.wyrd"
mkdir -p "$out/bundle"
wyrd export "$sid" -o "$bundle" --redact "$redact" --json >"$out/export.json"
output "bundle-path=$bundle"
echo "wyrdline: session $sid sealed; bundle $bundle ($(jq -r .bundle_bytes "$out/export.json") bytes, $redact redaction)"
