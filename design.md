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

### 8.2 Updates (`reeve update`)

- **Checking:** reeved asks GitHub's latest-release endpoint (user agent `reeve/<version>`, nothing
  else sent). Drafts and pre-releases never count. The answer goes to `observer/update.json`, with
  the version reeved runs, which is the one installed. When reeved isn't running and the answer is
  more than a day old, the TUI asks instead, in the background. `[updates] check = false` stops both.
- **Showing:** the header gets `↑ 0.5.0`, or `restart for 0.5.0` when the new version is installed
  but this window predates it. `/update` and ⌃K say what's out and how to get it. There's no desktop
  notification: a new version isn't a proposed fix.
- **Installing:** `reeve update` works out how this copy got here and does the same again:
  - **install.sh** (a `bin/` under some prefix): download the release tarball and `SHA256SUMS`,
    refuse a mismatch, check that the archive's binary is the expected version, then run that
    release's own `install.sh --from` with flags matching this install. The installer never starts
    reeved; `reeve update` restarts it only when it runs the binary just replaced.
  - **An RPM:** the release RPM, checked the same way, through `sudo dnf upgrade` (or `downgrade`).
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
  and undo puts the old versions back from the package cache with `pacman -U`, then removes what was
  new with `pacman -R`. A version the cache lost is named and the undo refused, never half done.
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

---

## 11. The TUI: the ledger, and tabs for the rest

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
  findings/  proposals/  orders/*.toml
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
  cleanly, and an undo refuses when the target changed since.
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
- [x] Budgets hold across the TUI, the drafter, and standing orders, and a model with no known
  price fails closed while any cap is set.

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

**Not needed for 1.0:** skills, themes beyond Omarchy's, timeshift, and macOS or Windows.
