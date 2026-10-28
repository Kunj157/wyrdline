#!/usr/bin/env bash
# Packages built binaries into the release assets for one target:
#
#   wyrdline-<target>.tar.gz     wyrdline/bin/{wyrd,wyrdd,wyrd-sensord,wyrd-hub},
#                                wyrdline/share/wyrdline/studio (when built),
#                                deploy/ units, LICENSE, README
#   wyrdline_<version>_<arch>.deb  /usr/bin binaries, the wyrd-sensord systemd
#                                unit, the wyrd AppArmor profile, the Studio
#
# Both are reproducible: file order, owners, and timestamps are fixed
# (SOURCE_DATE_EPOCH, default 0).
#
# Usage: scripts/release/package.sh --version <v> --target <triple> --bins <dir> --out <dir> [--studio <dir>]
set -euo pipefail
cd "$(dirname "$0")/../.."

version=''
target=''
bins=''
out=''
studio=''
while [ $# -gt 0 ]; do
  case "$1" in
    --version) version=$2; shift 2 ;;
    --target) target=$2; shift 2 ;;
    --bins) bins=$2; shift 2 ;;
    --out) out=$2; shift 2 ;;
    --studio) studio=$2; shift 2 ;;
    *) echo "package: unknown argument $1" >&2; exit 2 ;;
  esac
done
if [ -z "$version" ] || [ -z "$target" ] || [ -z "$bins" ] || [ -z "$out" ]; then
  echo "usage: package.sh --version <v> --target <triple> --bins <dir> --out <dir> [--studio <dir>]" >&2
  exit 2
fi

case "$target" in
  x86_64-unknown-linux-gnu) deb_arch=amd64 ;;
  aarch64-unknown-linux-gnu) deb_arch=arm64 ;;
  *) echo "package: unsupported target $target" >&2; exit 2 ;;
esac
# Debian versions sort pre-releases with `~` (1.0.0~rc.1 < 1.0.0).
deb_version=${version//-/\~}
epoch=${SOURCE_DATE_EPOCH:-0}
binaries=(wyrd wyrdd wyrd-sensord wyrd-hub)
for b in "${binaries[@]}"; do
  [ -x "$bins/$b" ] || { echo "package: no executable $b in $bins" >&2; exit 1; }
done
if [ -n "$studio" ]; then
  [ -f "$studio/index.html" ] || { echo "package: $studio has no index.html" >&2; exit 1; }
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p "$out"

# Everything under $1 gets fixed ownership-neutral permissions and mtimes.
normalise() {
  find "$1" -type d -exec chmod 0755 {} +
  find "$1" -type f -exec chmod 0644 {} +
  find "$1" -exec touch -h -d "@$epoch" {} +
}

# ---- tarball ---------------------------------------------------------------
root="$work/tar/wyrdline"
mkdir -p "$root/bin" "$root/deploy/systemd" "$root/deploy/apparmor"
for b in "${binaries[@]}"; do install -m 0755 "$bins/$b" "$root/bin/$b"; done
cp deploy/systemd/wyrd-sensord.service "$root/deploy/systemd/"
cp deploy/apparmor/wyrd "$root/deploy/apparmor/"
cp LICENSE README.md "$root/"
if [ -n "$studio" ]; then
  mkdir -p "$root/share/wyrdline"
  cp -R "$studio" "$root/share/wyrdline/studio"
fi
normalise "$root"
chmod 0755 "$root"/bin/*
tarball="$out/wyrdline-$target.tar.gz"
tar --sort=name --mtime="@$epoch" --owner=0 --group=0 --numeric-owner \
  --pax-option=exthdr.name=%d/PaxHeaders/%f,delete=atime,delete=ctime \
  -C "$work/tar" -cf - wyrdline | gzip -n -9 >"$tarball"

# ---- .deb ------------------------------------------------------------------
deb="$work/deb"
mkdir -p "$deb/DEBIAN" "$deb/usr/bin" "$deb/usr/lib/systemd/system" "$deb/etc/apparmor.d" \
  "$deb/usr/share/doc/wyrdline"
for b in wyrd wyrdd wyrd-sensord; do install -m 0755 "$bins/$b" "$deb/usr/bin/$b"; done
# The unit and the profile ship with /usr/local/bin paths for `make install`;
# the package installs to /usr/bin.
sed 's#/usr/local/bin/#/usr/bin/#g' deploy/systemd/wyrd-sensord.service >"$deb/usr/lib/systemd/system/wyrd-sensord.service"
sed 's#/usr/local/bin/wyrd#/usr/bin/wyrd#g' deploy/apparmor/wyrd >"$deb/etc/apparmor.d/wyrd"
cp README.md "$deb/usr/share/doc/wyrdline/README.md"
{
  echo "Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/"
  echo "Upstream-Name: wyrdline"
  echo "Source: https://github.com/Kunj157/wyrdline"
  echo
  echo "Files: *"
  echo "Copyright: Kunj157 <kunjr157@gmail.com>"
  echo "License: MIT"
  sed 's/^/ /; s/^ $/ ./' LICENSE
} >"$deb/usr/share/doc/wyrdline/copyright"
if [ -n "$studio" ]; then
  mkdir -p "$deb/usr/share/wyrdline"
  cp -R "$studio" "$deb/usr/share/wyrdline/studio"
fi

installed_size=$(du -sk --apparent-size "$deb" --exclude=DEBIAN | cut -f1)
cat >"$deb/DEBIAN/control" <<EOF
Package: wyrdline
Version: $deb_version
Architecture: $deb_arch
Maintainer: Kunj157 <kunjr157@gmail.com>
Installed-Size: $installed_size
Depends: libc6
Recommends: apparmor
Section: utils
Priority: optional
Homepage: https://github.com/Kunj157/wyrdline
Description: Kernel-level flight recorder and sandbox for coding agents
 Wyrdline records what an AI coding agent intended and what actually
 happened on the host (eBPF), keeps it in a hash-chained ledger, and runs
 the agent in a rewindable Cell. This package installs the wyrd CLI, the
 per-user daemon wyrdd, the privileged sensor wyrd-sensord with its systemd
 unit (not enabled; see the README), and the AppArmor profile for wyrd.
EOF
echo "/etc/apparmor.d/wyrd" >"$deb/DEBIAN/conffiles"
cat >"$deb/DEBIAN/postinst" <<'EOF'
#!/bin/sh
set -e
if [ "$1" = configure ]; then
  if [ -d /run/systemd/system ]; then systemctl daemon-reload || true; fi
  if command -v apparmor_parser >/dev/null && [ -d /sys/kernel/security/apparmor ]; then
    apparmor_parser -r /etc/apparmor.d/wyrd || true
  fi
  echo "wyrdline: start the kernel sensor with: sudo systemctl enable --now wyrd-sensord"
fi
EOF
cat >"$deb/DEBIAN/prerm" <<'EOF'
#!/bin/sh
set -e
if [ "$1" = remove ] && [ -d /run/systemd/system ]; then
  systemctl stop wyrd-sensord.service 2>/dev/null || true
  systemctl disable wyrd-sensord.service 2>/dev/null || true
fi
EOF
cat >"$deb/DEBIAN/postrm" <<'EOF'
#!/bin/sh
set -e
if [ -d /run/systemd/system ]; then systemctl daemon-reload || true; fi
if [ "$1" = remove ] || [ "$1" = purge ]; then
  if command -v apparmor_parser >/dev/null && [ -d /sys/kernel/security/apparmor ]; then
    apparmor_parser -R /etc/apparmor.d/wyrd 2>/dev/null || true
  fi
fi
EOF
normalise "$deb"
chmod 0755 "$deb"/usr/bin/* "$deb"/DEBIAN/postinst "$deb"/DEBIAN/prerm "$deb"/DEBIAN/postrm
(cd "$deb" && find . -type f -not -path './DEBIAN/*' | LC_ALL=C sort | sed 's#^\./##' | xargs md5sum >DEBIAN/md5sums)
chmod 0644 "$deb/DEBIAN/md5sums"
touch -d "@$epoch" "$deb/DEBIAN/md5sums" "$deb/DEBIAN" "$deb"
debfile="$out/wyrdline_${deb_version}_${deb_arch}.deb"
SOURCE_DATE_EPOCH=$epoch dpkg-deb --build --root-owner-group -Zxz "$deb" "$debfile" >/dev/null
touch -d "@$epoch" "$tarball" "$debfile"

echo "package: $(basename "$tarball") $(stat -c %s "$tarball") bytes"
echo "package: $(basename "$debfile") $(stat -c %s "$debfile") bytes"
