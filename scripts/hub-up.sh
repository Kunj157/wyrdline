#!/usr/bin/env bash
# Start the local Hub (deploy/docker/compose.yaml) and wait until /readyz
# answers 200. On first use, writes throwaway credentials to
# deploy/docker/.env.hub (git-ignored, mode 0600); later runs reuse them, so
# the volumes keep working. `make hub-down` stops it and removes the volumes.
#
# Usage: scripts/hub-up.sh [up|down]
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
compose=$root/deploy/docker/compose.yaml
env_file=$root/deploy/docker/.env.hub
dc() { docker compose -f "$compose" --env-file "$env_file" "$@"; }

if [ "${1:-up}" = down ]; then
  [ -f "$env_file" ] || { echo "no local Hub ($env_file is missing)"; exit 0; }
  dc down --volumes --remove-orphans
  exit 0
fi

if [ ! -f "$env_file" ]; then
  (
    umask 077
    {
      echo "WYRD_HUB_DEV_PG_PASSWORD=$(openssl rand -hex 24)"
      echo "WYRD_HUB_DEV_S3_KEY=$(openssl rand -hex 12)"
      echo "WYRD_HUB_DEV_S3_SECRET=$(openssl rand -hex 24)"
    } > "$env_file"
  )
fi
port=$(sed -n 's/^WYRD_HUB_DEV_PORT=//p' "$env_file")
port=${port:-7730}

dc up --detach --build
for _ in $(seq 1 60); do
  if curl -fsS "http://127.0.0.1:$port/readyz" > /dev/null 2>&1; then
    echo "Hub ready at http://127.0.0.1:$port"
    exit 0
  fi
  sleep 3
done
echo "the Hub did not become ready; its log:" >&2
dc logs --tail 100 hub >&2
exit 1
