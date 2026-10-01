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

Updating: when a newer release is out, the header shows `↑ <version>`. `reeve update` installs it the same way
Reeve was installed (the release's installer, dnf, or your AUR helper), checked against the release's
`SHA256SUMS` and with a receipt. `reeve update --rollback` goes back.

From source:

```sh
cargo build --release --locked -p reeve-cli && ./target/release/reeve
```

## Commands

- `reeve key set <connection>` stores an API key (the same as `/providers` in the TUI).
- `reeve models [filter]` lists a connection's models with live prices, including cache read and write rates. Models Reeve can't drive are hidden: ones without tool calling, and image, speech, embedding, and batch-only models. `--all` shows them, and so does `tab` in `/model`.
- `reeve report [--days N]` draws the state of the machine as a page and opens it (also `/report` in the TUI). See below.
- `reeve spend` shows today and this month, across every Reeve session.
- `reeve receipts [list|show N|verify]` lists receipts, prints one in full, or checks the whole chain.
- `reeve undo N` reverses the action on receipt N and writes a receipt for the undo.
- `reeve orders [list|show|run|sudoers|check|examples]` manages standing orders.
- `reeve daemon install|status|uninstall` manages the observer; `reeve doctor` checks the install.
- `reeve update [--check|--version vX.Y.Z|--rollback]` installs the newest release the way this one was installed.

## Approvals

| tier | what | asks? |
| --- | --- | --- |
| **T0** observe | reads, listings, status commands, and writes to Reeve's scratch folder | no |
| **T1** user change | files under your home, existing files in /tmp, user services, your processes | yes (see below for fewer asks) |
| **T2** system change | system files, packages, system services, anything with sudo | yes, every time |
| **T3** floor | formatting disks, partition tables, bootloader, `rm -rf` of top-level dirs, protected packages, secrets | you type `yes`, even in YOLO |

Reeve's own keys can't be read, and its receipts and undo store can't be written, by any tool, in any mode.

**Reading never asks.** Commands are read the way the shell will run them: `if`, `for … do … done`, `case`, functions,
and builtins like `cd` and `export` are structure, not programs; awk and sed programs that only print are reads (one
that writes a file, pipes to a command, or calls `system()` asks); `tool --help` and `tool --version` ask nothing.

**Scratch is free.** Each session has its own folder, `~/.reeve/scratch/<session>` (`$REEVE_SCRATCH` in every shell
command), for intermediate files; writing there, or creating a new file in `/tmp`, never asks. Folders are cleared
after a week. Unattended runs (the drafter, standing orders) still count these as writes.

**Fewer asks for a T1 change:**
- `a`: **yes to the rest of this request.** Every other user-level change until Reeve finishes answering you runs
  without asking. (Inside a verified change, `a` is yes to the rest of that change.)
- `s`: **this kind of change, all session.** It remembers what the action does, not its exact text: writes in
  `~/Documents`, deletes in `~/Downloads`, or runs `flatpak`. The card says which.
- `[approvals] undoable = true` (off by default): changes Reeve can undo (file writes, edits, moves, and deletes it
  keeps a copy of) run without asking, each with an undo in the chat. Shell commands, which can't be undone, still
  ask. The top bar shows `tiered · ↶ auto`.

## Verified changes

A fix states up front how Reeve will know it worked. For example: "bluetooth.service is active", "bluetooth.service logs no errors", "/ is under 90% full", or "`bluetoothctl show` prints `Powered: yes`". Reeve makes the changes, then runs those checks itself. If one fails, it undoes every change in the fix, newest first, and says so. The receipts read "verified" or "failed and rolled back", with a receipt for each undo.

The approval card shows when a step is part of a verified change, and whether that step can be rolled back. File edits, packages, and units can. Most raw shell commands can't, and the card says so.

## Root

When an approved action needs root, Reeve asks for your password in a masked panel. The password goes to
sudo and nowhere else: not to the model, a log, or a receipt. You can let Reeve remember it in memory for
5 minutes (`tab` in the panel). The prompt only works while an approved root action is running.

- Root-owned files are changed in place by `sudo reeve root`, so owner, mode, and SELinux label are kept.
  Undo copies go to `/var/lib/reeve/undo` (root-only). Edits to `/etc/sudoers*` and `/etc/fstab` must pass
  `visudo -c` / `findmnt --verify` first.
- Package changes can be undone. On Fedora the receipt records the dnf transaction, and undo runs
  `dnf history undo`. On Arch it records what pacman logged (installed, upgraded, removed, with versions),
  and undo puts the old versions back from pacman's cache, then removes what was new. If the cache was
  cleaned, undo says which version is missing and changes nothing. Service changes record the unit's
  previous state.
- On Arch, a package the repos don't have is built from the AUR with paru or yay. The approval card shows
  its PKGBUILD, and you're asked every time: YOLO and session yeses never cover it, and standing orders
  can't do it. A system upgrade (`pacman -Syu`) leaves AUR packages alone; they're listed with `aur:` and
  upgraded by name.
- With snapper configured for `/`, each root action is wrapped in a pre/post snapshot pair. The receipt holds
  the numbers for `snapper undochange`. With snap-pac, pacman takes that pair itself, and the receipt
  records snap-pac's. Reeve won't create a snapper config unless you ask.
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

Findings are reported, not announced: they wait in `/findings`, the header badge, and the agent's view,
with no popups. In `/findings`, `d` asks Reeve to look into one, `p` carries out a drafted fix, `a` marks it seen,
and `x` dismisses it for good. A popup means a proposed fix is ready: the drafter wrote one, or a standing order
stopped and needs you. (`[observer] notify_findings = true` brings back popups for findings themselves.)

The observer never changes the machine. The optional **drafter** pre-drafts a fix for each finding while
you're away. It's off by default, and when on it uses read-only tools and its own model and budget
(`/observer`, or `[observer.drafter]`). Its spend counts toward your global caps too.

Each session keeps `~/.reeve/sessions/<id>/report.md`: what you asked, what was done, what can be undone,
and what it cost.

Configuration lives in `~/.reeve/config.toml` (see [`config.example.toml`](config.example.toml)). Reeve never
rewrites that file. Choices made in the TUI go to `~/.reeve/settings.toml`, which is layered on top.
Set `REEVE_HOME` to use a different state directory.

## Standing orders

Work Reeve does on its own, inside limits you set. The easiest way to make one is to ask:

> clear my thumbnail cache every Sunday if it's over 500 MB

Reeve looks first, then writes the order and asks you once, with the order in plain words on the approval
card: when it runs, what it does, the exact commands and files it may change, and what it may spend a run
and a day. `⏎` approves it and it's on; `n` says no. Nothing answers that card for you: not YOLO, not "yes to
the rest", not a session yes. Ask Reeve to change, pause, or delete an order the same way.

Or build one yourself: open **F7 orders** and press `n`, and a form walks you through
one, section by section, with each field explained and a live **What it will do** beside it in plain words.
You can start blank or from an example, pick a schedule from choices instead of writing one, tick what
reeved finds that should start a run, and choose what it may change. `ctrl+s` saves it (off, until you turn
it on with `space`), and the form points to anything missing first. `e` edits an order in the same form;
`E` opens its file in `$EDITOR`.

Nothing you do to an order is lost:

- **Every change can be undone.** Saving, turning on or off, deleting, and editing in `$EDITOR` each leave a
  receipt with the file as it was. `u` in F7 undoes the last one, and F4 activity has them all.
- **Saving changes only what you changed.** Comments, keys Reeve doesn't know, and anything changed in the
  file while the form was open stay as they are. If a field you changed was also changed in the file, the
  form says so, and a second `ctrl+s` keeps yours.
- **A new order never replaces another,** and picking a different example to start from asks first once
  you've typed something.
- **Closing a form with changes keeps it** until Reeve quits: `n` (or `e` on the same order) brings it back.

Each order is a file in `~/.reeve/orders/*.toml`, also managed with `reeve orders`. F7 writes three examples
the first time, all off; once deleted, they stay deleted (`reeve orders examples` writes them again). An
order has:

- **a task,** in plain words;
- **triggers:** finding ids (`disk-full:/var`, `unit-failed:*`) and/or a schedule (`daily 03:00`, `weekly sun 03:00`,
  `every 6h`);
- **a scope:** a tier cap (never T3), the tools that may change things, the command globs every part of a
  command must match (`*` stays within a word), and path globs for file tools. Reads are always fine.
- **a budget:** a cap per run, runs per day, and a cooldown.

**reeved runs orders, not cron.** It checks every 10 seconds whether an order's schedule has come due or a
finding it watches for is new, and runs one at a time. A schedule's limits (runs a day, hours between) are
set to let it run each time it's due. Because reeved is a systemd user service, it runs while you're logged
in (a locked screen counts); a daily or weekly run missed while the machine was off or you were logged out
runs when reeved next starts. It runs each finding once per occurrence. A yes inside
the scope is receipted as `order:<id>`. Anything outside it is refused, and the run ends as a proposal in
`/findings`. Nobody is there to type a password, so root commands need a sudoers rule: `reeve orders sudoers <id>`
prints exact ones, and never wildcards. `reeve orders run <id>` runs one now, exactly as `reeved` would.

The model writes orders only through `order_save` and `order_delete`, which always ask you, and only in a
conversation: an order run or the drafter can't write one, so an order can never make another. Writing an
order file any other way (a file tool, the shell) is on the floor (T3), and so is undoing a change to one.

## Skills

A skill is a job you want done your way, saved by name in `~/.reeve/skills/<id>.md`: a one-line
description and the steps.

- **Run one** with `/id` (anything you type after it goes along), from ⌃K, or with `⏎` on the skills tab
  of F8 memory. Reeve also uses a skill when what you ask clearly matches its description, and says so.
- **Make one** by telling Reeve: do something, then say "save that as a skill" (or press `n` on the skills
  tab and describe it). The approval card shows the whole text, and you're asked every time; YOLO doesn't
  cover it. You can also write or edit the file yourself (`e`).
- **A skill grants nothing.** Every step still asks the way it always would.
- Three starters are written the first time Reeve runs: `tidy-downloads`, `update-everything`, and
  `why-slow`. Delete them and they stay deleted.
- A standing order can use a skill: "every Sunday, use the tidy-downloads skill".

## The state of the machine

`/report` (or `reeve report --days 1|7|30`) draws one page and opens it in your browser:

- **Headlines:** what needs you, worst first.
- **Vital signs:** CPU, memory and swap, temperature, load, and network over the window. Hover for readings.
- **Disks:** use, growth per day, and when each disk will be full at that rate.
- **What changed:**
  - packages installed, upgraded, or removed;
  - kernels, and whether a reboot is pending;
  - units enabled or disabled;
  - files edited in `/etc`, with Reeve's own edits marked by receipt.
- **Findings,** **Reeve's work** (verified, rolled back, undone), **spend** by day and role, and **what Reeve has learned**.

It's drawn locally from what reeved and the receipts already recorded: no model, no cost, and nothing leaves the machine. Pages go to `~/.reeve/reports/`, readable only by you, and the last 20 are kept. reeved keeps 31 days of minute readings, and takes a daily snapshot of packages and units, so the "what changed" section fills in over time.

## Privacy

Reeve reads your machine, so what it sends a model could include keys from dotfiles, email addresses, IPs, and your user and host names. Before anything leaves, Reeve swaps those for placeholders (`<secret1>`, `<email1>`, `<ip1>`, `<user>`, `<host>`), and swaps them back here before anything runs or is shown. The model can still say "restart the service on `<host>`", and the command that runs has the real name.

- **Secrets go one way.** A `<secretN>` can only be written back into a file's content (an edit of a file that holds a key still works), never into a command. So an instruction hidden in a log can't have the model send your key anywhere.
- **Levels:** `standard` in chat; `strict` for the drafter and standing orders, which also masks private IPs, MACs, and UUIDs. Add your own words (a company, a project) with `[privacy] terms`.
- **OpenRouter routing:** by default, requests go only to providers that don't store or train on prompts (`data_collection = "deny"`). Zero data retention (`zdr`) is one key away. If no provider for your model qualifies, Reeve says so.
- **`/privacy`** shows all of it, and every value masked this session. Secrets are shown only by their ends.
- Local models are never masked, since nothing leaves.

It's pattern matching, not a guarantee: a secret in a format Reeve doesn't recognize gets through.

## The TUI

Home is the **board**: every tile at a glance, each on its own function key.

| tile | shows |
| --- | --- |
| **F1 needs you** | the approval asking now, with its keys, then the fixes the drafter wrote while you were away |
| **F2 health** | CPU over the last day, memory, swap, temperature, network, and disks |
| **F3 findings** | what reeved noticed, worst first |
| **F4 activity** | every receipt: what Reeve did, and which of it can be undone |
| **F5 spend** | today by role and by hour, and the month |
| **F6 changed** | packages, kernels, `/etc`, and services over the last day |
| **F7 orders** | standing orders and when they run |
| **F8 memory** | what Reeve has learned, newest first |

Press a tile's key (or click it) to open it. It takes the main area: a list and, beside it, the selected
item in detail. The other seven fold into a **strip** along the top that keeps their numbers live, so
nothing useful is ever a screen away. `esc` goes back.

The **chat** opens when you send something, or with `↑` from the board. It's the conversation and
everything Reeve did in it, on one timeline: tool calls branch off Reeve's replies, a verified change is a
bracket around its steps, and on a wide screen each model round shows its cost and the session's running
total. An approval that arrives while you're in the chat asks there; on the board, it waits in F1.

The composer is always at the bottom. On an open tile it knows what's selected: `?` asks Reeve about it.

| key | does |
| --- | --- |
| `⏎` / `alt+⏎` | send / newline |
| `F1`…`F8` | open a tile, from anywhere. Also `tab` / `shift+tab`, a click, or plain `1`…`8` on a tile |
| `↑` | the chat, from the board (empty composer) |
| `esc` | stop the running turn · back to where you were · the board |
| `?` | on a tile: ask Reeve about what's selected |
| `ctrl+k` | search everything: drafted fixes, findings, orders, memory, receipts, tiles, commands. `tab` asks Reeve instead |
| `$` | the spend statement (from an empty composer) |
| `/` | commands: `/providers`, `/model`, `/findings`, `/orders`, `/spend`, `/system`, `/memory`, `/report`, `/privacy`, `/observer`, `/reflect`, `/receipts`, `/new`, `/yolo`, `/help`, `/quit` |
| `^p` / `^r` | `/providers` / activity (`u` undo, `v` verify) |
| `⏎` `a` `s` `n` | on an approval: approve · yes to the rest of this request (or of the verified change) · this kind of change all session · deny |
| `^y` | YOLO: auto-approve T0–T2 actions. The safeguard floor still asks. |
| `pgup` / `pgdn`, mouse wheel | scroll the chat |
| `^c` | stop, clear, then quit |
| `ctrl+l` | redraw the screen (if the terminal cleared it: Konsole's ctrl+shift+k does) |

The default theme is `auto`: on Omarchy, Reeve takes Omarchy's current theme and follows
`omarchy-theme-set` while it runs; elsewhere it's **slate** (cool near-black, filled tiles).
`[ui] theme = "ink"` is the warm charcoal of 0.2.0, and `"brass"` the original navy and brass. A theme
of your own goes in `~/.reeve/themes/<name>.toml` with the keys of Omarchy's `colors.toml`
(`background`, `foreground`, `accent`, `red`, `green`, …), so any Omarchy theme's file works as it is:
`theme = "<name>"`. With 16 colors or none, tiles get borders instead of fills.

## Layout

```
crates/reeve-core      config, keys, providers, pricing, ledger, agent, policy (tiers), tools, receipts, undo
crates/reeve-observer  sampler, journal follower, baselines, detectors, notifications, drafter, the reeved loop
crates/reeve-tui       the board, the chat, and the tiles
crates/reeve-cli       the `reeve` binary
```
