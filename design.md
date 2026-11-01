# Reeve — Design

Status: **proposed** · Started 2026-09-26 · License: Apache-2.0

Reeve is an **operator harness**: an agent that runs on your computer and manages it for you.
It is not a coding agent. It installs and removes packages, fixes services, reads logs, tidies
disks, edits config across the whole filesystem, and learns how *this* machine behaves so it
gets better at managing it over time.

Reeve borrows its provider and cost layers from Ryter as a copied starting point. The two
projects share **no crates**; after the copy they evolve independently.

---

## 1. Principles

Carried from Ryter:

1. **Unknown means unknown.** A missing price renders `$?.??`, never `$0.00`. A budget stop really stops.
2. **Fail closed.** No one to answer an approval → deny. Unknown command → treat as risky.
3. **Keys never leak.** They stay out of logs, receipts, transcripts, and the environment of every command Reeve runs.
4. **Say what is happening.** Thinking, running, waiting on sudo, and watching each get a visible indicator. Silence is a bug.
5. **Degrade, never break.** 80×24, 16 colors, no mouse, no truecolor, and no `reeved` running all still work.
6. **The code shows the what; `DECISIONS.md` records the why.**

New for an operator:

7. **Every action leaves a receipt.** Including actions YOLO approved and actions a standing order ran. No exceptions, no off switch.
8. **Undo before do.** A write that can be snapshotted is snapshotted before it happens, not after.
9. **The machine is the context.** Reeve is not bound to its launch directory. It starts in `$HOME`, and paths are absolute.
10. **Log text is data, never instructions.** Journal lines, file contents, and web pages can contain text written by anyone on the system. Tool output is quoted to the model as untrusted data.
11. **Learning is inspectable.** Everything Reeve "knows" is a plain file you can read, edit, or delete.

---

## 2. Architecture

*As shipped (0.7): reeved and the TUI are the same binary and share files under `~/.reeve`
(findings, proposals, `observer/status.json`, `orders/.run-now`); there is no `reeved.sock`. The
askpass socket is the only socket. The diagram is the original plan.*

```
 ┌──────────────── reeve (TUI, your user) ────────────────┐      ┌──── reeved (systemd --user) ────┐
 │ UI thread ── View/draw ◄── AgentEvent ── worker thread │◄────►│ samplers → baselines            │
 │                                   │  agent loop, tools  │ unix │ journal follower → detectors    │
 │                                   │  policy, receipts   │ sock │ findings → notify + proposals   │
 └───────────────────────────────────┼─────────────────────┘      │ standing orders (scheduler)     │
                                     │ sudo -A (askpass → TUI modal)└─────────────────────────────────┘
                                     ▼
                         ~/.reeve  (receipts, undo, memory, sessions, spend)
                         /var/lib/reeve/undo  (root-owned pre-images of root files)
```

### 2.1 Crates

| Crate | Kind | Holds |
| --- | --- | --- |
| `reeve-core` | lib | config, secrets, `llm` (copied from Ryter), `spend`/`meter`, session store, receipts, undo store, policy (risk tiers, safeguard floor), tools, distro layer, memory, agent loop, IPC message types |
| `reeve-observer` | lib | samplers, journal follower, detectors, baselines, findings, notifications, standing-order scheduler |
| `reeve-tui` | lib | mission-control UI |
| `reeve-cli` | bin `reeve` | TUI by default; subcommands `receipts`, `undo`, `memory`, `orders`, `daemon`, `key`, `doctor`, `update`, `askpass` |
| `reeved` | bin | thin `main` around `reeve-observer` |

Rust 1.88, edition 2024, `#![forbid(unsafe_code)]`, ratatui 0.29 + crossterm 0.28, same as Ryter.

### 2.2 Threads (TUI)

This is Ryter's model. The UI thread owns the terminal and a pure `View`. The worker thread owns the
agent, the tokio runtime, and tool execution. Channels between them: `Work` (UI→worker),
`AgentEvent` (worker→UI), `UserRequest` (approval, sudo password, confirmation). `apply(view, ev)`
is pure, so the UI is snapshot-testable without a terminal.

---

## 3. Providers and cost

The starting point is copied from `ryter-core/src/llm/` and `spend.rs`, then trimmed to what
Reeve needs.

- **Connections:** OpenRouter (`kind = "openrouter"`) and any OpenAI-compatible endpoint
  (`kind = "openai"`: OpenAI, Groq, Together, llama.cpp, Ollama, vLLM, LM Studio). Both use
  Chat Completions with SSE streaming. Local endpoints are priced at $0. Anthropic's own API
  (`kind = "anthropic"`) is a second wire behind the same provider: the Messages API, with tool
  results as blocks in one user turn, a required `max_tokens` taken from the catalog, `reasoning`
  sent as adaptive thinking plus `effort` on the models that take them, and thinking blocks shown
  but not sent back (Reeve's system prompt changes every round, which would invalidate them).
- **Prices:** pulled live from each connection's `GET /models`. For OpenRouter that is per-token
  prompt, completion, and cache pricing. Anthropic's catalog has none, so a table of its published
  list prices stands in. They are refreshed at startup and when the model picker
  opens, and TOML `[pricing]` overrides them. **A cost the provider reports (`usage.cost`) always
  beats the price book.**
- **Tokens tracked:** input, output, cache read, **and cache write**. Ryter ignores cache writes;
  Reeve fixes that gap.
- **Ledger:** every call appends a `SpendRecord` to its session's `spend.jsonl` **and** to the
  global `~/.reeve/spend/YYYY-MM.jsonl`, which budgets count from. `reeved`'s autonomous
  runs write to the same ledger.
- **Budgets:** per session, per day, per month (USD), plus a per-run cap on each standing order.
  They are charged before they are checked, as in Ryter's `Meter::charge`. An autonomous run
  whose model has **no known price refuses to start** while any cap is set. An interactive
  session warns and shows `≥$x`.
- **Keys:** stored in `~/.reeve/keys/<connection>` (0600). The lookup order is Ryter's: inline
  (the config file must be 0600), then `env_key`, then the stored key, then the kind's default
  variable (`OPENROUTER_API_KEY`, `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`). The `keys/` directory is on the policy's
  never-read list (§5.4).

### 3.1 Privacy

Reeve reads the machine, so what it sends a model can include keys in dotfiles, email addresses,
IPs, and names. Two layers keep that down, without asking anyone to run a local model:

1. **Masking, locally.** `MaskingProvider` wraps every provider. It swaps values for stable
   placeholders in each outgoing request (system prompt, messages, earlier tool calls), then
   swaps them back in the streamed text and in tool call arguments before anything runs or is
   shown.
   - **standard** (chat): secrets (known token formats, `*_TOKEN=`/`password:` values, URL
     credentials, private keys), emails, public IPs, the user and host names, and `[privacy] terms`.
   - **strict** (the drafter and standing orders): also private IPs, MACs, and UUIDs.

   A placeholder always means the same value within a session. A secret's placeholder is written
   back only into file content (`fs_write`, `fs_edit`). Anywhere else, and for a placeholder
   Reeve never handed out, the call is refused and never runs. Secrets stay masked in the chat
   too. Local connections are never masked.
2. **Routing, at OpenRouter.** Requests carry `provider.data_collection = "deny"`
   (`no_training`, on by default) and optionally `provider.zdr = true`. When no provider
   qualifies, the error says so and points to `/privacy`.

`/privacy` shows the levels, the routing, and every value masked this session (secrets only by
their ends). The header shows how many.

---

## 4. Tools

Every tool call goes through the same gate: `classify → decide → snapshot → execute → receipt`.

| Group | Tools |
| --- | --- |
| Files | `fs_read`, `fs_list`, `fs_search`, `fs_stat`, `fs_write`, `fs_edit` (exact replace), `fs_move`, `fs_delete` |
| Shell | `shell` (any cwd, timeout, `sudo: bool`) |
| Packages | `pkg_search`, `pkg_info`, `pkg_list`, `pkg_install`, `pkg_remove`, `pkg_updates`, `pkg_upgrade`, `pkg_history` |
| Services | `svc_status`, `svc_list` (incl. failed), `svc_control` (start/stop/restart/enable/disable/mask; user or system) |
| Logs | `logs_query` (unit, priority, since/until, boot, grep, limit) |
| Processes | `proc_list`, `proc_info`, `proc_signal` |
| System | `sys_info` (hardware, OS, metrics now), `sys_disk` (usage by mount, largest dirs) |
| Memory | `memory_search`, `memory_read`, `memory_write` |
| Receipts | `receipt_search` (the agent can see what it did before) |
| Skills | `skill` (read one), `skill_save`, `skill_delete` (§7.1) |

The package, service, and log tools are the preferred path. They give the policy structured
arguments to classify, and each maps to a structured undo (for example `dnf history undo <id>`).
`shell` is the escape hatch, classified by parsing the command line (§5.2).

Command execution is non-interactive, with Ryter's rules: no editors, no pagers, no terminal prompts,
Reeve's keys removed from the environment, and output capped at head plus tail.

---

## 5. Approvals and safety

### 5.1 Risk tiers

| Tier | Meaning | Examples | Default |
| --- | --- | --- | --- |
| **T0 Observe** | Reads, no side effects | `fs_read` (non-sensitive), `logs_query`, `svc_status`, `pkg_search`, `proc_list` | run |
| **T1 User change** | Reversible changes as you | write under `$HOME`, user units, signal your own processes, flatpak `--user` | ask; "allow for session" and per-pattern rules allowed |
| **T2 System change** | Needs root, or touches system state | anything with sudo, `pkg_install/remove/upgrade`, system units, `/etc` writes | ask every time |
| **T3 Floor** | Could destroy the system or leak secrets | see §5.3 and §5.4 | ask with typed confirmation, **even in YOLO** |

A compound command takes the most restrictive tier of its parts. An unknown command runs as T1,
or T2 with sudo.

### 5.2 Classifying shell commands

The shell command is tokenized (pipes, `&&`, `;`, subshells, redirections). Each segment is
classified by program and form, in the style of Ryter's `policy.rs`: read-only programs are T0
unless they use an output-writing form, a redirect to a path is a write to that path, and so on.
Ryter's lists are the starting point, extended for sysadmin tools (`systemctl`, `journalctl`,
`dnf`, `rpm`, `pacman`, `ip`, `nmcli`, `firewall-cmd`, `lsblk`, `smartctl`, and others).

### 5.3 The safeguard floor (T3)

This is the bare minimum that prevents destroying the system. It applies in every mode:

- `mkfs*`, `wipefs`, `dd`/`cp`/`>` writing to a block device, `blkdiscard`, `shred` on a device
- Partition-table writers: `fdisk`, `sfdisk`, `gdisk`/`sgdisk`, `parted` (non-print), `cryptsetup luksFormat|erase`
- Recursive `rm`/`chmod`/`chown` whose target resolves to `/`, a top-level system dir (`/usr /etc /boot /var /bin /lib*`), or `$HOME` itself
- Bootloader and EFI changes: `grub2-install`, `efibootmgr` writes, `bootctl remove`, deleting from `/boot` or `/boot/efi`
- Removing the running kernel, the last installed kernel, or a protected package (`glibc`/`libc6`, `systemd`, `dnf`/`pacman`/`apt`/`dpkg`, `sudo`, `kernel-core`, `linux-image-*`), plus dependency-ignoring forms (`rpm -e --nodeps`, `pacman -Rdd`, `dpkg -r --force-depends`, `apt-get --allow-remove-essential`)
- Writing `/etc/sudoers*`, `/etc/fstab`, `/etc/crypttab`, or `/etc/passwd|shadow|group` without the validator. Reeve runs `visudo -c` / `findmnt --verify` on the new content first, and a failed check blocks the write outright.
- Disabling or masking units the session depends on (`sshd` over SSH, the display manager, `NetworkManager`)
- Fork bombs and `kill -9 -1`

An action in the floor list gets a red confirmation card: type `yes` to proceed. YOLO does not skip it.

### 5.4 Sensitive reads

Reads that would send secrets to a model provider are T3: `~/.ssh/id_*`, `~/.gnupg`,
`~/.local/share/keyrings`, browser profiles, `/etc/shadow`, `*.pem`/`*.key` outside `/etc/pki/ca-trust`.
`~/.reeve/keys/` is **never readable** by any tool in any mode.

### 5.5 YOLO mode

- Approves every T0–T2 action automatically. T3 still asks (§5.3–5.4).
- `^y` toggles it for the current session. `yolo = true` in config makes it the default.
- While it is on, a pulsing **YOLO** badge is shown in the header, the border tint changes, and each
  receipt is stamped `approved_by: "yolo"`.
- Receipts, undo snapshots, and btrfs snapshots all still happen.

### 5.6 Root: `sudo -A`

Reeve always runs as you. A root action runs `sudo -A` with `SUDO_ASKPASS=reeve-askpass`.
The askpass helper connects to the running TUI over a private socket
(`$XDG_RUNTIME_DIR/reeve/askpass-<pid>.sock`, 0600), and the TUI shows a masked password modal.
The password is never sent to the model and never logged. sudo's own timestamp cache is no help
(commands run with no terminal, so it is keyed on each command's own shell), so Reeve remembers
the typed password in memory for `[approvals] password_minutes` (5 by default, 0 never) and
answers each new sudo with it; the helper names the sudo asking, and the same one asking twice
means it was refused, so the modal comes back. `tab` in the modal keeps that one password out
of memory.

`reeved` has no terminal and no password. A standing order that needs root works only if you
install the narrow sudoers drop-in that `reeve orders sudoers <order>` prints. The drop-in lists
exact commands only. Without it, the order stops at the root step and queues a proposal.

---

## 6. Receipts

Each receipt has four parts:

1. **A hash-chained audit log.** One JSON line per action in `~/.reeve/receipts/YYYY-MM.jsonl`:
   ```json
   {"seq":1042,"ts":"2026-09-26T23:41:07Z","session":"0192…","prev":"b3f1…","hash":"9ac0…",
    "tool":"pkg_remove","args":{"packages":["kernel-6.9.4"]},"tier":"T2","sudo":true,
    "approved_by":"user","why":"free 612 MB in /boot",
    "outcome":{"status":"ok","exit":0,"summary":"removed 3 packages"},
    "undo":{"kind":"dnf_history","id":57},"snapshot":{"snapper":[311,312]},
    "cost":{"usd":0.0031,"turn":"0192…"}}
   ```
   `hash = sha256(prev ‖ canonical_json(record without hash))`. `reeve receipts verify` walks
   the chain, and a gap or edit is reported at the exact `seq`. Arguments are redacted by the
   same rules that keep keys out of logs.
2. **Undo.** Before every file write, Reeve stores a pre-image in the content-addressed store
   `~/.reeve/undo/objects/<sha256>` with path, mode, owner, and "existed?" metadata. For
   root-owned files the pre-image lives in `/var/lib/reeve/undo` (root, 0600), so copies of
   root files never land in your home. Structured undo covers packages (`dnf history undo`) and
   units (restore previous enable/mask state). In the TUI, `u` on a receipt undoes it. The undo
   is itself an action with its own receipt, and it checks that the file still matches the
   post-image before reverting: the same content, mode and kind (file or link), so a later edit
   or chmod is never clobbered. An undo's receipt carries its inverse, so it can be undone in
   turn; for a move that overwrote a file, the inverse knows what the undo put back and moves
   over it only while it is still that file.
3. **Session reports.** At session end, `sessions/<id>/report.md` records what was asked, what changed
   (with receipt seqs), what failed, what it cost, and what Reeve learned.
4. **Snapshots.** On btrfs with snapper, a T2 action is wrapped in a `snapper create --type pre/post`
   pair, described `reeve #<seq>: <why>`. Several T2 actions within one approved plan share a
   single pair. If snapper isn't configured, Reeve offers to set up a root config once. It never
   does so silently. On Arch with snap-pac, pacman already takes a pair around every transaction,
   so Reeve adds none around a pacman command (or an AUR helper's) and records snap-pac's pair on
   the receipt instead.

### 6.1 Verified changes

A fix is a transaction that has to prove it worked.

1. **`change_begin`** states the goal and the checks before anything changes. Checks are a fixed
   set that Reeve runs itself:
   - `unit_active`: a unit is active;
   - `journal_quiet`: a unit logs no more than `max` errors after the last change;
   - `disk_below`: a mount is under some percentage full;
   - `command`: a T0 command, with no sudo, exits 0 and, optionally, prints some text.

   A check that would change something is refused at begin.
2. **Each change** made while the transaction is open is tagged with its id (`txn` on the
   receipt). Its approval card shows the goal and the checks, and says whether that step can be
   rolled back.
3. **`change_commit`** waits `wait_secs` (default 3), then runs the checks. If they all pass, the
   receipt says `verified`. If any fails, every tagged change that has an undo record is undone,
   newest first. Each undo gets its own receipt, approved by `txn:<id>`. A change without an
   undo record (most shell commands) is listed as still in place. A turn stopped during the
   wait, the checks or the rollback leaves the change open: the next `change_commit`, or the end
   of a turn, finishes it, and a change already undone (by the stopped rollback, or by the owner)
   counts as rolled back, while one put back again by an undo of that undo is rolled back again.
4. **A transaction left open** when the model ends its turn is committed then, and the model is
   told the result so it can report it.

Only Reeve's check results decide pass or fail. The model's report doesn't count. In a standing
order, a rollback ends the run `rolled_back` and leaves a proposal, like a blocked run.

---

## 7. Memory

All memory is plain files in `~/.reeve/memory/`, each with frontmatter (source, observed_at,
confidence, tags, OS version).

| Layer | Holds | Written by |
| --- | --- | --- |
| **Facts** | hardware, distro and version, filesystem layout, installed services, desktop, quirks ("NVIDIA with akmods", "`/home` is a separate subvolume") | survey at first run, refreshed by `reeved`; the agent, with provenance |
| **Baselines** | what "normal" is: boot time, idle CPU and memory, disk growth rate per mount, usual journal error rate, usual failed-unit set | `reeved` only (numeric JSON, rolling hour-of-week stats) |
| **Runbooks** | problem signature → steps → outcomes (successes and failures, last used, which OS version) | the agent, after a fix is **verified**; failed attempts are recorded too |
| **Preferences** | your rules: "never touch Hyprland config", "prefer flatpak", "don't restart docker during work hours" | you, or the agent from your corrections (confirmed with you before saving) |

**Using it:**
- A compact **machine profile** (key facts plus active preferences) goes in every system prompt.
- Runbooks and older facts are fetched through `memory_search`, which is keyword plus tag scoring
  with no vector database.
- Preferences can compile into policy. "Never touch X" becomes a hard deny rule, not just advice.

**Learning loop:**
1. **Observe.** `reeved` keeps updating baselines.
2. **Act.** A fix produces receipts.
3. **Verify.** Reeve checks that the symptom is gone: the unit is active, the error rate is back
   to baseline, or the disk is below its threshold.
4. **Reflect.** At session end, a cheap model reads the session and proposes new facts, a
   new or updated runbook with its outcome, and preference candidates. A runbook's "worked"
   counts only when the receipts bear it out: a verified change passed its checks, or something
   ran and nothing failed. Otherwise the runbook is left out, and the summary says so.
5. **Review.** Facts and runbooks are written with provenance and show as "new" in the Memory
   panel. Preferences wait for your yes.

Runbook confidence falls with failures and resets for review after a major OS upgrade.

### 7.1 Skills

A skill is a job the owner wants done their way, by name: `~/.reeve/skills/<id>.md`, a header
(`name`, `description`) and the steps in Markdown. Runbooks are what Reeve learned; orders say when
Reeve works alone; a skill is how the owner wants something done when they ask, and an order's task
can say to use one.

- **Using one:** every system prompt lists the ids and descriptions. Reeve reads the steps with
  `skill` (T0) when the owner types `/id` (anything after it goes along), picks it in ⌃K or F8, or
  asks for something that clearly matches a description, and says which skill it's using. A skill
  grants nothing: each step goes through the same tiers and approvals.
- **Writing one:** "save that as a skill" makes Reeve call `skill_save`. It's `owner_only`, like an
  order: the card shows the whole text as a diff and asks every time, YOLO and broad yeses don't
  cover it, and unattended runs can't. The owner can also edit the file (`e` in F8). A raw write
  into `~/.reeve/skills` is on the floor: a skill is text Reeve trusts later, so nothing gets in
  unseen. Saves and deletes carry an undo.
- **Where they show:** a skills tab in F8 memory (run, edit, delete, new), the slash palette, and ⌃K.
- **Starters:** three are written once (`tidy-downloads`, `update-everything`, `why-slow`); deleted
  ones stay deleted.

---

## 8. The observer: `reeved`

A `systemd --user` service installed by `reeve daemon install`. **It watches and records; it
never acts** unless a standing order says so (§9). Reeve is Linux-only for now; macOS and
Windows will need their own observer design later.

- **Samplers** (every 5 s, downsampled to 1 min on disk): CPU, load, memory and swap, disk usage
  per mount, disk I/O, network, temperatures (hwmon), and battery.
- **Journal follower:** `journalctl -f -o json -p warning` (readable by `wheel` on Fedora), grouped by
  unit and message template.
- **Periodic checks:** failed units (system and user) every minute; pending updates and
  security advisories every 6 h; boot time after each boot; SMART data only if a sudoers rule allows it.
- **Detectors** are rules, not LLM calls: a threshold (disk > 90%), a trend ("`/var` fills in ~4 days"),
  a baseline deviation ("journal errors from `bluetooth.service` 20× normal"), or a state change
  (a unit newly failed, a reboot pending after a kernel update).
- **Findings** go to `~/.reeve/findings/`. They are reported, not announced: `/findings`, the header badge, and
  the agent's prompt, with no popup. A desktop notification means a proposed fix is waiting: the drafter
  wrote one, or a standing order stopped at its scope. `notify_findings = true` opts back into popups for
  findings, paced.
- **Releases:** a minute after it starts and every 12 h, reeved asks GitHub for the latest release
  (§8.2).
- **Proposals:** when you open Reeve, the Findings inbox shows each finding, and "draft a fix"
  runs a read-only (T0-only) diagnosis to produce a proposed plan you approve. Nothing runs
  without you, except under §9.
- **The drafter** (off by default): a separate role that drafts those proposals in the background,
  so a plan is waiting when you open Reeve. It works like an auditor seat:
  ```toml
  [observer.drafter]
  enabled = false
  connection = "openrouter"            # default: the main connection
  model = "…"                          # default: the main model; a cheap one is plenty
  daily_usd = 0.25                     # its own cap, per day
  per_draft_usd = 0.05                 # stop a single draft past this
  max_drafts_per_day = 10
  min_severity = "warning"             # don't draft for notices
  ```
  - **Turning it on:** `/observer` in the TUI toggles it and sets the model and budgets, saved to
    `settings.toml`, or edit `config.toml` directly.
  - **Tools:** T0 only, so it can read and diagnose but never change anything. The draft is a
    proposal that waits in the inbox.
  - **Spend:** recorded in the global ledger under the `drafter` role. It counts toward the global
    day and month caps as well as its own; whichever is hit first stops it. The Spend panel shows
    drafter spend on its own line.
  - **Over budget:** the finding stays undrafted and says why ("drafter budget reached for today").
  - **Unknown price:** a model with no known price can't be used for the drafter while any cap is
    set; it fails closed.
- **Nothing waits on slow work.** The 6-hourly checks (package queries) and a draft (model calls,
  minutes) run as tasks beside the loop and report back on a channel it listens to, so sampling,
  the heartbeat, the journal and standing orders keep their cadence meanwhile. Each has a deadline:
  the checks get 10 minutes, and past it they resolve nothing (a finding they didn't see isn't
  gone); a draft stops itself between model calls after 15 minutes, with every call it made in
  the ledger, and reeved cuts off one stuck in a call that never returned at 35. A draft that
  didn't finish leaves its note on the finding, so the next pass doesn't pick it again. One of
  each at a time; the heartbeat says what's running and since when (`/observer`, `reeve daemon
  status`).
- **Sharing state:** `reeved` and the TUI share files, not a socket. Findings are one JSON file each in
  `~/.reeve/findings/`, and `~/.reeve/observer/status.json` holds a heartbeat every 10 s. The TUI polls
  them each second and shows "observer ○ off" when the heartbeat is stale. The service runs
  `reeve daemon run`, the same binary as the TUI.

### 8.1 The state of the machine (`reeve report`, `/report`)

One self-contained HTML page with inline SVG. There's no model and no network, and the page loads
nothing from outside, not even fonts. Colors are CSS variables, so the page follows the system's
light or dark theme; a small inline script adds a hover readout. Sources:

- **Vital signs:** reeved's minute rows in `observer/metrics/<day>.jsonl`, now kept 31 days,
  averaged into 360 points. A bucket with no rows is a gap. The charts start at the first reading
  when reeved started after the window did.
- **Disks:** a least-squares slope over hourly means gives growth per day and a days-to-full
  estimate. It needs 20 h of history; below 0.01% a day, a disk counts as steady.
- **What changed:**
  - packages from rpm's `INSTALLTIME` (Fedora), `/var/log/pacman.log` (Arch), or `/var/log/dpkg.log` (Debian);
  - removals and unit changes from the daily snapshot reeved writes to
    `observer/state/<day>.json` (60 days kept);
  - `/etc` from file modification times, matched against receipts to mark Reeve's own edits;
  - a pending reboot from the newest `kernel-core` against `uname -r`; on Arch, the running
    kernel's missing modules; on Debian, the newest `linux-image-*` against `uname -r`.
- **Findings, receipts, spend, memory:** the usual stores.
- **Headlines:** rules over the above (a disk full within 90 days, swap full ≥ 50% of the time,
  temperature past `temp_warn`, critical findings, fixes verified or rolled back, a pending
  reboot).

Everything shown is escaped. Log lines and finding text are data.

### 8.2 Updates (`reeve update`)

- **Checking:** reeved asks GitHub's latest-release endpoint (user agent `reeve/<version>`, nothing
  else sent). Drafts and pre-releases never count. The answer goes to `observer/update.json`, with
  the version reeved runs, which is the one installed. When reeved isn't running and the answer is
  more than a day old, the TUI asks instead, in the background. `[updates] check = false` stops both.
- **Showing:** the header gets `↑ 0.5.0`, or `restart for 0.5.0` when the new version is installed
  but this window predates it. `/update` and ⌃K say what's out and how to get it. There's no desktop
  notification: a new version isn't a proposed fix.
- **Installing:** `reeve update` works out how this copy got here and does the same again:
  - **First, the signature:** the release's `SHA256SUMS` must carry `SHA256SUMS.sig`, an ed25519
    signature made with Reeve's release key. The public half is in the repository
    (`release/reeve-release.pub.pem`), built into every Reeve and pinned in `install.sh`; the
    private half is a secret of the release workflow, which signs and then verifies against the
    committed key before drafting. A release that isn't signed (every one before 0.8.0), is signed
    with another key, or whose checksums changed after signing is refused before anything is
    downloaded. Rotating the key means releasing a build with the new public key before the new
    key signs anything.
  - **install.sh** (a `bin/` under some prefix): download the release tarball and `SHA256SUMS`,
    refuse a mismatch, check that the archive's binary is the expected version, then run that
    release's own `install.sh --from` with flags matching this install. The installer never starts
    reeved; `reeve update` restarts it only when it runs the binary just replaced.
  - **An RPM or .deb from a repository** (dnf's record of the source repository isn't
    `@commandline`; apt's policy lists the package from a URL): updated through that repository,
    by version (`sudo dnf upgrade -y reeve-<version>`, `sudo apt-get install -y
    reeve=<the repository's version>`), never with the release's own package, so a distribution's
    signed build stays the one installed. If the repository doesn't have the version yet, Reeve
    says so and nothing changes.
  - **An RPM installed by hand:** the release RPM, checked the same way, through `sudo dnf upgrade`
    (or `downgrade`).
  - **A .deb installed by hand:** the release .deb, checked the same way, through
    `sudo apt-get install` (with `--allow-downgrades` for a rollback).
  - **A pacman package:** the AUR helper.
  - **A cargo build, or a binary somewhere no installer puts it:** refused, with what to do instead.
- **Receipts and rollback:** every attempt leaves a `reeve_update` receipt (T1 under your home, T2
  otherwise), failures too. `--rollback` installs the version the last update replaced, from its own
  release; nothing kept in your home is ever copied into a system directory.

---

## 9. Standing orders

Standing orders are scheduled autonomous work you define once. They are the only way Reeve acts
without asking.

```toml
# ~/.reeve/orders/keep-var-lean.toml
name = "keep /var lean"
trigger = { finding = "disk.usage", mount = "/var", above = 85 }   # or: schedule = "weekly sun 03:00"
max_tier = "T2"
allow = ["journal_vacuum", "pkg_clean", "fs_delete:/var/tmp/**"]
budget_usd = 0.10
model = "openrouter:anthropic/claude-haiku-4.5"
notify = "after"          # after | before | never
```

- When a finding matches an order's trigger, `reeved` runs the agent with the order's scope as a
  hard allowlist. Anything outside it stops the run and queues a proposal.
- The floor (§5.3–5.4) is never auto-approved by an order.
- Receipts are stamped `approved_by: "order:keep-var-lean"`, and you get a notification with the result.

---

## 10. Distro layer

```rust
trait Distro {
    fn id(&self) -> DistroId;                 // fedora, arch, …
    fn packages(&self) -> &dyn PackageManager; // search/info/install/remove/upgrade/history/undo
    fn snapshots(&self) -> Option<&dyn Snapshotter>; // snapper; timeshift later
    fn protected_packages(&self) -> &[&str];
    fn notes(&self) -> &[&str];               // e.g. SELinux relabel after moving files into /etc
}
```

- **Fedora first:** `dnf5` with `dnf history undo`, `flatpak`, SELinux awareness (`restorecon`
  after writes into labeled paths, and `ausearch` in diagnoses).
- **Atomic variants** (Silverblue, Kinoite) are detected, and package tools refuse with a clear message in v1.
- **Arch:** `pacman`, with undo from its log and cache: Reeve records what its command changed as
  pacman logged it (`/var/log/pacman.log`: installed, upgraded, downgraded, removed, with versions),
  and undo puts the old versions back from the package cache with `pacman -U` and removes what was
  new with `pacman -R`. Those are two transactions and either may need the other done first, so
  Reeve tries each order with `--print` (nothing changes) and runs the one that resolves. `--print`
  checks dependencies but not conflicts, so if pacman then refuses for real (a package that replaced
  another), which changes nothing, the other order is run. A version the cache lost, or changes
  neither order can take apart, are named and the undo refused before anything runs. An undo that stops between its two steps leaves a receipt that takes back the
  step that ran.
  - **The AUR**, through `paru` or `yay`: a package the repos can't satisfy (by name, group, or
    something a repo package provides) is built with the helper, asking nothing, through Reeve's
    askpass. Its PKGBUILD is on the approval card, and the yes is the owner's every time
    (`owner_only`): YOLO and session yeses never cover it, and unattended runs refuse it. A raw
    `paru -S` or `yay` through `shell` is owner-only too.
  - **Upgrades:** an empty `pkg_upgrade` is `pacman -Syu`, repos only. AUR updates are listed with
    `aur:` and rebuilt by name, each on its own card.
  - **snap-pac** (§6) and **reeved**: a reboot is pending when the running kernel's modules are gone
    from `/usr/lib/modules`; security updates come from `arch-audit -u` when it's installed.
  - **Omarchy:** the theme follows Omarchy's (§11).
- **Debian** and its family (Ubuntu, Mint, Raspberry Pi OS, by `ID` or `ID_LIKE`): `apt-get` and `dpkg`, with
  undo from dpkg's log and apt's cache. Reeve records what its command changed as dpkg logged it
  (`/var/log/dpkg.log`: `install`, `upgrade`, `remove`, with versions; a downgrade is logged as an upgrade
  with the versions reversed, and a `purge` of an installed package logs `remove` first, so `purge` alone is
  config files going). Undo is one transaction: `apt-get install` with the old versions' files from
  `/var/cache/apt/archives` and `name-` for what was new. `apt-get -s` runs first (no root, nothing
  changes): if apt can't resolve it, or would remove a package the change didn't install, or would change
  the version of one it didn't touch (every undo allows downgrades), the undo is refused. Names are compared
  with their architecture: apt prints the native one bare, so `Remv libfoo:i386` never stands for
  `libfoo`. A version the cache lost (`apt-get
  clean`, or an install through `apt` rather than `apt-get`, which deletes downloads) is asked for as
  `name=version`; if the repos no longer carry it, apt says so and nothing changes. A failed command still
  records what dpkg did before it stopped, and so does a failed undo (`UndoFailed.partial`).
  - **Commands:** installs and upgrades run `apt-get update -qq` first and pass
    `-o Dpkg::Options::=--force-confold`, so a conffile the owner edited is kept instead of stopping at a
    prompt no one can answer. An empty `pkg_upgrade` is `apt-get upgrade --with-new-pkgs` (what `apt
    upgrade` does): new dependencies and kernel packages come in, nothing is removed, and what would need a
    removal is held back and listed. `pkg_remove` is `apt-get remove`: config files stay, and no
    `--autoremove`, which would also sweep up packages orphaned earlier. `pkg_list updates` reads the lists
    as they are and says so (only root can refresh them). `pkg_history` is apt's `history.log`, a block per
    run.
  - **Environment:** `DEBIAN_FRONTEND=noninteractive`, `APT_LISTCHANGES_FRONTEND=none`, and
    `NEEDRESTART_MODE=l`: needrestart lists the services an upgrade left on old libraries rather than
    restarting them on its own; a restart is an action with its own approval and undo.
  - **The floor:** `libc6`, `dpkg`, `apt`, `systemd-sysv`, `grub-*`, `shim-signed`, `initramfs-tools`,
    `linux-image-amd64`, and the rest of the Debian list in `protected_packages`; `install foo-` is a
    removal, and a `/release` or `=version` suffix comes off before the name is read (`libc6/now`);
    a package argument that isn't a name is read the way apt reads it (`policy/apt_syntax.rs`:
    a case-folded glob, apt's unanchored case-insensitive regex, or an apt pattern parsed with
    apt-patterns(7)'s grammar, a `:arch` suffix split off first) and is the floor when it can take the
    running kernel's image or a protected name (`linux*`, `LINUX*`, `~nlinux~i`, `?not(~nhtop)`,
    `~ramd64`, `lib*`) or when apt itself couldn't parse it; the running kernel's
    `linux-image-$(uname -r)`; `--allow-remove-essential` and `--force-yes` as flags or as the config
    items they set, with any value apt's `StringToBool` reads as true (`1`, `0x1`, `+1`, `enable`);
    dpkg's `--force-depends`, `--force-all`, `--force-remove-essential` when removing, as a flag, in a
    comma list, in `-o Dpkg::Options::=…`, or in `DPKG_FORCE=` (on the command, `+=`-appended,
    `export`ed earlier on the line, which includes a later bare assignment to an exported name and
    `declare`/`readonly` or any assignment under `set -a`, `set -o allexport` or `shopt -os allexport`,
    behind `command`/`builtin` too, while `export -n`/`declare +x` undo it and a subshell or a pipeline
    stage changes nothing after itself; inherited by a `bash -c` script, an `eval` of literal words, or
    a `$(…)`; or through `env`, which is read as getopt_long reads it: clusters,
    abbreviated long options, a bare `-`, and `-S` strings env re-reads as its own command line); an
    apt config file Reeve hasn't read (`-c FILE`, `-cFILE`, `APT_CONFIG=`) with a removal. apt's command
    line is read by apt's rules throughout, so `-s 0` and `--no-simulate` are not simulations. The
    floor doesn't rest on that model of bash, though: `forced_by_env` reads the text of the whole line,
    and a line that gives `DPKG_FORCE` a force word or `APT_CONFIG` any value by any route (an assignment
    in any scope, exported or not, `${NAME:=…}`, `printf -v`, `read`, `for`, a nameref), or a value Reeve
    can't read (`$(…)`, `$VAR`), and removes a package, asks. Only a read (`$NAME`), an unset, and
    `export NAME` without a value are nothing. The scan runs over the raw text and over the tokenized
    words at every level a line nests (quotes joined, `$'…'` decoded, braces expanded in values, so
    `DPKG_FOR''CE`, `{confold,remove-essential}` and `$'\x44PKG_FORCE'` read as what bash runs); an
    expansion where the environment is set (`export $(…)`, `declare $(…)`, `env $(…)`, `sudo $(…)`),
    where the program is named, or where a removed package is named can't be read and is the floor on a
    removal; an alias defined on the line runs in place of the command word; `$NAME` is filled in where
    the line gave NAME a literal. Brace expansion happens in the tokenizer, on unquoted text, before
    anything else, as bash does it (`{a,b}`, `{1..9}`, nested; past 512 words the word is unreadable),
    so `DPKG_F{ORCE,}=…`, `--force-{remove-essential,confold}` and `{dpkg,--force-remove-essential,-r,sed}`
    read as the words bash runs. A function defined on the line is read at each call with the call's
    arguments in `$@`/`$1`…/`${N}` (the last definition before the call; `shift` and `set --` move the
    positionals; a body calling another function the line defines runs it too; a call behind `if`,
    `{`, `time` counts), so `dpkg() { command dpkg "$@"; }; dpkg --force-remove-essential -r sed` is the
    command it forwards. Two rules close what the models don't follow: an expansion left in a
    removal's arguments (`--force-${1:-remove-essential}`) is the floor, and a brace Reeve stopped
    expanding (more than 4096 words) is the floor wherever it stands. Over-asking there is by design;
    the model above only makes the common cases precise.
  - **reeved:** a reboot is pending when the newest installed `linux-image-<abi>` isn't `uname -r`, when
    the running kernel's package version no longer matches the build in `/proc/version` (a security
    rebuild under the same name), or when `/var/run/reboot-required` is set. Security updates are the
    `Inst` lines of `apt-get -s upgrade` from a security suite, as of the last `apt-get update`.
  - **Not yet:** a `.deb` package (the installer's static binaries run on any release), `snap`, and
    `do-release-upgrade`.

---

## 11. The TUI: the ledger, and tabs for the rest

*As shipped (0.7): the TUI is the board of tiles (DECISIONS 2026-09-29), F1–F8 open tiles from
anywhere, esc returns to the board, the composer stays at the bottom. This section describes the
earlier ledger-and-tabs design; the approval card and receipt semantics below still hold.*

```
 reeve   F1 ledger   F2 findings 7   F3 orders   F4 spend   F5 memory   F6 system   nexus · all quiet, except swap 99%
─────────━━━━━━━━──────────────────────────────────────────────────────────────────────────────────────────────────
                                                                                                    cost    session
  09:34  ⚑ drafter  drafted a fix for mailsync keeps crashing                                    ($0.0276) own budget
  10:38  ● you  mailsync keeps crashing and swap is full. what's going on?
  10:39  ◆ reeve  7.9k in · 153 out                                                                $0.0120   $0.0120
         ├ ✓  T0  coredumpctl list mailsync --since -24h ·········· 102 dumps  #81                    —
         └ ✓  T0  memory_search "mailsync crash" ·················· 1 runbook  #83                    —
  10:41  ┏ verified change  stop mailsync crashing
         ┃ checks  unit active · no coredumps for 20 min
         ┃ ✓  T1  stop app-com.getmailspring… (user) ················ stopped  #85 ↶                 —
         ┗ verified · kept  stop mailsync crashing (2 checks passed)  #88                   $0.0048   $0.0180
 ────────────────────────────────────────────────────────────────────────────────────────────────────────────────────
  › Ask Reeve about this machine · / commands · ⌃K everything
 LEDGER  session $0.0180 · today $0.31                          grok-4.7 · ▣ 5 masked · reeved ●  TIERED   ⌃K everything
```

- **The ledger** (home) is the transcript drawn as a timeline: time, spine, row, and on wide screens
  (≥ 96 columns) a cost and a running-total column. A model round's cost goes on its reply, or, for a
  round that only called tools, on its first tool call. Tool calls branch (├ └). `change_begin` and
  `change_commit` draw a bracket (┏ ┃ ┗), and a rollback's undos go inside it. New findings (reeved) and
  new drafts (the drafter) seen while Reeve is open appear as ⚑ rows; a draft's cost is its own budget,
  not the session's. The approval card, or the composer, is at the bottom.
- **Tabs** are the panels the old layout floated, drawn full-screen: findings (list and detail with
  evidence and the drafted fix), orders, spend (`ledger::statement`: one line per session and role, by
  day, week, or month, with by-role and by-model totals and CSV export), memory, and system (the report's
  data for 24 h, 7 d, or 30 d, next to the live readout, what changed, and headlines). Switching: `F1…F6` (as the tabs
  are labelled), `tab` / `shift+tab`, a click on the tab row, plain digits on a tab, or `alt+1…6` (Konsole keeps those for its own
  tabs); `esc` returns to the ledger.
- **⌃K** searches one index built when it opens: drafted fixes, open findings, orders, memory notes,
  recent receipts, tabs, and commands. The query itself is always the first row ("ask Reeve"). Each
  result says where it goes.
- **The status sentence** (top right) says how the machine is in a few words. The status line (bottom)
  shows the tab's mode and context, the model, masking, reeved, and the approval mode.
- **Approvals inside a verified change:** `a` approves the rest of the change (`Decision::AllowChange`):
  T1/T2 steps until `change_commit`, never T3, recorded as `change-rule`. The checks and rollback still
  guard the whole change. Outside a change, `a` is "allow this exact action for the session" (T1).

**Themes:** the default is `auto`. On Omarchy it's Omarchy's current theme, read from its
`colors.toml` (`~/.local/state/omarchy/current/theme/`, or `~/.config/omarchy/current/theme/` before
Omarchy 4) and re-read every second, so `omarchy-theme-set` recolors Reeve too; elsewhere it's
**slate**. Omarchy's keys map onto Reeve's roles: `background`, `lighter_background` (tiles),
`selection` (raised), `muted` (borders), `accent` (Reeve and the focus), `foreground`, `green`,
`yellow`, `red`, `orange`, `cyan`, `magenta`; anything missing is mixed from the background and
foreground, so light themes work. A custom theme is `~/.reeve/themes/<name>.toml` with the same keys.
**ink** is warm charcoal and **brass** (deep navy, brass and amber) the original. Everything degrades
to 256, 16, or mono colors, with `NO_COLOR` respected.

**Looking at it without a terminal:** `REEVE_SHOTS=<dir> cargo test -p reeve-tui shots -- --ignored`
renders each screen to colored HTML (with `REEVE_SHOTS_HOME`, from a real Reeve home, read-only).

---

## 12. State layout

```
~/.reeve/
  config.toml            # yours (0600 if it holds a key)
  settings.toml          # written by the TUI
  keys/<connection>      # 0600, never tool-readable
  sessions/<uuidv7>/     # meta.json, transcript.jsonl, events.jsonl, spend.jsonl, report.md
  receipts/YYYY-MM.jsonl # hash chain
  undo/objects/<sha256>  undo/index.jsonl
  spend/YYYY-MM.jsonl    # global ledger (budgets)
  memory/{facts,baselines,runbooks,preferences}/
  skills/<id>.md         # the owner's skills
  findings/  proposals/  orders/*.toml
  .settings.lock  orders/.state.lock  receipts/.lock  observer/.update.lock   # held while a shared file is read, changed and written
  observer/metrics/YYYY-MM-DD.jsonl
  observer/update.json   # newest release seen, version installed, version replaced
  themes/*.toml
/var/lib/reeve/undo/     # root-owned pre-images (created via sudo on first use)
$XDG_RUNTIME_DIR/reeve/  # reeved.sock, askpass-<pid>.sock
```

`REEVE_HOME` overrides `~/.reeve`. There is no SQLite; JSONL is the source of truth.

---

## 13. Milestones

| # | Goal | Done when |
| --- | --- | --- |
| **M0** ✓ | Scaffold | Workspace builds. Config and keys work. Provider and spend are copied in. The TUI draws the mission-control layout with a live System rail and Spend panel. Chat streams with no tools. |
| **M1** ✓ | Hands with a paper trail | File and shell tools, tier classifier, approval cards, YOLO, floor, hash-chained receipts, file undo, `reeve receipts verify`. |
| **M2** ✓ | Sysadmin | Package, service, log, and process tools. Fedora distro layer. `sudo -A` askpass modal. Snapper pairs. Session reports. |
| **M3** ✓ | Memory | Four layers, memory tools, machine profile in the prompt, the reflect step, the Memory view. |
| **M4** ✓ | Observer | `reeved` with samplers, journal, detectors, baselines, notifications, and the Findings inbox with proposals. |
| **M5** ✓ | Standing orders | Scheduler, scoped autonomous runs, day and month budgets enforced across TUI and daemon. |
| **M6** ✓ | Arch | pacman undo from its log and cache, the AUR with the PKGBUILD on the card, snap-pac, Arch checks in reeved, Omarchy themes. |
| **M7** ✓ | Debian | apt-get and dpkg through the same tools, undo from dpkg's log and apt's cache in one simulated-first transaction, the Debian floor, Debian reboot and security checks in reeved, dpkg's log in the drift report. |

---

## 14. What 1.0 means

1.0 is a gate, not a count. Reeve stays on 0.x (0.10, 0.11, and on) until every item below is
done, and reaching the end of the list makes 1.0 possible, not automatic. A ticked box is done
today; an open one is not, or not yet proven.

**It runs where it says it does**

- [ ] **Arch is as solid as Fedora.** M6: install, remove, upgrade, and undo through pacman (and
  the AUR helper when there is one), snap-pac snapshots, and Omarchy theme import. Today: all
  built, and pacman, its undo, and yay pass a live test on Arch (in a container). Not yet proven
  on a real machine: paru, snap-pac, and the Omarchy theme following a running Omarchy.
- [ ] **It installs as a package.** Published Fedora (COPR) and Arch (AUR) packages ship `reeve`
  and `reeved.service`, and `reeve daemon install` uses the packaged unit. Today: the spec and
  PKGBUILDs are in `packaging/`, and installs go through `install.sh`.
- [ ] **Snapshots work out of the box.** On btrfs with no snapper root config, Reeve offers once to
  create one, never silently, and T2 actions get pre/post pairs from then on. Today: `reeve
  doctor` reports the missing config.
- [x] Image-based Fedora (Silverblue, Kinoite) is recognized, and package changes are refused with
  a clear message.

**Safety holds**

- [ ] **The floor is proven.** Every rule in §5.3 and §5.4 has a test, a corpus of real commands
  has expected tiers, and no mode (YOLO, a session or change "yes", a standing order) gets past
  the floor without the typed confirmation.
- [ ] **Undo covers what it claims.** Files, dnf and pacman transactions, and unit state each undo
  cleanly, and an undo refuses when the target changed since. *Files compare content, mode and
  kind as of 2026-11-01; moves and units are still to record a post-state.*
- [ ] **No key leaks.** Tests push known token formats through prompts, tool output, receipts,
  logs, and session reports, and none comes out unmasked.
- [x] A standing order that needs root runs only with the drop-in `reeve orders sudoers <order>`
  prints. Without it, the order stops at the root step and leaves a proposal.

**It runs unattended for months**

- [ ] **reeved stays small.** Metrics, state, findings, proposals, and scratch all have retention
  limits. A month of running leaves `~/.reeve` and reeved's memory flat, and it survives reboot,
  suspend, and its own upgrade.
- [ ] **It keeps itself current.** reeved notices a new release and the app shows it, without a
  popup. `reeve update` installs it the way Reeve was installed, checks it against the release's
  `SHA256SUMS`, leaves a receipt, and can roll back.
- [x] Budgets hold across the TUI, the drafter, standing orders and reflection, and a model with
  no known price fails closed while any cap is set.

**Nothing you keep is lost**

- [ ] **Upgrades never lose data.** Every file format in `~/.reeve` carries a version, and 1.x reads
  or migrates whatever an earlier release wrote: orders, memory, settings, receipts.
- [x] Receipts form a hash chain, and `reeve receipts verify` reports the exact break.

**It's usable without the source**

- [ ] **The TUI degrades as §1 promises.** 80×24, 256 and 16 colors, mono, `NO_COLOR`, no mouse,
  and no reeved each render correctly in the screenshot harness.
- [ ] **First run teaches itself.** From a fresh install, you can set up a provider, run the
  survey, and get a useful answer without opening the docs.
- [ ] **The docs match the product.** A user guide covers approvals, undo, orders, privacy, and the
  drafter, and this design describes what shipped (§11 still shows the ledger UI, not the board).

**Not needed for 1.0:** timeshift, and macOS or Windows.

---

## 15. Road to the next releases

Written 2026-10-29 against 0.7.1, from a day of field use and a 25-finding review of the code
(referred to below as G01–G25). The review's own words: this is not a missing-feature problem; the
gaps are between the guarantees Reeve states and what happens under failure. The order below puts
what stops daily use first, what makes unattended work trustworthy second, and the 1.0 gate (§14)
last. Sizes: S is under half a day, M one or two days, L a week or more. Every fix lands with a
regression test that fails on 0.7.1.

**Release A, fixes only (a PATCH): shipped as 0.7.2 on 2026-10-30**

- The sudo password is asked once per `password_minutes`, not every other root step (the remembered
  password is keyed on the sudo asking; DECISIONS 2026-10-29).
- Orders count every start against `runs_per_day`, run-now requests wait their turn, and a journal
  that wasn't read fails the `journal_quiet` check instead of passing it (G05, G08, G09).
- Failure paths fail closed (DECISIONS 2026-10-29, "Failure paths fail closed"): temp files are
  created exclusively under random names, as is the root helper's check file (G02, part of G17);
  no tool call runs when a receipt can't be written (G03, the cheap half); `~/.reeve`, sessions,
  ledgers and transcripts are private whatever the umask, and a torn ledger line can't swallow the
  next record (G06); an unreadable ledger is a floor, not zero, and stops unattended spend (G07);
  reeved with a config or order state that doesn't load watches and starts nothing (G11); an
  errored run or draft is a failure (G12); `fs_read` and search cap what they load (G20); a
  `local` connection must point at this machine (G06); interpreter one-liners that name Reeve's
  keys are refused and ones that name its receipts or undo store take the typed yes, and the README
  says what is true (G01, the part a parser can do); change ids can't collide (part of G04); the
  unit description, the permission-denied message and design §2 and §11 tell the truth (G25).

Done when: `cargo test --workspace`, clippy and fmt pass (they do), the live container tests for
pacman and apt pass, and the dev → main PR has its two external reviews.

**Release B, features (a MINOR): items 1, 4, 5 and 6 ship as 0.8.0 (2026-10-31), with signed releases
and repository-aware updates besides; 2 and 3 remain**

*0.9.0 (2026-11-01) adds Anthropic's own API as a connection kind (DECISIONS 2026-11-01); 2 and 3
still remain.*

1. **The observer loop never blocks** (G10). Slow checks and the drafter run as supervised tasks
   with deadlines, like orders already do; sampling, the heartbeat and the scheduler keep their
   cadence while a model call takes minutes. M. *In dev as of 2026-10-30 (§8, "Nothing waits on
   slow work"; DECISIONS 2026-10-30).*
2. **Snapshots out of the box** (G14, §14). The once-only offer to create the snapper root config;
   a failed pre-snapshot shown on the card and the receipt; the coverage a T2 action has, shown
   before it runs. M.
3. **Long sessions** (G18). When the transcript nears the model's context length, older rounds are
   compacted; the owner's constraints, pending approvals and the open change are kept. M.
4. **A .deb** in the release workflow, since the servers it's wanted on are Debian; install.sh stays.
   M. *In dev as of 2026-10-30, with the source tarball, the filled spec and the third-party
   notices (DECISIONS 2026-10-30, "The release carries every package").*
5. **A runbook "worked" only with evidence** (G19): reflection counts a success only when the
   session has a verified change or no failed receipt. S. *In dev as of 2026-10-30 (§7, step 4).*
6. **Locked updates for shared files** (G17): settings and order state use the lock the receipts
   already use. S. *In dev as of 2026-10-30 (`lockfile::hold`; DECISIONS 2026-10-30).*

**The 1.0 gate (§14), with the findings that bear on each**

- The floor is proven: a real execution boundary for shell programs (Landlock or bubblewrap masking
  Reeve's state; or, if neither is available, an honest statement of the trust model) and tests
  that execute, not only classify (G01, G23). L.
- Receipts cover every change: an intent record before the action and a completion after, with
  reconciliation at startup; an open verified change survives a crash or cancel (G03, G04). L. *A
  stopped check or rollback leaves the change open for the retry as of 2026-11-01; the intent
  record and the crash case stay open.*
- Undo covers what it claims: moves and units record a post-state and refuse when it changed;
  partial multi-file restores leave their inverse (G13). M. *A file's mode and kind are compared,
  and an undone overwriting move can be redone, as of 2026-11-01; the rest stays open.*
- reeved stays small: retention for sessions, undo objects and findings; a 90-day soak with
  measured disk, memory and query time (G15). M.
- Upgrades never lose data: a version in every file format, fixtures from each shipped release,
  and a migration test (G16). M.
- `reeve update` and rollback behave the same on every install method, or the docs say where they
  don't; a signed release manifest if that becomes a requirement (G22). M. *The signed manifest is
  in dev as of 2026-10-30: `SHA256SUMS.sig` (§8.2); the rest of the item stays open.*
- Arch as solid as Fedora, and packages published: the live tests run on a schedule in disposable
  containers; COPR and AUR publish from the release workflow (G21). M.
- First run teaches itself, and the TUI degrades: the screenshot harness asserts 80×24, 16 colors,
  mono and no-reeved, and a fresh-home walk-through is part of each release (G24). M.

**Reviewed and not planned**

- Retrying a finding occurrence skipped for cooldown (noted under G09): each episode is handled
  once, on purpose.
- Local transcript redaction and atomic budget reservations, as above.

