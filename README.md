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
- `reeve models [filter]` lists a connection's models with live prices, including cache read and write rates. Models Reeve can't drive are hidden: ones without tool calling, and image, speech, embedding, and batch-only models. `--all` shows them, and so does `tab` in `/model`.
- `reeve report [--days N]` draws the state of the machine as a page and opens it (also `/report` in the TUI). See below.
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

Home is the **ledger**: the conversation and everything Reeve did, on one timeline. Each row has a time,
a node on the spine, and what happened:
- **Tool calls** branch off Reeve's replies, with their result and receipt number.
- **A verified change** is a bracket around its steps (┏ … ┗), closing on "verified · kept" or "failed ·
  rolled back", with each undo inside it.
- **Background activity:** what reeved found and what the drafter drafted while you were here show up in
  the ledger too.
- **Money:** on a wide screen, two columns show each model round's cost and the session's running total.
  Tool calls and checks cost nothing, and a drafter's draft is marked as its own budget.
- **Approvals:** whatever needs you sits at the bottom, where you type.

The other tabs are full screens: **2 findings** (list, detail, evidence, drafted fix), **3 orders**,
**4 spend** (a statement by day, week, or month; by role and model; export to CSV), **5 memory**, and **6
system** (vitals over 24 h, 7 d, or 30 d, disks, what changed, headlines). The line above the tabs says how
the machine is: "nexus · all quiet, except swap 99%".

| key | does |
| --- | --- |
| `⏎` / `alt+⏎` | send / newline |
| `⌃K` | search everything: drafted fixes, findings, orders, memory, receipts, tabs, commands. `tab` asks Reeve instead |
| `tab` / `shift+tab` | next / previous screen. Also `F1`…`F6`, a click on the tab, or on a tab plain `1`…`6` (`alt+1`…`6` where the terminal doesn't keep it; Konsole does). `esc` goes back to the ledger |
| `$` | the spend statement (from an empty composer) |
| `/` | commands: `/providers`, `/model`, `/findings`, `/orders`, `/spend`, `/system`, `/memory`, `/report`, `/privacy`, `/observer`, `/reflect`, `/receipts`, `/new`, `/yolo`, `/help`, `/quit` |
| `^p` / `^r` | `/providers` / `/receipts` (`u` undo, `v` verify) |
| `⏎` `a` `n` | on an approval: approve, allow for the session (in a verified change: yes to the rest of the change), deny |
| `esc` | stop the running turn, or clear the composer |
| `^y` | YOLO: auto-approve T0–T2 actions. The safeguard floor still asks. |
| `pgup` / `pgdn`, mouse wheel | scroll |
| `^c` | stop, clear, then quit |

The default theme is **ink** (warm charcoal). `[ui] theme = "brass"` brings back the original navy and brass.

## Layout

```
crates/reeve-core      config, keys, providers, pricing, ledger, agent, policy (tiers), tools, receipts, undo
crates/reeve-observer  sampler, journal follower, baselines, detectors, notifications, drafter, the reeved loop
crates/reeve-tui       mission-control UI
crates/reeve-cli       the `reeve` binary
```
