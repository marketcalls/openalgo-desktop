#!/usr/bin/env bash
# The packages a .deb's Depends names, as apt installs them for it: the
# first package of each alternative, without version constraints or
# architecture qualifiers, separated by spaces.
#
#   scripts/ci/deb_depends.sh <file.deb>
#
# The native E2E and install smoke jobs install these from cached package
# files (.github/actions/apt-cached) before the .deb itself, so a slow
# Ubuntu mirror does not hold the job: the .deb's own name changes with
# every version, its Depends do not.
set -euo pipefail

deb="${1:?usage: deb_depends.sh <file.deb>}"
dpkg-deb -f "$deb" Depends \
  | tr ',' '\n' \
  | sed 's/|.*//; s/([^)]*)//g; s/:[^ ]*//; s/^ *//; s/ *$//' \
  | grep -v '^$' \
  | sort -u \
  | paste -sd ' ' -
