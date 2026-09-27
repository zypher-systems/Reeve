#!/bin/sh
# Package a built release binary: packaging/dist.sh <target> [out-dir]
#
# Expects target/<target>/release/reeve. Writes <out>/reeve-<target>.tar.gz
# (binary, user unit, installer, docs) and its .sha256. CI and local
# builds use the same script, so what's tested is what ships.
set -eu

target="${1:?usage: packaging/dist.sh <target> [out-dir]}"
out="${2:-dist}"
root=$(cd "$(dirname "$0")/.." && pwd)
bin="$root/target/$target/release/reeve"
[ -x "$bin" ] || { echo "dist: no binary at $bin (cargo build --release --locked -p reeve-cli --target $target)" >&2; exit 1; }

name="reeve-$target"
stage="$out/$name"
rm -rf "$stage"
mkdir -p "$stage"
cp "$bin" "$stage/reeve"
cp "$root/packaging/systemd/reeved.service" "$stage/reeved.service"
cp "$root/install.sh" "$root/LICENSE" "$root/README.md" "$root/config.example.toml" "$stage/"
chmod 755 "$stage/reeve" "$stage/install.sh"
tar -C "$out" -czf "$out/$name.tar.gz" "$name"
rm -rf "$stage"
cd "$out"
if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$name.tar.gz" > "$name.tar.gz.sha256"
else
    shasum -a 256 "$name.tar.gz" > "$name.tar.gz.sha256"
fi
echo "dist: $out/$name.tar.gz"
