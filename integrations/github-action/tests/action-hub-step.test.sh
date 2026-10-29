#!/usr/bin/env bash
# The "Upload the bundle to the Hub" step of action.yml, run as the runner
# runs it: its own `run:` block and `env:`, read from action.yml, with a stub
# hub-upload.sh behind $GITHUB_ACTION_PATH. Checks the wiring only (the
# script's own behaviour is hub-upload.test.sh).
# Run: bash integrations/github-action/tests/action-hub-step.test.sh
set -uo pipefail

here=$(cd "$(dirname "$0")" && pwd)
action_dir="$here/.."
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
pass=0
fail=0

# step.sh <name>: writes the step's run block and env (as JSON) to $tmp.
python3 - "$action_dir/action.yml" "$tmp" <<'EOF2'
import json, sys, yaml
action, out = sys.argv[1], sys.argv[2]
steps = yaml.safe_load(open(action))["runs"]["steps"]
step = next(s for s in steps if s.get("id") == "hub")
open(f"{out}/run.sh", "w").write(step["run"])
open(f"{out}/env.json", "w").write(json.dumps(step.get("env", {})))
open(f"{out}/if.txt", "w").write(step.get("if", ""))
EOF2

# The stub upload script: exit code and message from the environment.
mkdir -p "$tmp/action/scripts"
cat >"$tmp/action/scripts/hub-upload.sh" <<'EOF2'
#!/usr/bin/env bash
echo "stub upload ran for $WYRD_BUNDLE"
if [ -n "${STUB_FAIL:-}" ]; then
  echo "wyrdline hub upload: the Hub refused the token exchange (HTTP 403)" >&2
  exit 1
fi
EOF2
chmod +x "$tmp/action/scripts/hub-upload.sh"

# run_step <want 0|1> <name> [VAR=value ...]: runs the step with the env the
# runner would give it, the job's own values substituted from the arguments.
run_step() {
  local want=$1 name=$2
  shift 2
  local out="$tmp/out-$((pass + fail))"
  # Substitute ${{ inputs.* }} / ${{ steps.* }} from the env list, as Actions does.
  python3 - "$tmp" "$@" >"$tmp/step.env" <<'EOF2'
import json, sys
tmp, *pairs = sys.argv[1:]
env = json.load(open(f"{tmp}/env.json"))
values = dict(p.split("=", 1) for p in pairs)
for k, v in list(env.items()):
    expr = v.strip()
    if expr.startswith("${{") and expr.endswith("}}"):
        key = expr[3:-2].strip()
        env[k] = values.get(key, "")
for k, v in env.items():
    print(f"{k}={v}")
EOF2
  local step_env=()
  mapfile -t step_env <"$tmp/step.env"
  env -i PATH="/usr/bin:/bin" HOME="$HOME" GITHUB_ACTION_PATH="$tmp/action" \
    "${step_env[@]}" "$@" bash "$tmp/run.sh" >"$out" 2>&1
  local got=$?
  if { [ "$want" = 0 ] && [ "$got" = 0 ]; } || { [ "$want" = 1 ] && [ "$got" != 0 ]; }; then
    pass=$((pass + 1))
    echo "ok   $name"
  else
    fail=$((fail + 1))
    echo "FAIL $name (exit $got)"
    sed 's/^/     /' "$out"
  fi
  LAST_OUT=$out
}

contains() {
  if grep -q -- "$2" "$1"; then
    pass=$((pass + 1))
    echo "ok   ↳ $3"
  else
    fail=$((fail + 1))
    echo "FAIL ↳ $3: no '$2' in $1"
    sed 's/^/     /' "$1"
  fi
}

# The step runs only with a hub-url and a bundle (action.yml's if:).
if grep -q "inputs.hub-url != ''" "$tmp/if.txt" && grep -q "steps.run.outputs.bundle-path != ''" "$tmp/if.txt"; then
  pass=$((pass + 1)); echo "ok   the step is gated on hub-url and a bundle"
else
  fail=$((fail + 1)); echo "FAIL the step's if: lost its gate: $(cat "$tmp/if.txt")"
fi

# The step's environment: no token, no agent input, only the Hub settings.
if python3 - "$tmp/env.json" <<'EOF2'
import json, sys
keys = set(json.load(open(sys.argv[1])))
forbidden = {"github-token", "WYRD_COMMAND", "GITHUB_TOKEN"}
sys.exit(1 if keys & forbidden else 0)
EOF2
then
  pass=$((pass + 1)); echo "ok   no token or agent input reaches the upload step"
else
  fail=$((fail + 1)); echo "FAIL the upload step is given a token or the agent command"
fi

run_step 0 "success: the step passes" hub-url=https://hub.test hub-org=acme hub-upload-required=true steps.run.outputs.bundle-path=/tmp/x.wyrd inputs.hub-upload-required=true
contains "$LAST_OUT" "stub upload ran" "the script ran"

run_step 1 "a failed upload fails the step, with the script's message" STUB_FAIL=1 hub-upload-required=true inputs.hub-upload-required=true
contains "$LAST_OUT" "the Hub refused the token exchange" "the script's reason is shown"
contains "$LAST_OUT" "hub-upload-required: false" "the way to make it optional is named"

run_step 0 "hub-upload-required: false turns the failure into a warning" STUB_FAIL=1 inputs.hub-upload-required=false
contains "$LAST_OUT" "::warning::" "a warning annotation is emitted"

echo "action-hub-step.test.sh: $pass passed, $fail failed"
[ "$fail" = 0 ]
