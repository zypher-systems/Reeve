# Reeve

An **operator harness**: an agent that runs on your computer and manages it for you. It isn't a
coding agent. It keeps the machine healthy, tidy, and configured. It gives receipts for everything
it does, and it learns how your system behaves.

> **Status: M4 (observer).** Everything from M3 (files anywhere including root-owned ones, packages,
> services, logs, processes, shell with `sudo` through Reeve's own password prompt, risk tiers, receipts,
> undo, snapper pairs, session reports, memory), plus `reeved`. It's a background observer that watches
> the machine with rules (no model, no cost), learns what's normal, notifies you of findings, and can
> pre-draft fixes with its own budget if you turn that on. See [`design.md`](design.md) and
> [`DECISIONS.md`](DECISIONS.md).

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/zypher-systems/reeve/main/install.sh | sh
```

This installs `/usr/local/bin/reeve` (using sudo for the copy) and the `reeved` user service, starts the
observer, and checks every download against the release's `SHA256SUMS`. Options: `--user` (everything under
`~/.local`, no sudo), `--no-start`, `--version v0.1.0`, `--uninstall`. Your data in `~/.reeve` is never touched.

Packages:

- **Fedora:** `sudo dnf install ./reeve-<version>-1.x86_64.rpm` from the release, or build from source with
  `packaging/fedora/reeve.spec` (offline, with the vendored crates each release ships).
- **Arch / Omarchy:** the release's `PKGBUILD` (`reeve-bin`), or `packaging/arch/reeve` to build from source.

With a package, start the observer yourself: `systemctl --user enable --now reeved` (or `reeve daemon install`).

Then run `reeve`, type `/providers`, and paste your API key. `reeve doctor` checks the whole install.

From source:

```sh
cargo build --release --locked -p reeve-cli && ./target/release/reeve
```

## Keys in the TUI

| key | does |
| --- | --- |
| `⏎` / `alt+⏎` | send / newline |
| `/` | commands: `/providers`, `/model`, `/findings`, `/observer`, `/memory`, `/reflect`, `/receipts`, `/new`, `/yolo`, `/help`, `/quit` |
| `^p` / `^r` | `/providers` / `/receipts` (`u` undo, `v` verify) |
| `⏎` `a` `n` | on an approval card: approve, allow for session, deny |
| `esc` | stop the running turn, or clear the composer |
| `^y` | YOLO: auto-approve T0–T2 actions. The safeguard floor still asks. |
| `^b` | on narrow terminals, switch between the chat and the live rail |
| `pgup` / `pgdn`, mouse wheel | scroll |
| `^c` | stop, clear, then quit |

## Layout

```
crates/reeve-core      config, keys, providers, pricing, ledger, agent, policy (tiers), tools, receipts, undo
crates/reeve-observer  sampler, journal follower, baselines, detectors, notifications, drafter, the reeved loop
crates/reeve-tui       mission-control UI
crates/reeve-cli       the `reeve` binary
```
