#!/usr/bin/env bash
# Install and start wyrd-sensord as a systemd service, and give the runner
# user a systemd user manager (Cells run in delegated session cgroups).
# A sensor that does not come up fails the step with the doctor report,
# unless WYRD_ALLOW_DEGRADED=true: then the run goes ahead without kernel
# capture and every report says so.
#
# Env: WYRD_BIN_DIR (wyrd, wyrd-sensord), WYRD_SENSORD_UNIT (unit file),
#      WYRD_ALLOW_DEGRADED, WYRD_SENSORD_TIMEOUT (seconds, default 30).
set -euo pipefail

bin=${WYRD_BIN_DIR:?WYRD_BIN_DIR is required}
unit=${WYRD_SENSORD_UNIT:?WYRD_SENSORD_UNIT is required}
timeout=${WYRD_SENSORD_TIMEOUT:-30}
socket=${WYRD_SENSORD_SOCKET:-/run/wyrdline/sensord.sock}

output() { if [ -n "${GITHUB_OUTPUT:-}" ]; then echo "$1" >>"$GITHUB_OUTPUT"; fi; }
setenv() { if [ -n "${GITHUB_ENV:-}" ]; then echo "$1" >>"$GITHUB_ENV"; fi; }

degraded() {
  echo "::group::wyrd doctor"
  "$bin/wyrd" doctor || true
  echo "::endgroup::"
  echo "::group::wyrd-sensord service"
  sudo -n systemctl status --no-pager wyrd-sensord.service || true
  sudo -n journalctl -u wyrd-sensord.service -n 50 --no-pager || true
  echo "::endgroup::"
  if [ "${WYRD_ALLOW_DEGRADED:-false}" = true ]; then
    echo "::warning::wyrdline: $1; continuing WITHOUT kernel capture (allow-degraded: true)"
    output sensor=degraded
    exit 0
  fi
  echo "::error::wyrdline: $1. Not running the agent unprotected; set allow-degraded: true to run anyway."
  exit 1
}

sudo -n true 2>/dev/null || degraded "passwordless sudo is not available to install the sensor"

# A user manager for the runner user, so `systemd-run --user --scope` works.
uid=$(id -u)
if [ ! -S "/run/user/$uid/systemd/private" ]; then
  sudo -n loginctl enable-linger "$(id -un)" || true
  for _ in $(seq 1 50); do
    [ -S "/run/user/$uid/systemd/private" ] && break
    sleep 0.2
  done
fi
if [ -S "/run/user/$uid/systemd/private" ]; then
  setenv "XDG_RUNTIME_DIR=/run/user/$uid"
  setenv "DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/$uid/bus"
else
  echo "::warning::wyrdline: no systemd user manager; Cells fall back to cgroupfs"
fi

sudo -n install -m 0755 "$bin/wyrd-sensord" /usr/local/bin/wyrd-sensord ||
  degraded "could not install wyrd-sensord"
sudo -n install -m 0644 "$unit" /etc/systemd/system/wyrd-sensord.service ||
  degraded "could not install the wyrd-sensord unit"
sudo -n systemctl daemon-reload || degraded "systemctl daemon-reload failed"
sudo -n systemctl restart wyrd-sensord.service || degraded "wyrd-sensord did not start"

deadline=$((SECONDS + timeout))
until [ -S "$socket" ]; do
  if [ "$SECONDS" -ge "$deadline" ]; then degraded "wyrd-sensord did not open $socket within ${timeout}s"; fi
  if ! sudo -n systemctl is-active --quiet wyrd-sensord.service; then
    degraded "wyrd-sensord exited during start-up"
  fi
  sleep 0.2
done
echo "wyrdline: wyrd-sensord is running ($socket)"
output sensor=running
