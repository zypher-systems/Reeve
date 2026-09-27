#!/bin/sh
# Install Reeve (the `reeve` binary and the `reeved` observer service) from
# its GitHub releases.
#
#   curl -fsSL https://raw.githubusercontent.com/zypher-systems/reeve/main/install.sh | sh
#
# Options:
#   --user          install for you only: ~/.local/bin and ~/.local/share/systemd/user
#                   (default: /usr/local, using sudo to copy files)
#   --prefix DIR    install under DIR (bin/ and, for /usr/local, lib/systemd/user/)
#   --version TAG   a release like v0.1.0 (default: the latest)
#   --no-service    don't install the reeved unit
#   --no-start      install the unit, but don't enable or start it
#   --from FILE     install from a downloaded reeve-<target>.tar.gz (skips the download)
#   --uninstall     stop reeved and remove what this script installed (keeps ~/.reeve)
#
# Environment: REEVE_VERSION, REEVE_DOWNLOAD_URL (a mirror of the release files).
#
# Every download is checked against the release's SHA256SUMS before anything
# is installed. The default system install matters: `sudo reeve root` runs
# the binary as root to edit root-owned files, so it should live somewhere
# only root can write.

set -eu

REPO="zypher-systems/reeve"
MARK="Reeve observer"

say() { printf '%s\n' "$*"; }
warn() { printf 'reeve install: %s\n' "$*" >&2; }
die() { printf 'reeve install: %s\n' "$*" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || die "needs $1"; }

mode=system
prefix=""
version="${REEVE_VERSION:-}"
service=1
start=1
from=""
uninstall=0
while [ $# -gt 0 ]; do
    case "$1" in
        --user) mode=user ;;
        --prefix) prefix="${2:?--prefix needs a directory}"; shift ;;
        --version) version="${2:?--version needs a tag}"; shift ;;
        --no-service) service=0 ;;
        --no-start) start=0 ;;
        --from) from="${2:?--from needs a file}"; shift ;;
        --uninstall) uninstall=1 ;;
        -h | --help) sed -n '2,24p' "$0" 2>/dev/null | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) die "unknown option $1 (see --help)" ;;
    esac
    shift
done

[ "$(uname -s)" = Linux ] || die "Reeve manages Linux systems; this is $(uname -s)"

if [ -z "$prefix" ]; then
    if [ "$mode" = user ]; then prefix="$HOME/.local"; else prefix=/usr/local; fi
fi
bindir="$prefix/bin"
if [ "$mode" = user ]; then
    unitdir="${XDG_DATA_HOME:-$HOME/.local/share}/systemd/user"
elif [ "$prefix" = /usr/local ] || [ "$prefix" = /usr ]; then
    unitdir="$prefix/lib/systemd/user"
else
    unitdir=/etc/systemd/user
fi

# Run as root only for the copies that need it.
as_root() {
    if [ "$(id -u)" -eq 0 ]; then
        "$@"
    elif [ "$mode" = user ]; then
        "$@"
    else
        need sudo
        sudo "$@"
    fi
}

# systemctl --user must run as the person, not root.
user_systemd() {
    [ "$(id -u)" -ne 0 ] && systemctl --user show-environment >/dev/null 2>&1
}

if [ "$uninstall" -eq 1 ]; then
    if user_systemd; then
        systemctl --user disable --now reeved.service >/dev/null 2>&1 || true
    fi
    for d in "$unitdir" /usr/local/lib/systemd/user /etc/systemd/user "${XDG_DATA_HOME:-$HOME/.local/share}/systemd/user" "$HOME/.config/systemd/user"; do
        f="$d/reeved.service"
        if [ -f "$f" ] && grep -q "$MARK" "$f"; then
            case "$f" in "$HOME"/*) rm -f "$f" ;; *) as_root rm -f "$f" ;; esac
            say "removed $f"
        fi
    done
    if [ -f "$bindir/reeve" ]; then
        case "$bindir" in "$HOME"/*) rm -f "$bindir/reeve" ;; *) as_root rm -f "$bindir/reeve" ;; esac
        say "removed $bindir/reeve"
    fi
    user_systemd && systemctl --user daemon-reload || true
    say "Reeve is uninstalled. Your settings, keys, receipts, and memory are still in ~/.reeve; remove that folder to forget everything."
    exit 0
fi

need tar
need mkdir
need uname
case "$(uname -m)" in
    x86_64 | amd64) cpu=x86_64 ;;
    aarch64 | arm64) cpu=aarch64 ;;
    *) die "no prebuilt binary for $(uname -m) yet; build from source: cargo install --git https://github.com/$REPO reeve-cli" ;;
esac
target="$cpu-unknown-linux-musl"
asset="reeve-$target.tar.gz"

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT INT TERM

if [ -n "$from" ]; then
    [ -f "$from" ] || die "no such file $from"
    cp "$from" "$tmp/$asset"
    sums="$(dirname "$from")/SHA256SUMS"
    [ -f "$sums" ] || sums="$from.sha256"
    if [ -f "$sums" ]; then
        cp "$sums" "$tmp/SHA256SUMS"
    else
        warn "no SHA256SUMS next to $from; not verified"
    fi
else
    if command -v curl >/dev/null 2>&1; then
        fetch() { curl -fsSL "$1" -o "$2"; }
    elif command -v wget >/dev/null 2>&1; then
        fetch() { wget -q "$1" -O "$2"; }
    else
        die "needs curl or wget"
    fi
    if [ -n "${REEVE_DOWNLOAD_URL:-}" ]; then
        base="$REEVE_DOWNLOAD_URL"
    elif [ -n "$version" ]; then
        base="https://github.com/$REPO/releases/download/$version"
    else
        base="https://github.com/$REPO/releases/latest/download"
    fi
    say "downloading $asset ${version:-(latest)}"
    fetch "$base/$asset" "$tmp/$asset" || die "couldn't download $base/$asset"
    fetch "$base/SHA256SUMS" "$tmp/SHA256SUMS" || die "couldn't download the checksums; refusing to install unverified"
fi

if [ -f "$tmp/SHA256SUMS" ]; then
    expected=$(grep " \*\{0,1\}$asset\$" "$tmp/SHA256SUMS" | awk '{print $1}' | head -n 1)
    [ -n "$expected" ] || die "$asset isn't listed in SHA256SUMS"
    if command -v sha256sum >/dev/null 2>&1; then
        actual=$(sha256sum "$tmp/$asset" | awk '{print $1}')
    else
        need shasum
        actual=$(shasum -a 256 "$tmp/$asset" | awk '{print $1}')
    fi
    [ "$expected" = "$actual" ] || die "checksum mismatch for $asset: expected $expected, got $actual"
    say "checksum ok"
fi

tar -C "$tmp" -xzf "$tmp/$asset"
src="$tmp/reeve-$target"
[ -x "$src/reeve" ] || die "the archive has no reeve binary"

as_root mkdir -p "$bindir"
as_root install -m 755 "$src/reeve" "$bindir/reeve"
say "installed $bindir/reeve ($("$bindir/reeve" --version 2>/dev/null || echo '?'))"

if [ "$service" -eq 1 ]; then
    sed "s|^ExecStart=.*|ExecStart=$bindir/reeve daemon run|" "$src/reeved.service" > "$tmp/reeved.service"
    as_root mkdir -p "$unitdir"
    as_root install -m 644 "$tmp/reeved.service" "$unitdir/reeved.service"
    say "installed $unitdir/reeved.service"
    if [ "$start" -eq 1 ]; then
        if user_systemd; then
            systemctl --user daemon-reload
            if systemctl --user enable --now reeved.service >/dev/null 2>&1; then
                say "reeved is running (systemctl --user status reeved)"
            else
                warn "couldn't start reeved; check: systemctl --user status reeved"
            fi
        else
            say "start the observer as yourself (not root): systemctl --user enable --now reeved"
        fi
    fi
fi

case ":$PATH:" in
    *":$bindir:"*) ;;
    *) say "note: $bindir isn't on your PATH; add it, or run $bindir/reeve" ;;
esac
if [ "$mode" = user ]; then
    say "note: a --user install keeps the binary in your home. For root file edits, a system install (the default) is safer."
fi
say ""
say "Run \`reeve\`, then type /providers to add your API key. \`reeve doctor\` checks the install."
