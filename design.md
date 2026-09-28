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
| `reeve-cli` | bin `reeve` | TUI by default; subcommands `receipts`, `undo`, `memory`, `orders`, `daemon`, `key`, `doctor`, `askpass` |
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
  Chat Completions with SSE streaming. Local endpoints are priced at $0.
- **Prices:** pulled live from each connection's `GET /models`. For OpenRouter that is per-token
  prompt, completion, and cache pricing. They are refreshed at startup and when the model picker
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
  variable (`OPENROUTER_API_KEY`, `OPENAI_API_KEY`). The `keys/` directory is on the policy's
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
- Removing the running kernel, the last installed kernel, or a protected package (`glibc`, `systemd`, `dnf`/`pacman`, `sudo`, `kernel-core`), plus dependency-ignoring forms (`rpm -e --nodeps`, `pacman -Rdd`)
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

Reeve always runs as you. A root action runs `sudo -A` with `SUDO_ASKPASS=reeve askpass`.
The askpass helper connects to the running TUI over a private socket
(`$XDG_RUNTIME_DIR/reeve/askpass-<pid>.sock`, 0600), and the TUI shows a masked password modal.
The password is never sent to the model, never logged, and never kept after sudo reads it.
sudo's own timestamp cache applies as usual.

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
   post-image before reverting (no clobbering later edits).
3. **Session reports.** At session end, `sessions/<id>/report.md` records what was asked, what changed
   (with receipt seqs), what failed, what it cost, and what Reeve learned.
4. **Snapshots.** On btrfs with snapper, a T2 action is wrapped in a `snapper create --type pre/post`
   pair, described `reeve #<seq>: <why>`. Several T2 actions within one approved plan share a
   single pair. If snapper isn't configured, Reeve offers to set up a root config once. It never
   does so silently.

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
   undo record (most shell commands) is listed as still in place.
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
   new or updated runbook with its outcome, and preference candidates.
5. **Review.** Facts and runbooks are written with provenance and show as "new" in the Memory
   panel. Preferences wait for your yes.

Runbook confidence falls with failures and resets for review after a major OS upgrade.

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
  - packages from rpm's `INSTALLTIME` (Fedora) or `/var/log/pacman.log` (Arch);
  - removals and unit changes from the daily snapshot reeved writes to
    `observer/state/<day>.json` (60 days kept);
  - `/etc` from file modification times, matched against receipts to mark Reeve's own edits;
  - a pending reboot from the newest `kernel-core` against `uname -r`, or on Arch, the running
    kernel's missing modules.
- **Findings, receipts, spend, memory:** the usual stores.
- **Headlines:** rules over the above (a disk full within 90 days, swap full ≥ 50% of the time,
  temperature past `temp_warn`, critical findings, fixes verified or rolled back, a pending
  reboot).

Everything shown is escaped. Log lines and finding text are data.

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
- **Arch next:** `pacman`, AUR helper detection (`paru`/`yay`), the `snap-pac` snapper integration, and
  Omarchy theme detection.

---

## 11. The TUI: mission control

```
╭─ ◆ REEVE ── nexus · Fedora 44 · up 3d 4h ──────── observer ● · 2 findings · TIERED ─╮
│ ╭─ conversation ─────────────────────────────╮ ╭─ system ─────────────────────────╮ │
│ │ you   clean up old kernels                 │ │ cpu  ▁▂▅▃▂▁▂▃  18%  load 0.92     │ │
│ │                                            │ │ mem  ███████░░░░  9.1 / 32 G      │ │
│ │ ◆ reeve  Three kernels you no longer boot: │ │ /    ████████░░  71%  +0.4G/day   │ │
│ │   ╭─ T2 · pkg_remove ─────────────────╮    │ │ /boot ██████████ 94%  ⚠           │ │
│ │   │ kernel-6.9.4  kernel-6.9.7  …     │    │ │ temp 52°C   net ↓1.2M ↑80K        │ │
│ │   │ frees 612 MB · snapshot first     │    │ ╰──────────────────────────────────╯ │
│ │   │  ⏎ approve   e explain   esc deny │    │ ╭─ spend ──────────────────────────╮ │
│ │   ╰───────────────────────────────────╯    │ │ session $0.0142  ▁▂▂▅  12.4k tok  │ │
│ │                                            │ │ today   $0.31 / $2.00  ██░░░░░    │ │
│ │                                            │ │ month   $4.12 / $30   █░░░░░░     │ │
│ │                                            │ │ cache 61% · sonnet-5 · openrouter │ │
│ │                                            │ ╰──────────────────────────────────╯ │
│ │                                            │ ╭─ receipts ───────────────────────╮ │
│ │                                            │ │ #1041 ✓ T0 logs_query  kernel     │ │
│ │                                            │ │ #1040 ✓ T1 fs_edit ~/.bashrc  ↶   │ │
│ │                                            │ │ #1039 ✓ T2 svc restart bluetooth  │ │
│ ╰────────────────────────────────────────────╯ ╰──────────────────────────────────╯ │
│ ❯ _                                                                                 │
╰─ ⏎ send · ^y yolo · F2 receipts · F3 memory · F4 findings · F5 orders · F6 spend ───╯
```

**Layout:**
- Wide (≥ 140 columns): conversation on the left (~60%), and a stacked right rail with System,
  Spend, and Receipts.
- Medium: the rail narrows to compact rows.
- Narrow (< 100 columns): the rail becomes a single tabbed panel (`^b` cycles).

**Views:**
- F1 Chat
- F2 Receipts (browse, filter, verify, `u` undo)
- F3 Memory (four layers, "new" markers, edit, delete)
- F4 Findings and Proposals
- F5 Standing orders
- F6 Spend (by day, model, and session; caps)

**Look.** It should not read like another dull coding harness:
- Truecolor gradient header and accents.
- Rounded borders.
- Braille sparklines and charts (ratatui `Chart`/`Sparkline`).
- Smooth gauge fills and an animated "thinking" glyph.
- Risk tiers carry color throughout: T0 slate, T1 teal, T2 amber, T3 red.
- The approval card pulses gently while it waits.
- YOLO recolors the frame and puts a badge in the header.

**Themes:**
- Default **"Brass"**: deep navy ground, brass and amber accents, teal secondary.
- Themes are TOML in `~/.reeve/themes/`. A partial theme fills in from the default, as in Ryter.
- Later: import the palette from the active Omarchy, pywal, or KDE color scheme.
- Everything degrades to 256, 16, or mono colors, with `NO_COLOR` respected.

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
  findings/  proposals/  orders/*.toml
  observer/metrics/YYYY-MM-DD.jsonl
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
| **M6** | Arch | pacman/AUR, snap-pac, Omarchy theme import. |
