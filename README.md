# Reeve

An **operator harness**: an agent that runs on your computer and manages it for you. It isn't a
coding agent. It keeps the machine healthy, tidy, and configured. It gives receipts for everything
it does, and it learns how your system behaves.

> **Status: M2 (sysadmin).** Reeve reads, searches, and changes files anywhere on the machine,
> including root-owned ones. It manages packages (dnf5 on Fedora, pacman on Arch), systemd units, the
> journal, and processes, and it runs shell commands, `sudo` included: you approve the action, then type
> your password into Reeve. Every action is classified by risk and gets a hash-chained receipt. File,
> package, and service changes can be undone. Root changes get a snapper snapshot pair when snapper is
> set up for `/`. Each session writes a report. See [`design.md`](design.md) and [`DECISIONS.md`](DECISIONS.md).

## Quick start

```sh
cargo build --release
./target/release/reeve
```

Inside, type `/providers` (or press `^p`), choose a connection, and paste its key. The key is stored
in `~/.reeve/keys/<connection>` (mode 0600). It is never drawn, logged, or sent to a model. Reeve checks the
key with the provider; OpenRouter also shows how much of its limit is used. Then pick a model from the live
price list. You can add any OpenAI-compatible endpoint, or a local server, with `a` in the same panel.

From the shell, `reeve key set openrouter` does the same thing, and `OPENROUTER_API_KEY` / `OPENAI_API_KEY` also work.

Other commands:

- `reeve models [filter]` lists a connection's models with live prices, including cache read and write rates.
- `reeve spend` shows today and this month, across every Reeve session.
- `reeve receipts [list|show N|verify]` lists receipts, prints one in full, or checks the whole chain.
- `reeve undo N` reverses the action on receipt N and writes a receipt for the undo.

## Approvals

| tier | what | asks? |
| --- | --- | --- |
| **T0** observe | reads, listings, status commands | no |
| **T1** user change | files under your home or /tmp, user services, your processes | yes. `a` allows the same action for the rest of the session |
| **T2** system change | system files, packages, system services, anything with sudo | yes, every time |
| **T3** floor | formatting disks, partition tables, bootloader, `rm -rf` of top-level dirs, protected packages, secrets | you type `yes`, even in YOLO |

Reeve's own keys can't be read, and its receipts and undo store can't be written, by any tool, in any mode.

## Root

When an approved action needs root, Reeve asks for your password in a masked panel. The password goes to
sudo and nowhere else: not to the model, a log, or a receipt. You can let Reeve remember it in memory for
5 minutes (`tab` in the panel). The prompt only works while an approved root action is running.

- Root-owned files are changed in place by `sudo reeve root`, so owner, mode, and SELinux label are kept.
  Undo copies go to `/var/lib/reeve/undo` (root-only). Edits to `/etc/sudoers*` and `/etc/fstab` must pass
  `visudo -c` / `findmnt --verify` first.
- Package changes record their dnf transaction, so undo runs `dnf history undo`. Service changes record
  the unit's previous state.
- With snapper configured for `/`, each root action is wrapped in a pre/post snapshot pair. The receipt holds
  the numbers for `snapper undochange`. Reeve won't create a snapper config unless you ask.
- `reeve undo N` from a terminal asks for sudo there.

Each session keeps `~/.reeve/sessions/<id>/report.md`: what you asked, what was done, what can be undone,
and what it cost.

Configuration lives in `~/.reeve/config.toml` (see [`config.example.toml`](config.example.toml)). Reeve never
rewrites that file. Choices made in the TUI go to `~/.reeve/settings.toml`, which is layered on top.
Set `REEVE_HOME` to use a different state directory.

## Keys in the TUI

| key | does |
| --- | --- |
| `⏎` / `alt+⏎` | send / newline |
| `/` | commands: `/providers`, `/model`, `/receipts`, `/new`, `/yolo`, `/help`, `/quit` |
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
crates/reeve-observer  /proc + /sys sampler (becomes the `reeved` observer in M4)
crates/reeve-tui       mission-control UI
crates/reeve-cli       the `reeve` binary
```
