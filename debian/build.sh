#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-or-later
#
# debian/build.sh [OUT]: build the Debian package of the committed HEAD in a
# throwaway debian:forky container for this machine's architecture, into OUT
# (default ../omacam-deb). The arm64 package comes from GitHub Actions, which
# runs this on an arm64 machine; ARCH=arm64 on another machine emulates arm64
# with qemu-user, which is very slow. Uncommitted changes are not part of
# the build.
#
# The version is <workspace version>+git<commit date UTC, YYYYMMDD.HHMMSS>
# .g<commit>, so every later commit builds a higher version. The crates named
# in Cargo.lock are fetched from crates.io once (cache: OMACAM_CRATES,
# default ~/.cache/omacam-crates), checked against Cargo.lock and put into
# vendor/; cargo then builds without network access (debian/rules). The
# container fetches only the Debian build dependencies.
set -euo pipefail
cd "$(dirname "$0")/.."
ARCH=${ARCH:-$(case $(uname -m) in aarch64) echo arm64 ;; x86_64) echo amd64 ;; *) uname -m ;; esac)}
SUITE=${SUITE:-forky}
OUT=$(mkdir -p "${1:-../omacam-deb}" && cd "${1:-../omacam-deb}" && pwd)
CRATES=${OMACAM_CRATES:-$HOME/.cache/omacam-crates}
fail() { echo "build.sh: $*" >&2; exit 1; }

[ -z "$(git status --porcelain --untracked-files=no)" ] \
	|| echo "build.sh: uncommitted changes are left out, building HEAD" >&2
up=$(sed -n '/^\[workspace\.package\]/,/^\[/s/^version = "\(.*\)"$/\1/p' Cargo.toml)
[ -n "$up" ] || fail "no workspace version in Cargo.toml"
stamp=$(TZ=UTC git log -1 --date=format-local:%Y%m%d.%H%M%S --format=%cd HEAD)
rev=$(git rev-parse --short=7 HEAD)
V=$up+git$stamp.g$rev
echo "omacam $V for $ARCH"

W=$(mktemp -d)
trap 'rm -rf "$W"' EXIT
S=$W/omacam-$V
mkdir -p "$S"
git archive HEAD | tar -x -C "$S"

# The crates of Cargo.lock, each checked against its checksum there
if grep '^source = ' Cargo.lock | grep -qv '^source = "registry+https://github.com/rust-lang/crates.io-index"$'; then
	fail "Cargo.lock names sources other than crates.io"
fi
mkdir -p "$CRATES" "$S/vendor"
awk '/^\[\[package\]\]/ {if (n != "" && c != "") print n, v, c; n = v = c = ""}
	/^name = / {gsub(/"/, "", $3); n = $3} /^version = / {gsub(/"/, "", $3); v = $3}
	/^checksum = / {gsub(/"/, "", $3); c = $3} END {if (n != "" && c != "") print n, v, c}' \
	"$S/Cargo.lock" > "$W/crates"
while read -r n v c; do
	f=$CRATES/$n-$v.crate
	if ! echo "$c  $f" | sha256sum -c --status 2>/dev/null; then
		curl -fsSL --retry 3 -o "$f.part" "https://static.crates.io/crates/$n/$n-$v.crate" || fail "crate $n $v not loadable"
		mv "$f.part" "$f"
		echo "$c  $f" | sha256sum -c --status || fail "crate $n $v does not match Cargo.lock"
	fi
	mkdir -p "$S/vendor/$n-$v"
	tar -xzf "$f" -C "$S/vendor/$n-$v" --strip-components=1
	printf '{"files":{},"package":"%s"}\n' "$c" > "$S/vendor/$n-$v/.cargo-checksum.json"
done < "$W/crates"
echo "$(wc -l < "$W/crates") crates in vendor/"

# This build's changelog entry, dated by the commit
branch=$(git rev-parse --abbrev-ref HEAD)
{
	printf 'omacam (%s) %s; urgency=medium\n\n' "$V" "$SUITE"
	printf '  * Build of %s at %s.\n\n' "$branch" "$(git rev-parse HEAD)"
	printf ' -- %s  %s\n\n' "$(sed -n 's/^Maintainer: //p' debian/control)" "$(git log -1 --format=%cD HEAD)"
	cat debian/changelog
} > "$S/debian/changelog"

docker run --rm --platform "linux/$ARCH" -e V="$V" -e HUID="$(id -u)" -e HGID="$(id -g)" \
	-v "$W:/b" -v "$OUT:/out" "debian:$SUITE" sh -c '
set -eu
trap "chown -R \$HUID:\$HGID /out /b" EXIT
export DEBIAN_FRONTEND=noninteractive LC_ALL=C.UTF-8
apt-get update -qq > /tmp/apt.log 2>&1 || { tail -20 /tmp/apt.log; exit 1; }
apt-get install -y -qq --no-install-recommends build-essential dpkg-dev fakeroot ca-certificates xz-utils >> /tmp/apt.log 2>&1 \
	|| { tail -20 /tmp/apt.log; exit 1; }
cd /b/omacam-$V
apt-get build-dep -y --arch-only --no-install-recommends ./ >> /tmp/apt.log 2>&1 || { tail -40 /tmp/apt.log; exit 1; }
export DEB_BUILD_OPTIONS="nocheck noautodbgsym parallel=$(nproc)"
dpkg-buildpackage --build=source,any -us -uc > /b/build.log 2>&1 || { tail -80 /b/build.log; exit 1; }
cd /b
cp omacam_$V*.deb omacam_$V.dsc omacam_$V.tar.* omacam_$V*.buildinfo /out/
cp build.log /out/omacam_${V}_build.log'

cd "$OUT"
deb=omacam_${V}_$ARCH.deb
[ -f "$deb" ] || fail "no $deb"
dpkg-deb -x "$deb" "$W/deb"
grep -a -q 'libcamera\.so\.0\.7' "$W/deb/usr/bin/omacam" || fail "$deb: omacam without the libcamera backend"
sha256sum -- omacam_"$V"* > SHA256SUMS."$V"
cp SHA256SUMS."$V" SHA256SUMS
echo "version=$V" > VERSION
cat SHA256SUMS
