# Reeve

An **operator harness**: an agent that runs on your computer and manages it for you. It isn't a
coding agent. It keeps the machine healthy, tidy, and configured. It gives receipts for everything
it does, and it learns how your system behaves.

> **Status: M5 (standing orders).** Reeve manages this machine with you:
> - files anywhere, including root-owned ones
> - packages, services, logs, and processes
> - shell, with `sudo` through its own password prompt
>
> Every action has a risk tier, a receipt, and undo where possible. It remembers the machine, and `reeved`
> watches it with rules. Standing orders are the one way it acts on its own: work you wrote down, run when
> a finding or a schedule calls for it, inside a hard scope and budget. See [`design.md`](design.md) and
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

## Commands

- `reeve key set <connection>` stores an API key (the same as `/providers` in the TUI).
- `reeve models [filter]` lists a connection's models with live prices, including cache read and write rates.
- `reeve spend` shows today and this month, across every Reeve session.
- `reeve receipts [list|show N|verify]` lists receipts, prints one in full, or checks the whole chain.
- `reeve undo N` reverses the action on receipt N and writes a receipt for the undo.
- `reeve orders [list|show|run|sudoers|check|examples]` manages standing orders.
- `reeve daemon install|status|uninstall` manages the observer; `reeve doctor` checks the install.

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

## Memory

`~/.reeve/memory/{facts,runbooks,preferences,baselines}/*.md`: plain Markdown with a short header. Read,
edit, or delete any of it, by hand or in `/memory`.

- **Facts** come from a read-only survey on first run (refreshed weekly), from the model's `memory_write`,
  and from reflection. A fact you edit becomes yours, and reflection won't rewrite it.
- **Runbooks** are fixes, with how many times they worked and failed. Reeve searches them before
  diagnosing from scratch.
- **Preferences** are your rules. Ones Reeve proposes stay *pending* until you accept them (`a` in
  `/memory`). A preference can carry a rule, `deny-path: ~/.config/hypr/**` or `deny-command: docker restart*`,
  which the policy then enforces for every tool.
- **Reflection:** at `/new` (or `/reflect`), one model call reads the session and proposes memories. After a
  restart, recent sessions that were never reflected on are caught up. `[memory] auto_reflect = false` turns
  this off.
- **Baselines** arrive with the observer in M4.

## The observer (`reeved`)

```sh
reeve daemon install     # systemd user service, started now and at login
reeve daemon status      # running? what has it found?
reeve daemon uninstall
```

(or `i` / `u` in `/observer`). It samples every 5 s and follows the journal. It raises findings for:

- full or fast-filling disks
- swap nearly full, sustained memory pressure, heat, or load
- failed units (grouped, so one finding covers every instance of a crashing template)
- journal spikes against each unit's normal rate, and critical messages
- a newer kernel waiting for a reboot, and security updates

Findings notify the desktop (rate-limited) and wait in `/findings`. There, `d` asks Reeve to look into one,
`p` carries out a drafted fix, `a` marks it seen, and `x` dismisses it for good. Baselines of what's normal
appear in `/memory`'s Baselines tab.

The observer never changes the machine. The optional **drafter** pre-drafts a fix for each finding while
you're away. It's off by default, and when on it uses read-only tools and its own model and budget
(`/observer`, or `[observer.drafter]`). Its spend counts toward your global caps too.

Each session keeps `~/.reeve/sessions/<id>/report.md`: what you asked, what was done, what can be undone,
and what it cost.

Configuration lives in `~/.reeve/config.toml` (see [`config.example.toml`](config.example.toml)). Reeve never
rewrites that file. Choices made in the TUI go to `~/.reeve/settings.toml`, which is layered on top.
Set `REEVE_HOME` to use a different state directory.

## Standing orders

`~/.reeve/orders/*.toml`, managed in `/orders` or with `reeve orders`. `/orders` writes three examples the first
time, all off. An order has:

- **a task,** in plain words;
- **triggers:** finding ids (`disk-full:/var`, `unit-failed:*`) and/or a schedule (`daily 03:00`, `weekly sun 03:00`,
  `every 6h`);
- **a scope:** a tier cap (never T3), the tools that may change things, the command globs every part of a
  command must match (`*` stays within a word), and path globs for file tools. Reads are always fine.
- **a budget:** a cap per run, runs per day, and a cooldown.

`reeved` runs it when a finding or the schedule calls for it, once per occurrence of a finding. A yes inside
the scope is receipted as `order:<id>`. Anything outside it is refused, and the run ends as a proposal in
`/findings`. Nobody is there to type a password, so root commands need a sudoers rule: `reeve orders sudoers <id>`
prints exact ones, and never wildcards. `reeve orders run <id>` runs one now, exactly as `reeved` would. Order
files and Reeve's own config are floor-protected (T3), so the model can't give itself unattended powers.

## Keys in the TUI

| key | does |
| --- | --- |
| `⏎` / `alt+⏎` | send / newline |
| `/` | commands: `/providers`, `/model`, `/orders`, `/findings`, `/observer`, `/memory`, `/reflect`, `/receipts`, `/new`, `/yolo`, `/help`, `/quit` |
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
