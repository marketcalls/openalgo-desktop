#!/usr/bin/env bash
# Checks the dependency list of an OpenAlgo Desktop .deb (Debian/Ubuntu only).
#
#   scripts/ci/check_deb.sh <file.deb>
#
# tauri.conf.json leaves bundle.linux.deb.depends empty and Tauri adds the
# packages itself, so what a trader's apt installs is only known from the
# built package. This fails unless:
#   - Depends names WebKitGTK 4.1 and GTK 3 (libwebkit2gtk-4.1-0, libgtk-3-0);
#   - every shared library the packaged binary links against is provided by a
#     package that Depends pulls in (directly or through its dependencies),
#     so `apt install ./<file>.deb` leaves nothing the binary needs missing.
# It prints the control fields and the library-to-package map either way.
set -euo pipefail

deb="${1:?usage: check_deb.sh <file.deb>}"
binary_path="usr/bin/openalgo-desktop"

echo "== Control fields"
dpkg-deb -I "$deb"
depends=$(dpkg-deb -f "$deb" Depends)
echo "Depends: ${depends}"

fail=0
for pkg in libwebkit2gtk-4.1-0 libgtk-3-0; do
  if ! printf '%s\n' "$depends" | tr ',' '\n' | sed 's/^ *//; s/ .*//' | grep -qx "$pkg"; then
    echo "::error::The .deb does not depend on ${pkg}. Depends: ${depends}"
    fail=1
  fi
done

# The packages Depends installs, with everything they depend on in turn.
# Alternatives (a | b) count each side; version constraints are dropped.
direct=$(printf '%s\n' "$depends" | tr ',|' '\n' | sed 's/([^)]*)//g; s/^ *//; s/ *$//; s/:.*//' | grep -v '^$' | sort -u)
# shellcheck disable=SC2086 # one package name per word
closure=$(apt-cache depends --recurse --no-recommends --no-suggests --no-conflicts \
  --no-breaks --no-replaces --no-enhances $direct 2>/dev/null \
  | grep -v '^ ' | sed 's/:.*//; s/^<//; s/>$//' | sort -u)
if [ -z "$closure" ]; then
  echo "::error::apt-cache could not resolve the Depends of the .deb (is the apt index present?)."
  exit 1
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
dpkg-deb -x "$deb" "$work"
if [ ! -x "$work/$binary_path" ]; then
  echo "::error::The .deb has no executable ${binary_path}."
  exit 1
fi

echo "== Libraries the binary links against, and the package that provides each"
needed=$(readelf -d "$work/$binary_path" | sed -n 's/.*(NEEDED).*\[\(.*\)\]/\1/p')
for lib in $needed; do
  # Owning packages of any installed file with this exact name.
  owners=$(dpkg -S "/${lib}" 2>/dev/null | cut -d: -f1 | tr ', ' '\n' | grep -v '^$' | sort -u || true)
  if [ -z "$owners" ]; then
    echo "::error::${lib}: no installed package provides it on this machine, so the check cannot place it."
    fail=1
    continue
  fi
  covered=""
  for owner in $owners; do
    if printf '%s\n' "$closure" | grep -qx "$owner"; then
      covered="$owner"
      break
    fi
  done
  if [ -n "$covered" ]; then
    echo "  ${lib}: ${covered}"
  else
    echo "::error::${lib} comes from $(echo "$owners" | tr '\n' ' '), which the .deb's Depends does not pull in."
    fail=1
  fi
done

if [ "$fail" -ne 0 ]; then
  exit 1
fi
echo "OK: Depends covers every library the binary links against."
