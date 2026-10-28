#!/usr/bin/env bash
# Tests for the release packaging scripts (scripts/release/*.sh) with fake
# binaries. Run: bash tests/release/scripts.test.sh
set -uo pipefail

root=$(cd "$(dirname "$0")/../.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
pass=0
fail=0

ok() { pass=$((pass + 1)); echo "ok   $1"; }
bad() { fail=$((fail + 1)); echo "FAIL $1"; }
check() { if "$@" >/dev/null 2>&1; then ok "$*"; else bad "$*"; fi; }
# expect <0|1> <name> <command...>
expect() {
  local want=$1 name=$2
  shift 2
  local out="$tmp/out-$((pass + fail))"
  "$@" >"$out" 2>&1
  local got=$?
  if { [ "$want" = 0 ] && [ "$got" = 0 ]; } || { [ "$want" = 1 ] && [ "$got" != 0 ]; }; then
    ok "$name"
  else
    bad "$name (exit $got)"
    sed 's/^/     /' "$out"
  fi
  LAST_OUT=$out
}
contains() {
  if grep -q -- "$2" "$1"; then ok "↳ $3"; else bad "↳ $3: no '$2'"; sed 's/^/     /' "$1"; fi
}

# ---- fixtures --------------------------------------------------------------
mkdir -p "$tmp/bins" "$tmp/studio"
for b in wyrd wyrdd wyrd-sensord wyrd-hub; do
  printf '#!/bin/sh\necho "%s 1.2.3"\n' "$b" >"$tmp/bins/$b"
  chmod +x "$tmp/bins/$b"
done
echo '<!doctype html>' >"$tmp/studio/index.html"
package="$root/scripts/release/package.sh"
compare="$root/scripts/release/compare-builds.sh"
stamp="$root/scripts/release/stamp-version.sh"

# ---- package.sh ------------------------------------------------------------
expect 0 "package: tarball and .deb for x86_64" \
  env SOURCE_DATE_EPOCH=1700000000 "$package" --version 1.2.3-rc.1 --target x86_64-unknown-linux-gnu \
  --bins "$tmp/bins" --studio "$tmp/studio" --out "$tmp/a"
check test -f "$tmp/a/wyrdline-x86_64-unknown-linux-gnu.tar.gz"
check test -f "$tmp/a/wyrdline_1.2.3~rc.1_amd64.deb"

tar -tzf "$tmp/a/wyrdline-x86_64-unknown-linux-gnu.tar.gz" >"$tmp/tar.list"
for f in wyrdline/bin/wyrd wyrdline/bin/wyrdd wyrdline/bin/wyrd-sensord wyrdline/bin/wyrd-hub \
  wyrdline/share/wyrdline/studio/index.html wyrdline/deploy/systemd/wyrd-sensord.service \
  wyrdline/deploy/apparmor/wyrd wyrdline/LICENSE; do
  contains "$tmp/tar.list" "^$f\$" "tarball has $f"
done

dpkg-deb -c "$tmp/a/wyrdline_1.2.3~rc.1_amd64.deb" >"$tmp/deb.list"
for f in ./usr/bin/wyrd ./usr/bin/wyrdd ./usr/bin/wyrd-sensord ./usr/lib/systemd/system/wyrd-sensord.service \
  ./etc/apparmor.d/wyrd ./usr/share/wyrdline/studio/index.html ./usr/share/doc/wyrdline/copyright; do
  contains "$tmp/deb.list" " $f\$" ".deb has $f"
done
contains "$tmp/deb.list" "root/root" ".deb files are owned by root"
dpkg-deb -I "$tmp/a/wyrdline_1.2.3~rc.1_amd64.deb" >"$tmp/deb.info"
contains "$tmp/deb.info" "Version: 1.2.3~rc.1" ".deb version uses ~ for the pre-release"
contains "$tmp/deb.info" "Architecture: amd64" ".deb architecture"
contains "$tmp/deb.info" "conffiles" "AppArmor profile is a conffile"
dpkg-deb --fsys-tarfile "$tmp/a/wyrdline_1.2.3~rc.1_amd64.deb" | tar -xOf - ./usr/lib/systemd/system/wyrd-sensord.service >"$tmp/unit"
contains "$tmp/unit" "ExecStart=/usr/bin/wyrd-sensord" "unit points at /usr/bin"
dpkg-deb --fsys-tarfile "$tmp/a/wyrdline_1.2.3~rc.1_amd64.deb" | tar -xOf - ./etc/apparmor.d/wyrd >"$tmp/profile"
contains "$tmp/profile" "profile wyrd /usr/bin/wyrd" "AppArmor profile points at /usr/bin"
dpkg-deb -e "$tmp/a/wyrdline_1.2.3~rc.1_amd64.deb" "$tmp/control"
contains "$tmp/control/postinst" "daemon-reload" "postinst reloads systemd"
contains "$tmp/control/postinst" "enable --now wyrd-sensord" "postinst does not enable the sensor itself"
check test -x "$tmp/control/postinst"
check test -x "$tmp/control/prerm"
check test -x "$tmp/control/postrm"

expect 0 "package: aarch64 names the .deb arm64" \
  "$package" --version 1.2.3 --target aarch64-unknown-linux-gnu --bins "$tmp/bins" --out "$tmp/arm"
check test -f "$tmp/arm/wyrdline_1.2.3_arm64.deb"
check test -f "$tmp/arm/wyrdline-aarch64-unknown-linux-gnu.tar.gz"
tar -tzf "$tmp/arm/wyrdline-aarch64-unknown-linux-gnu.tar.gz" >"$tmp/arm.list"
if grep -q studio "$tmp/arm.list"; then bad "no Studio means no share/ dir"; else ok "no Studio means no share/ dir"; fi

expect 1 "package: refuses an unknown target" \
  "$package" --version 1.2.3 --target x86_64-apple-darwin --bins "$tmp/bins" --out "$tmp/x"
rm "$tmp/bins/wyrd-hub"
expect 1 "package: refuses a missing binary" \
  "$package" --version 1.2.3 --target x86_64-unknown-linux-gnu --bins "$tmp/bins" --out "$tmp/x"
contains "$LAST_OUT" "no executable wyrd-hub" "names the missing binary"
printf '#!/bin/sh\necho "wyrd-hub 1.2.3"\n' >"$tmp/bins/wyrd-hub"
chmod +x "$tmp/bins/wyrd-hub"

# ---- reproducibility of the packaging itself --------------------------------
expect 0 "package: second run with the same inputs" \
  env SOURCE_DATE_EPOCH=1700000000 "$package" --version 1.2.3-rc.1 --target x86_64-unknown-linux-gnu \
  --bins "$tmp/bins" --studio "$tmp/studio" --out "$tmp/b"
expect 0 "compare-builds: identical packages" "$compare" "$tmp/a" "$tmp/b" first second
contains "$LAST_OUT" "all 2 files identical" "summary"

# ---- compare-builds.sh -----------------------------------------------------
mkdir -p "$tmp/c1" "$tmp/c2"
echo one >"$tmp/c1/same"
echo one >"$tmp/c2/same"
echo x >"$tmp/c1/changed"
echo y >"$tmp/c2/changed"
echo z >"$tmp/c1/only-in-first"
expect 1 "compare-builds: differing builds fail" "$compare" "$tmp/c1" "$tmp/c2" build-1 build-2
contains "$LAST_OUT" "^same     same" "identical file reported"
contains "$LAST_OUT" "^DIFFERS  changed" "changed file reported"
contains "$LAST_OUT" "^MISSING  only-in-first" "missing file reported"
contains "$LAST_OUT" "2 of 3 files differ between build-1 and build-2: changed only-in-first" "summary lists the differing files"
expect 1 "compare-builds: empty directory is an error" "$compare" "$tmp/empty-none" "$tmp/c2"

# ---- stamp-version.sh ------------------------------------------------------
ws="$tmp/ws"
mkdir -p "$ws/crates/one/src"
cat >"$ws/Cargo.toml" <<'EOF'
[workspace]
resolver = "3"
members = ["crates/*"]

[workspace.package]
version = "0.0.0"
edition = "2024"
EOF
cat >"$ws/crates/one/Cargo.toml" <<'EOF'
[package]
name = "one"
version.workspace = true
edition.workspace = true
EOF
echo 'pub fn one() -> u8 { 1 }' >"$ws/crates/one/src/lib.rs"
(cd "$ws" && cargo generate-lockfile --offline --quiet)
expect 0 "stamp-version: sets the workspace version" "$stamp" 1.2.3-rc.1 "$ws"
contains "$ws/Cargo.toml" '^version = "1.2.3-rc.1"$' "Cargo.toml updated"
contains "$ws/Cargo.lock" '^version = "1.2.3-rc.1"$' "Cargo.lock updated"
expect 0 "stamp-version: idempotent" "$stamp" 1.2.3-rc.1 "$ws"
contains "$LAST_OUT" "already at" "says so"
expect 1 "stamp-version: rejects a non-version" "$stamp" latest "$ws"
check cargo metadata --locked --offline --format-version 1 --manifest-path "$ws/Cargo.toml"

echo "release scripts: $pass passed, $fail failed"
[ "$fail" = 0 ]
