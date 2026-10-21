#!/usr/bin/env bash
# Tests for scripts/hub-upload.sh against a fake curl that records every
# call and answers like GitHub's token endpoint and the Hub.
# Run: bash integrations/github-action/tests/hub-upload.test.sh
set -uo pipefail

here=$(cd "$(dirname "$0")" && pwd)
upload_sh="$here/../scripts/hub-upload.sh"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
pass=0
fail=0

mkdir -p "$tmp/bin"
# The fake curl: parses the options the script uses, appends one line per
# call to calls.log (argv, then the config file and body it was given),
# writes the canned body to -o and prints the status for -w.
cat >"$tmp/bin/curl" <<'EOF'
#!/usr/bin/env bash
log=${FAKE_LOG:?}
out=""; cfg=""; data=""; url=""; forms=()
args=("$@")
while [ $# -gt 0 ]; do
  case "$1" in
    -o) out=$2; shift 2 ;;
    -w | --retry | --retry-delay | --max-time | -X | -H) shift 2 ;;
    -K) cfg=$2; shift 2 ;;
    --data-binary) data=${2#@}; shift 2 ;;
    -F) forms+=("$2"); shift 2 ;;
    -*) shift ;;
    *) url=$1; shift ;;
  esac
done
{
  echo "argv: ${args[*]}"
  [ -n "$cfg" ] && echo "config: $(cat "$cfg")"
  [ -n "$data" ] && echo "data: $(cat "$data")"
  for f in "${forms[@]}"; do echo "form: $f"; done
} >>"$log"
case "$url" in
  *audience=*)
    status=${FAKE_OIDC_STATUS:-200}
    printf '{"value":"%s"}' "${FAKE_JWT:-jwt.payload.sig}" >"$out" ;;
  */api/v1/ci/token)
    status=${FAKE_EXCHANGE_STATUS:-200}
    if [ "$status" = 200 ]; then
      printf '{"token":"wyrdci_%s","token_type":"Bearer","scope":"upload","expires_in":900}' "${FAKE_SECRET:-x}" >"$out"
    else
      printf '{"type":"ci-not-trusted","title":"CI token refused","status":%s}' "$status" >"$out"
    fi ;;
  */api/v1/orgs/*/bundles)
    status=${FAKE_UPLOAD_STATUS:-201}
    if [ "$status" = 201 ]; then
      printf '{"id":"01BUNDLE","url":"https://hub.test/bundles/01BUNDLE"}' >"$out"
    else
      printf '{"type":"too-large","title":"Bundle too large","status":%s,"detail":"the limit is 50 MB"}' "$status" >"$out"
    fi ;;
  *) status=404; : >"$out" ;;
esac
printf '%s' "$status"
EOF
chmod +x "$tmp/bin/curl"

printf 'bundle bytes' >"$tmp/session.wyrd"
secret=$(head -c 24 /dev/urandom | base64 | tr -d '/+=')

# expect <0|1> <name> [VAR=value ...]
expect() {
  local want=$1 name=$2
  shift 2
  local out="$tmp/out-$((pass + fail))"
  : >"$tmp/calls.log"
  : >"$tmp/gh-output"
  env -i PATH="$tmp/bin:/usr/bin:/bin" HOME="$HOME" FAKE_LOG="$tmp/calls.log" FAKE_SECRET="$secret" \
    GITHUB_OUTPUT="$tmp/gh-output" GITHUB_REPOSITORY=acme-gh/app \
    WYRD_HUB_URL=https://hub.test/ WYRD_HUB_ORG=acme WYRD_BUNDLE="$tmp/session.wyrd" \
    ACTIONS_ID_TOKEN_REQUEST_URL='https://token.test/get?api-version=2' ACTIONS_ID_TOKEN_REQUEST_TOKEN=runner-token \
    "$@" bash "$upload_sh" >"$out" 2>&1
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

lacks() {
  if grep -q -- "$2" "$1"; then
    fail=$((fail + 1))
    echo "FAIL ↳ $3: found '$2' in $1"
    sed 's/^/     /' "$1"
  else
    pass=$((pass + 1))
    echo "ok   ↳ $3"
  fi
}

expect 0 "happy path: OIDC token → exchange → upload"
contains "$tmp/calls.log" "https://token.test/get?api-version=2&audience=https%3A%2F%2Fhub.test" "audience is the Hub URL, appended to GitHub's query"
contains "$tmp/calls.log" 'config: header = "Authorization: bearer runner-token"' "runner token sent from a config file"
contains "$tmp/calls.log" '"token":"jwt.payload.sig"' "the OIDC token is posted to the Hub"
contains "$tmp/calls.log" '"org":"acme"' "with the organisation"
contains "$tmp/calls.log" "https://hub.test/api/v1/ci/token" "exchange endpoint"
contains "$tmp/calls.log" "config: header = \"Authorization: Bearer wyrdci_$secret\"" "upload token sent from a config file"
contains "$tmp/calls.log" "form: bundle=@$tmp/session.wyrd;type=application/octet-stream" "bundle file in the multipart body"
contains "$tmp/calls.log" "form: repo=acme-gh/app" "repository name alongside"
contains "$tmp/calls.log" "https://hub.test/api/v1/orgs/acme/bundles" "upload endpoint"
lacks "$tmp/calls.log" "argv:.*wyrdci_" "no token on a curl command line"
lacks "$tmp/calls.log" "argv:.*runner-token" "no runner token on a curl command line"
contains "$LAST_OUT" "::add-mask::wyrdci_$secret" "upload token masked in the log"
contains "$LAST_OUT" "::add-mask::jwt.payload.sig" "OIDC token masked in the log"
contains "$tmp/gh-output" "hub-bundle-id=01BUNDLE" "bundle id output"
contains "$tmp/gh-output" "hub-bundle-url=https://hub.test/bundles/01BUNDLE" "bundle url output"
contains "$LAST_OUT" "uploaded to https://hub.test" "success reported"

expect 1 "no id-token permission: refused before any request" ACTIONS_ID_TOKEN_REQUEST_URL=
contains "$LAST_OUT" "id-token: write" "the fix is named"
if [ ! -s "$tmp/calls.log" ]; then pass=$((pass + 1)); else fail=$((fail + 1)) && echo "FAIL requests were made"; fi

expect 1 "http:// hub: refused" WYRD_HUB_URL=http://hub.test
contains "$LAST_OUT" "must be https" "reason given"
expect 0 "http://localhost is fine for development" WYRD_HUB_URL=http://localhost:7730
expect 1 "missing hub-org: refused" WYRD_HUB_ORG=
expect 1 "bad hub-org: refused" WYRD_HUB_ORG='Acme Inc'
expect 1 "missing bundle: refused" WYRD_BUNDLE="$tmp/nowhere.wyrd"

expect 1 "GitHub refuses the OIDC request" FAKE_OIDC_STATUS=401
contains "$LAST_OUT" "HTTP 401" "status reported"

expect 1 "the Hub does not trust the repository" FAKE_EXCHANGE_STATUS=403
contains "$LAST_OUT" "HTTP 403: CI token refused" "the Hub's problem title is shown"
contains "$LAST_OUT" "is acme-gh/app trusted by organisation 'acme'" "the fix is named"
lacks "$tmp/calls.log" "/bundles" "no upload attempted"

expect 1 "the Hub cannot reach the issuer" FAKE_EXCHANGE_STATUS=503
contains "$LAST_OUT" "HTTP 503" "status reported"
contains "$LAST_OUT" "retried" "retries mentioned"

expect 1 "the Hub refuses the bundle" FAKE_UPLOAD_STATUS=413
contains "$LAST_OUT" "HTTP 413: Bundle too large: the limit is 50 MB" "title and detail shown"

echo "hub-upload.sh: $pass passed, $fail failed"
[ "$fail" = 0 ]
