#!/bin/sh
set -eu

fail() { printf 'ligaturize: %s\n' "$*" >&2; exit 1; }

repo=${LIGATURIZE_REPO:-__REPOSITORY__}
version=${LIGATURIZE_VERSION:-latest}
dir=${LIGATURIZE_INSTALL_DIR:-"$HOME/.local/bin"}
case $repo in
    */*) ;;
    *) fail 'set LIGATURIZE_REPO=owner/repository, or use install.sh from a published release' ;;
esac
command -v curl >/dev/null 2>&1 || fail 'curl is required'
command -v tar >/dev/null 2>&1 || fail 'tar is required'
case $(uname -s) in
    Darwin) os=apple-darwin ;;
    Linux) os=unknown-linux-musl ;;
    *) fail 'supported platforms: macOS and Linux' ;;
esac
case $(uname -m) in
    arm64|aarch64) arch=aarch64 ;;
    x86_64|amd64) arch=x86_64 ;;
    *) fail 'supported architectures: ARM64 and x86-64' ;;
esac
archive="ligaturize-$arch-$os.tar.gz"
if [ "$version" = latest ]; then
    base="https://github.com/$repo/releases/latest/download"
else
    base="https://github.com/$repo/releases/download/$version"
fi
tmp=$(mktemp -d) || fail 'could not create temporary directory'
pending=
trap 'rm -rf "$tmp"; if [ -n "$pending" ]; then rm -f "$pending"; fi' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
printf 'Fetching ligaturize for %s…\n' "$arch-$os" >&2
curl -fLsS --proto '=https' --tlsv1.2 "$base/$archive" -o "$tmp/$archive"
curl -fLsS --proto '=https' --tlsv1.2 "$base/SHA256SUMS" -o "$tmp/SHA256SUMS"
expected=$(awk -v name="$archive" '$2 == name { print $1 }' "$tmp/SHA256SUMS")
[ "${#expected}" -eq 64 ] || fail 'release has no valid checksum for this platform'
if command -v sha256sum >/dev/null 2>&1; then
    actual=$(sha256sum "$tmp/$archive" | awk '{ print $1 }')
elif command -v shasum >/dev/null 2>&1; then
    actual=$(shasum -a 256 "$tmp/$archive" | awk '{ print $1 }')
else
    fail 'sha256sum or shasum is required'
fi
[ "$expected" = "$actual" ] || fail 'checksum mismatch; nothing was installed'
tar -xzf "$tmp/$archive" -C "$tmp" ligaturize
[ -f "$tmp/ligaturize" ] && [ ! -L "$tmp/ligaturize" ] || fail 'release does not contain a regular executable'
mkdir -p "$dir"
pending=$(mktemp "$dir/.ligaturize.XXXXXXXX")
cp "$tmp/ligaturize" "$pending"
chmod 755 "$pending"
mv -f "$pending" "$dir/ligaturize"
pending=
printf 'Installed %s/ligaturize\n' "$dir" >&2
case :$PATH: in
    *:"$dir":*) ;;
    *) printf 'Add %s to PATH, then run: ligaturize MyFont.ttf\n' "$dir" >&2 ;;
esac
