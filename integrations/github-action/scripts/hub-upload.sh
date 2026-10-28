#!/usr/bin/env bash
# Upload the bundle to a Wyrdline Hub with the job's GitHub OIDC token,
# no stored secret (docs/architecture/hub.md, "CI upload tokens"):
#   1. ask the runner for an OIDC token whose audience is the Hub
#      (ACTIONS_ID_TOKEN_REQUEST_URL/TOKEN; needs `permissions: id-token: write`),
#   2. POST /api/v1/ci/token on the Hub → a short-lived upload-only token,
#   3. POST /api/v1/orgs/<org>/bundles (multipart) with it.
# Tokens travel in curl config files, never on a command line.
#
# Env: WYRD_HUB_URL, WYRD_HUB_ORG, WYRD_BUNDLE (required), WYRD_REPO
#      (default GITHUB_REPOSITORY). Outputs: hub-bundle-id (bundle.id) and
#      hub-bundle-url (the bundle's API resource, <hub>/api/v1/bundles/<id>).
set -euo pipefail

die() {
  echo "::error::wyrdline hub upload: $*" >&2
  exit 1
}

hub=${WYRD_HUB_URL:-}
hub=${hub%/}
[ -n "$hub" ] || die "hub-url is empty"
case "$hub" in
  https://*) ;;
  http://127.0.0.1 | http://127.0.0.1:* | http://localhost | http://localhost:*) ;;
  *) die "hub-url must be https:// (got '$hub')" ;;
esac
org=${WYRD_HUB_ORG:-}
case "$org" in
  '') die "hub-org is required with hub-url" ;;
  *[!a-z0-9-]* | -*) die "hub-org must be an organisation slug (got '$org')" ;;
esac
bundle=${WYRD_BUNDLE:-}
[ -n "$bundle" ] && [ -s "$bundle" ] || die "no bundle at '${bundle}'"
[ -n "${ACTIONS_ID_TOKEN_REQUEST_URL:-}" ] && [ -n "${ACTIONS_ID_TOKEN_REQUEST_TOKEN:-}" ] ||
  die "no OIDC token available: the job needs 'permissions: id-token: write'"
repo=${WYRD_REPO:-${GITHUB_REPOSITORY:-}}

work=$(mktemp -d)
chmod 700 "$work"
trap 'rm -rf "$work"' EXIT

output() { if [ -n "${GITHUB_OUTPUT:-}" ]; then echo "$1" >>"$GITHUB_OUTPUT"; fi; }

# request <name> <curl args…>: status in $status, body in $work/<name>.body.
# Transient failures (connection, 5xx, 429) are retried by curl.
request() {
  local name=$1
  shift
  status=$(curl -sS --retry 3 --retry-delay 2 --max-time 120 \
    -o "$work/$name.body" -w '%{http_code}' "$@") ||
    die "$name: cannot reach the server (curl exit $?)"
}

# "<title>: <detail>" of an RFC 9457 problem body, or the raw body.
problem() {
  jq -r '[.title, .detail] | map(select(. != null)) | join(": ")' "$work/$1.body" 2>/dev/null ||
    head -c 300 "$work/$1.body"
}

# 1. The job's OIDC token, for the Hub's audience.
case "$ACTIONS_ID_TOKEN_REQUEST_URL" in *\?*) sep='&' ;; *) sep='?' ;; esac
audience=$(jq -rn --arg a "$hub" '$a | @uri')
printf 'header = "Authorization: bearer %s"\n' "$ACTIONS_ID_TOKEN_REQUEST_TOKEN" >"$work/github.cfg"
request oidc -K "$work/github.cfg" -H 'accept: application/json' \
  "${ACTIONS_ID_TOKEN_REQUEST_URL}${sep}audience=${audience}"
[ "$status" = 200 ] || die "GitHub did not issue an OIDC token (HTTP $status)"
jwt=$(jq -r '.value // empty' "$work/oidc.body")
[ -n "$jwt" ] || die "GitHub's OIDC answer has no token"
echo "::add-mask::$jwt"

# 2. Exchange it for an upload token of the organisation.
jq -cn --arg token "$jwt" --arg org "$org" '{token: $token, org: $org}' >"$work/exchange.json"
request exchange -X POST -H 'content-type: application/json' -H 'accept: application/json' \
  --data-binary @"$work/exchange.json" "$hub/api/v1/ci/token"
case "$status" in
  200) ;;
  503) die "the Hub could not verify the token right now (HTTP 503: $(problem exchange)); retried 3 times" ;;
  *) die "the Hub refused the token exchange (HTTP $status: $(problem exchange)); is $repo trusted by organisation '$org'?" ;;
esac
token=$(jq -r '.token // empty' "$work/exchange.body")
[ -n "$token" ] || die "the Hub's exchange answer has no token"
echo "::add-mask::$token"

# 3. Upload the bundle.
printf 'header = "Authorization: Bearer %s"\n' "$token" >"$work/hub.cfg"
request upload -K "$work/hub.cfg" -H 'accept: application/json' \
  -F "bundle=@${bundle};type=application/octet-stream" -F "repo=${repo}" \
  "$hub/api/v1/orgs/$org/bundles"
case "$status" in
  200 | 201) ;;
  *) die "the Hub refused the bundle (HTTP $status: $(problem upload))" ;;
esac
# The answer is {"duplicate": bool, "bundle": {"id": ...}} for both a new
# upload (201) and a bundle the organisation already has (200). The Hub
# answers no URL; the bundle's API resource is the stable address.
id=$(jq -r '.bundle.id // empty' "$work/upload.body")
[ -n "$id" ] || die "the Hub's upload answer has no bundle id"
duplicate=$(jq -r '.duplicate // false' "$work/upload.body")
output "hub-bundle-id=$id"
output "hub-bundle-url=$hub/api/v1/bundles/$id"
if [ "$duplicate" = true ]; then
  echo "wyrdline: $bundle is already on $hub (organisation $org) as $id"
else
  echo "wyrdline: bundle uploaded to $hub (organisation $org) as $id"
fi
