# Decisions

Why, not what. Newest first. Each entry: Decision / Chosen vs rejected / Why / Where / Residual risk.

### 2026-09-27: Mask before it leaves; ask OpenRouter not to keep it
- **Decision:**
  - A `MaskingProvider` wraps every provider. It replaces secrets, emails, public IPs, and the user and host names with stable placeholders (`standard`, the chat default). `strict` also masks private IPs, MACs, and UUIDs, and is the default for the drafter and standing orders.
  - Placeholders are restored locally in streamed text and in tool call arguments.
  - A secret's placeholder is restored only into `fs_write`/`fs_edit` content. Anywhere else, and for any placeholder Reeve never issued, the call is refused before it runs.
  - Secrets stay masked in the chat.
  - OpenRouter requests carry `provider.data_collection = "deny"` (on by default) and optionally `provider.zdr = true`. A no-provider error points to `/privacy`.
  - Local connections are never masked.
- **Chosen vs rejected:**
  - Rejected "use a local model" as the privacy answer: the owner uses OpenRouter and wants it to stay the default.
  - Rejected removing secrets outright: the model then can't edit a file that holds one, and it would guess at what was there.
  - Rejected restoring secrets anywhere: that makes masking a way to use a key without seeing it, which is exactly what a prompt injection in a log would want.
  - Rejected a model-based filter: it would cost money, and it would still have to see the data.
- **Why:** An operator agent reads dotfiles, journals, and configs. Masking locally means a leak needs a secret format Reeve doesn't know. Routing means what does leave isn't used for training.
- **Where:** `reeve-core/src/privacy.rs`, `agent.rs` (wrapping, `restore_call`, `PRIVACY_NOTE`), `llm/http.rs` (`provider` routing, `explain`), `config.rs` (`PrivacyConfig`), `reeve-tui` (`/privacy`, the header count)
- **Residual risks:**
  - It's pattern matching: an unknown token format, or a password in free text, gets through.
  - A file that really contains the text `<user>` or `<ip1>` would have it replaced when written back.
  - `no_training` defaults on, so a model whose only providers collect data stops working until it's turned off in `/privacy`. The error says so.

### 2026-09-27: Fixes prove they worked, or are rolled back
- **Decision:**
  - A fix is a transaction. `change_begin` declares the goal and checks before anything changes; tagged changes follow; `change_commit` runs the checks.
  - If any check fails, Reeve undoes the tagged changes newest first. Each undo gets its own receipt, approved by `txn:<id>`.
  - Checks come from a fixed set Reeve runs itself: unit active, journal quiet since the last change, disk below a threshold, or a T0 command (no sudo) with optional expected text.
  - A transaction still open at the end of a turn is committed then, and the model is told the result.
  - In standing orders, a rollback ends the run `rolled_back` and leaves a proposal, which is worth a popup.
- **Chosen vs rejected:**
  - Rejected the model reporting whether its fix worked: only Reeve's own check results count.
  - Rejected free-form check scripts: any check command must classify T0.
  - Rejected rolling back with snapper: undoing a whole snapshot pair would revert unrelated changes. Snapper pairs stay a manual last resort.
  - Rejected refusing changes that have no undo record: restarting a service through `shell` is normal. Instead the card warns, and the result lists what's still in place.
- **Why:** Other tools report that they ran a command. Reeve should report that the problem is gone, and put things back when it isn't. The receipts, undo store, and unit/package undo already existed, so a rollback is just a series of undos.
- **Where:** `reeve-core/src/txn.rs`, `agent.rs` (`begin`, `commit`, auto-commit in `turn_inner`), `receipts.rs` (`txn`), `tools/mod.rs` (`undo_receipt_by`, `shell_exec_status`, specs), `reeve-tui/src/cards.rs`, `reeve-observer/src/orders.rs`
- **Residual risks:**
  - The model chooses the checks, so a weak check (one that passes anyway) proves little. The approval card shows them so the owner can judge.
  - A rollback of root changes needs the password again if the 5-minute remember has expired. In an order, a rollback needs exact sudoers lines like everything else, or it fails and is reported.
  - Starting a new session with a transaction open drops it: its changes keep their receipts, but it's never checked.

### 2026-09-27: The observer reports; a popup means a fix is ready
- **Decision:**
  - Findings no longer pop up. They wait in `/findings`, the header badge, and the agent's prompt.
  - A desktop notification is sent when there's something to decide: the drafter wrote a proposal ("Reeve has a fix ready: …"), or a standing order stopped at its scope and left one.
  - A standing order that simply finished is quiet unless it sets `notify = "after"`, and `never` is now the default.
  - `[observer] notify_findings = true` opts back into finding popups, paced by `notify_every_minutes`.
- **Chosen vs rejected:**
  - Rejected finding popups by default, even paced: a watcher that interrupts for everything teaches you to ignore it.
  - Rejected no popups at all: a drafted fix, or an order waiting on the owner, is the moment a person is needed.
- **Why:** The owner's call: "the watcher should watch and report; the popup should come when the observer writes a proposed solution".
- **Where:** `reeve-observer/src/daemon.rs` (`flush_notifications`, the drafter and order branches), `reeve-core/src/config.rs` (`notify_findings`), `orders.rs` (`notify` default)
- **Residual risk:** With the drafter off, a critical finding (a disk about to fill) has no popup. It shows the next time Reeve is opened.

### 2026-09-27: A crashing program is one finding; popups are paced
- **Decision:**
  - Critical journal messages that report a crash (`dumped core`, abrt's `crashed in`) become `app-crash:<program>`, a warning. The program is taken from the stack trace when there is one, since `comm` is often just `main`.
  - Sudo's auth failures become one `auth-failure:sudo` finding.
  - Other critical messages are grouped by unit template and their first line.
  - Popups: at most one every `notify_every_minutes` (default 5), and everything new in between shares it.
  - The agent sees the open findings in every prompt, and has a T0 `findings` tool for details.
  - The test that runs a real `sudo` is opt-in (`REEVE_TEST_SUDO=1`).
- **Chosen vs rejected:** Rejected keeping coredumps as critical: an app crashing is worth knowing about, not an emergency.
- **Why:** On the owner's machine, Mailspring's `mailsync` segfaults every few minutes. Every crash writes "dumped core" at critical priority under a new `systemd-coredump@<instance>` unit, so v0.1.0 raised a new finding, and a popup, per crash. Meanwhile the chat agent couldn't see any of it and said everything was fine. The sudo test had also put two auth-failure alerts in the owner's journal.
- **Where:** `reeve-observer/src/detect.rs` (`journal_critical`, `crashed_program`), `daemon.rs` (`flush_notifications`), `reeve-core/src/tools/obs.rs`, `agent.rs` (prompt), `config.rs` (`notify_every_minutes`)
- **Residual risk:** Findings made under v0.1.0's per-crash ids stay open until they go stale (24 h), unless dismissed.

### 2026-09-27: Standing orders: every part of a command must be allowed
- **Decision:**
  - An order's run is an ordinary agent turn with an approver that allows T0, and T1/T2 only inside the order's scope. It never allows T3.
  - Commands are split into their simple commands (the policy's own tokenizer), and each part must match a scope glob or be a pure read on its own.
  - `*` in a command glob stays within one word.
  - Write redirects and `$(…)` are refused for unattended runs.
  - A refusal tells the model to stop and say what it needs. The run ends `blocked`, and its report becomes a proposal on the finding (or an `order-blocked:<id>` finding).
  - Each occurrence of a finding triggers an order at most once, and runs per day and a cooldown bound it.
  - Order files and Reeve's `config.toml`/`settings.toml` are floor files for tools.
- **Chosen vs rejected:**
  - Rejected whole-line globs: `sudo journalctl --vacuum-size=*` matched `… && sudo dnf remove x` (a test caught it before it shipped).
  - Rejected wildcard sudoers rules from `reeve orders sudoers`: a `*` in a sudoers argument allows more than it looks like.
  - Rejected letting an order run T3 with a flag: the floor is where "nobody is watching" is least acceptable.
  - Rejected T1 protection for order files: a YOLO session could otherwise approve a model writing itself an order.
- **Why:** This is the one way Reeve acts without the owner. The user asked for scheduled autonomous work that can act on findings; an order's scope and budget have to hold even against a model misled by log text.
- **Where:** `reeve-core/src/orders.rs` (`Scope::allows`, `command_glob`, `Schedule`), `policy/shell.rs` (`simple_commands`), `policy/paths.rs`, `reeve-observer/src/orders.rs` (`OrderApprover`, `run`), `daemon.rs` (`orders_tick`), `reeve-cli` (`orders`), `reeve-tui` (`/orders`)
- **Residual risk:**
  - A scope glob can still be written too broadly (`sudo *`). The panel shows each order's scope, but it doesn't judge it.
  - A run blocked partway leaves the changes made before the block. Each has its own receipt and undo.
  - Root in orders depends on sudoers rules the owner writes, and a careless one is a standing grant outside Reeve's control.

### 2026-09-27: One archive, three ways in: installer, RPM, PKGBUILD
- **Decision:**
  - Releases build static musl binaries for x86_64 and aarch64 and package them once (`packaging/dist.sh`, used by CI and by hand) with the user unit, the installer, and docs.
  - From that: `install.sh` (checksum-verified; `/usr/local` by default, `--user` to stay in `~`), a prebuilt RPM (`cargo generate-rpm`), and an Arch `reeve-bin` PKGBUILD with the checksums filled in.
  - A source spec (Fedora, offline with a vendored-crates tarball) and a source PKGBUILD are in `packaging/`.
  - The unit ships in `/usr/lib/systemd/user` (packages) or `/usr/local/lib/systemd/user` (installer), and `reeve daemon install` enables that one instead of writing its own.
  - Releases are drafts: a person publishes them.
- **Chosen vs rejected:**
  - Rejected glibc binaries: they tie the release to the build machine's glibc, and static musl runs on any distribution.
  - Rejected auto-enabling the service from package scriptlets for every user: Fedora's presets decide that, and the owner turns it on.
  - Rejected `--user` as the installer's default: `sudo reeve root` runs the binary as root, so by default it should live where only root can write (`reeve doctor` warns otherwise).
  - Rejected installing without checksums when the SHA256SUMS download fails: the installer refuses.
- **Why:** The user wants one script that installs the binary and the reeved service together, and proper packages before running the observer on their machine.
- **Where:** `install.sh`, `packaging/`, `.github/workflows/{ci,release}.yml`, `crates/reeve-cli/Cargo.toml` (`generate-rpm`), `reeve-observer/src/service.rs` (`packaged_unit`), `reeve-cli` (`doctor`)
- **Residual risk:**
  - The prebuilt artifacts aren't signed. The checksums come from the same release, so they protect against corruption, not against a compromised release.
  - The source PKGBUILD's checksum is `SKIP` until it's published to the AUR with a real tarball.

### 2026-09-27: reeved shares files with the TUI, and is the same binary
- **Decision:**
  - The observer writes findings (one JSON file each) and a heartbeat file. The TUI polls them every second, and the owner's acknowledge and dismiss are written to the same files.
  - The service's `ExecStart` is `reeve daemon run`, and one instance runs at a time (a lock file).
  - Failed units are grouped by template (`drkonqi-coredump-processor@.service`), so a crashing app is one finding, not one per crash.
  - Journal spikes are measured against each unit's learned rate: 10× normal, and at least 30 in 10 minutes.
- **Chosen vs rejected:**
  - Rejected the socket in the original design: the TUI would need reconnect logic and still read the files for history. Files work with `reeved` down, and can be inspected with `cat`.
  - Rejected a separate `reeved` binary: two binaries to install and keep at the same version.
  - Rejected one finding per failed unit instance: KDE's crash processor makes a new unit per crash.
- **Why:** The observer must be simple enough to trust running all the time.
- **Where:** `reeve-core/src/findings.rs`, `reeve-observer/src/{daemon,detect,baselines,journal,notify,drafter,service}.rs`, `reeve-cli` (`daemon`), `reeve-tui/src/run.rs` (`poll_observer`)
- **Residual risk:**
  - The TUI and `reeved` can both write a finding file at the same moment, and the last write wins (an acknowledge could be lost to a tick). Writes are atomic, so a file is never torn.
  - The service runs the binary wherever `reeve daemon install` found it. After a rebuild in `target/`, restart it.

### 2026-09-27: Background drafting is a separate, budgeted role, off by default
- **Decision:**
  - `reeved` watches with rules and no model.
  - Pre-drafting fixes for findings is done by an opt-in "drafter" role with its own connection, model, daily cap, per-draft cap, and draft count (`[observer.drafter]`, or `/observer` in the TUI).
  - It uses T0 tools only.
  - Its spend is ledgered under its own role and also counts toward the global caps.
- **Chosen vs rejected:**
  - Rejected drafting on by default: a noisy journal could spend money while nobody is at the machine.
  - Rejected sharing the main session's budget: the owner can't tell or cap what the background costs.
- **Why:** The user wants a way to turn it on, "with its own budget like an auditor" (Ryter's auditor seat).
- **Where:** `reeve-observer/src/drafter.rs`, `reeve-tui` (`/observer`), `reeve-core/src/ledger.rs` (`role_today`)
- **Residual risk:** Findings are built from journal text any process can write, and the drafter reads that text. With T0-only tools, the worst outcome is a misleading proposal that the owner still has to approve.

### 2026-09-27: Memory is Markdown notes; the owner has the last word
- **Decision:**
  - Four layers under `~/.reeve/memory/`, one Markdown file per note with a short header (source, observed, confidence, status, OS, runbook counts, rule).
  - Search is keyword and tag scoring. Runbooks are weighted by their track record, and notes from another OS version rank lower.
  - A compact profile (preferences in effect, fact one-liners, a runbook count) goes into every system prompt, capped at about 3.5k characters.
  - Writers:
    - the read-only survey (weekly)
    - the model through `memory_write` (T0, receipted)
    - reflection (one model call per finished session, or catch-up after a restart)
    - the owner
  - The owner's notes, and survey facts the owner edited, are never rewritten by the model: a new note is added instead.
  - Preferences from the model or reflection stay `pending` until the owner accepts them.
  - `deny-path` and `deny-command` rules from accepted preferences are enforced in `tools::prepare`, for every tool and every tier except reads.
- **Chosen vs rejected:**
  - Rejected a vector store: opaque, and nobody can edit an embedding.
  - Rejected putting all memory in the prompt: it grows without bound, so the rest is found through `memory_search`.
  - Rejected auto-accepting preferences: a prompt-injected "preference" could otherwise disable a safeguard.
  - Rejected reflecting only on quit: quitting would hang on a model call, and a crash would lose the session's lessons.
- **Why:** Reeve should get better on this machine over time, and the owner must be able to see and correct what it learned.
- **Where:** `reeve-core/src/memory/` (`mod.rs`, `survey.rs`, `reflect.rs`), `tools/mem.rs`, `tools/mod.rs` (`apply_rules`), `agent.rs` (profile, `reflect`, `unreflected`), `reeve-tui/src/overlay.rs` (`MemoryPanel`)
- **Residual risk:**
  - Tool output can contain text written to mislead (a log line). A fact or runbook learned from it is marked `new` and shown, but it's in use until the owner retires it.
  - "Never store secrets" is an instruction to the model, not a filter. Memory files are 0600, but their text is sent to the model provider with every prompt.
  - Reflection costs one model call per session with actions.

### 2026-09-27: The sudo password is typed into Reeve, and only answers while armed
- **Decision:**
  - Root commands run with `~/.reeve/bin` first on `PATH`. A `sudo` wrapper there adds `-A`, and `SUDO_ASKPASS` points at `reeve-askpass` (a link to this binary).
  - The helper asks the running TUI over `$XDG_RUNTIME_DIR/reeve/askpass-*.sock` (0600, in a 0700 dir). It must present a per-session token.
  - The socket answers only while an approved root action is running (`Askpass::arm`). Otherwise it says no at once.
  - The password panel remembers the password for 5 minutes in memory by default (`tab` turns that off). A second request within 15 s of a remembered answer means it was wrong: the cache is dropped and the panel shown.
- **Chosen vs rejected:**
  - Rejected running Reeve as root, and a root daemon (see the earlier entry).
  - Rejected sudo's own timestamp cache: with no terminal, the record is keyed on the parent process, so every command would ask again.
  - Rejected a remembered password that never expires, or one written to disk.
  - Rejected answering any request on the socket: a stray same-user process could then trigger password prompts whenever it liked.
- **Why:** The user wants root, one approved action at a time, with the password never reaching the model.
- **Where:** `reeve-core/src/sudo.rs`, `tools/shell.rs` (`run_command`), `reeve-tui/src/run.rs` (`password_asked`), `reeve-cli/src/main.rs` (argv[0] check)
- **Residual risk:**
  - Anything running as the user can read the token from a root command's environment while it runs, and ask for a password during that window. It still gets a prompt the person sees.
  - A remembered password lives in process memory for up to 5 minutes.
  - A cancelled prompt may count as a failed login for `pam_faillock`, where that's enabled.

### 2026-09-27: Root files through `sudo reeve root`, changed in place
- **Decision:**
  - Files you can't write go through `sudo <reeve> root`: one JSON operation on stdin (write, edit, delete, revert), one reply on stdout.
  - Running as root, it snapshots into `/var/lib/reeve/undo` and writes in place, so the inode keeps its owner, mode, and SELinux label. New files get `restorecon`.
  - `/etc/sudoers*` and `/etc/fstab` must pass `visudo -cf` / `findmnt --verify` first.
- **Chosen vs rejected:**
  - Rejected `sudo tee` plus separate `sudo cat` for undo copies: several password prompts per edit, and the copies would sit in the user's home.
  - Rejected temp file plus rename: it changes owner and label unless every attribute is copied back by hand.
- **Why:** Most sysadmin fixes are edits to root-owned config, and they need the same undo promise as anything else.
- **Where:** `reeve-core/src/root.rs`, `tools/fs.rs` (`needs_root`, `root_exec`, `root_revert`)
- **Residual risk:**
  - sudo runs this binary as root. If the binary sits in a user-writable directory (a cargo `target/`), whoever can replace it gets root at the next approved edit. That is no more than the user's own sudo rights, but install Reeve somewhere root-owned for daily use.
  - An in-place write isn't atomic: a crash mid-write can leave a partial file. The undo copy is taken first.

### 2026-09-27: System tools are commands the shell policy already understands
- **Decision:**
  - `pkg_*`, `svc_*`, `logs_query`, `proc_*`, and `sys_info` build commands with validated names and quoted values. Changes are classified by `policy::shell` as if typed, so `pkg_remove systemd` meets the same floor as `dnf remove systemd`.
  - The read-only ones are T0 by construction.
  - Package changes record the dnf transaction id (undo is `dnf history undo`). `svc_control` records the unit's enable and active state first.
- **Chosen vs rejected:**
  - Rejected a separate tier table for structured tools: two policies drift apart.
  - Rejected parsing dnf's output to find the transaction: comparing `history list` before and after is simpler and survives format changes.
- **Why:** Structured tools give better approval cards and real undo, and the classifier stays the one source of truth.
- **Where:** `reeve-core/src/tools/sys.rs`, `distro.rs`
- **Residual risk:**
  - A package transaction that runs at the same time (a GNOME Software update) can be recorded as Reeve's.
  - Arch has no transaction undo; pacman changes carry no undo.

### 2026-09-27: Snapshots only where snapper is already set up
- **Decision:**
  - When snapper has a config for `/`, every approved root action is wrapped in `snapper create --type pre/post`, described "reeve: <action>", with the number cleanup algorithm.
  - The pair goes in the receipt and the session report, with the `undochange` command.
  - Without a config, nothing is taken, and Reeve never creates one unasked.
- **Chosen vs rejected:**
  - Rejected setting snapper up automatically: it decides what gets snapshotted and kept on the user's disk.
  - Rejected one pair per turn: a pair per action points at exactly what changed.
- **Why:** Some root changes can't be undone by Reeve itself (a shell command, a post-install script). A snapshot can undo them.
- **Where:** `reeve-core/src/snapshots.rs`, `agent.rs` (`run_call`)
- **Residual risk:** On this machine snapper is installed but has no config, so today nothing is snapshotted until the owner sets one up.

### 2026-09-27: Commands run in their own session, with no terminal
- **Decision:**
  - `shell` runs `setsid bash --noprofile --norc -c …` with stdin closed.
  - Reeve's API-key variables are removed from the environment, pagers are set to `cat`, and editors to `false`.
  - The command leads its own process group. A guard kills the whole group on timeout, and when the turn is stopped mid-command.
  - Until M2's askpass, `sudo` fails at once ("a terminal is required"), and Reeve tells the model to hand the owner the exact command instead.
- **Chosen vs rejected:**
  - Rejected running commands on Reeve's own terminal: `sudo`, `ssh`, and pagers would draw over the TUI or wait forever for a key.
  - Rejected `pre_exec(setsid)`: it needs `unsafe`, which the workspace forbids. The `setsid` binary execs in place (Reeve's child isn't a group leader), so the child's pid is the group to kill.
  - Rejected killing only the direct child: `(sleep 3; touch x) & sleep 30` left the background job running after a timeout (a test proves it's gone now).
- **Why:** An operator's commands are exactly the ones that prompt, page, or linger.
- **Where:** `reeve-core/src/tools/shell.rs` (`run`, `GroupGuard`)
- **Residual risk:**
  - A command that double-forks into a new session of its own (a daemon) escapes the group kill.
  - `setsid` must be installed (util-linux, standard on Fedora and Arch).

### 2026-09-27: What "allow for this session" covers
- **Decision:**
  - Only T1 actions can be allowed for a session.
  - For `shell`, the rule is the exact command line. For file tools, it's the tool plus the directory.
  - T2 and T3 ask every time, and YOLO never answers T3; that is checked in core, not in the TUI.
- **Chosen vs rejected:**
  - Rejected allowing a program name (`rm`): one yes would cover every later `rm`.
  - Rejected letting the approver enforce the floor: a second front end (the daemon, a future web UI) could forget. `Agent::approve` never asks YOLO about T3.
- **Why:** Repeating a small edit in the same folder shouldn't mean ten prompts, and a broad yes shouldn't leak into unrelated work.
- **Where:** `reeve-core/src/agent.rs` (`approve`), `tools/fs.rs` (`rule_for`), `tools/shell.rs` (`plan`)
- **Residual risk:** A directory rule covers any file in that directory, including ones the owner didn't picture when saying yes.

### 2026-09-27: Undo keeps both sides, and never clobbers
- **Decision:**
  - Every file change stores the file's before and after in the content-addressed undo store.
  - Reverting first checks every path still matches the after. If anything changed since, nothing is reverted.
  - An undo is itself an action with a receipt (`undoes: N`) whose own undo is the redo.
  - File tools refuse a change they can't snapshot: over 64 MB, or a folder of more than 5000 files.
- **Chosen vs rejected:**
  - Rejected keeping only the before: without the after, Reeve can't tell whether someone edited the file since, and an undo would silently throw that edit away.
  - Rejected changing a file without a copy "just this once": the promise is that file changes can be undone.
  - Rejected marking the original receipt as undone: receipts are append-only, so the undo is a new receipt that points back.
- **Why:** "Receipts for everything" is only worth something if the receipt can put things back.
- **Where:** `reeve-core/src/undo.rs`, `receipts.rs` (`undo`), `reeve-tui/src/run.rs` (`Action::Undo`)
- **Residual risk:**
  - `shell` changes have no undo: the receipt records the command and a hash of its output, not what it touched.
  - The undo store grows without bound until M3 adds pruning.

### 2026-09-27: Keys and models are set up inside the TUI
- **Decision:**
  - `/providers` lists every connection with where its key comes from (`keys/<name>`, `$VAR`, `config.toml`), and lets you set, check, or forget a key, add a connection, and pick a model.
  - The TUI's choices go to `settings.toml`, which is layered over `config.toml`.
  - Keys go only to `keys/<name>` (0600). The masked entry never draws the key, and its `Debug` prints only the length.
- **Chosen vs rejected:**
  - Rejected writing `config.toml` from the TUI: it's the user's file, comments and all.
  - Rejected storing keys in `settings.toml`: it would put a plaintext key in a file the user may copy around. `Settings::save` strips `api_key` even if one got in.
  - Rejected a key check that only lists models: OpenRouter's catalog is public, so listing succeeds with any key. `GET /key` actually authenticates, and it reports usage against the limit.
- **Why:** The user wants to open the binary, add the key through `/providers`, and start testing.
- **Where:** `reeve-core/src/settings.rs`, `config.rs` (`secret_source`, `remove_secret_at`), `llm/http.rs` (`verify`), `reeve-tui/src/overlay.rs`, `panels.rs`, `run.rs` (`App::perform`)
- **Residual risk:**
  - A key being typed sits in memory as a plain `String` until it is saved or the panel closes.
  - A key rejected by the check stays stored, so the user can fix a typo. The connection still switches, and the first turn then fails with the provider's error.

### 2026-09-27: Chat Completions only, with real cache prices
- **Decision:**
  - The copied provider layer keeps only Chat Completions.
  - `Usage` counts cache writes as well as cache reads.
  - `Rates` carries OpenRouter's own `input_cache_read` and `input_cache_write` prices.
  - OpenRouter requests ask for `usage.include`, so every call comes back with the provider's real cost.
  - Router pseudo-prices (`"-1"`) are treated as unknown.
- **Chosen vs rejected:**
  - Rejected keeping Ryter's Responses and Messages backends. OpenRouter and every OpenAI-compatible server (OpenAI, Groq, vLLM, llama.cpp, Ollama, LM Studio) speak Chat Completions, and the other two were ~500 lines to maintain for no connection Reeve offers.
  - Rejected pricing cached input at the input rate, as Ryter does. On Claude models that overstates a cache read by 10× and understates a cache write by 25%.
- **Why:** The user wants Reeve's cost tracking to "calculate nicely". Most of an agent loop's input is cache reads, so their price dominates the bill.
- **Where:** `reeve-core/src/spend.rs` (`Rates::cost`), `llm/parse.rs` (`usage_from`), `llm/http.rs` (`parse_models_json`, `body`)
- **Residual risk:**
  - A provider that reports neither a cost nor cache prices is billed at the plain input rate for cached tokens, which is an overestimate.
  - Adding native Anthropic or OpenAI Responses support later means porting Ryter's backends back in.

### 2026-09-26: Permissions follow risk, not a folder
- **Decision:**
  - Every action is classified into T0 Observe, T1 User change, T2 System change, or T3 Floor (`design.md` §5).
  - T0 runs. T1 asks, and can be allowed for the session or by pattern. T2 asks every time.
  - T3 asks with a typed `yes` in **every** mode.
  - YOLO auto-approves T0–T2 and still takes receipts and snapshots.
- **Chosen vs rejected:**
  - Rejected Ryter's "inside the project vs outside" gate. An operator's whole job is outside any project.
  - Rejected a YOLO with no floor. The user asked for "the basic safeguards that would save system destruction", and a model that runs `mkfs` on the wrong disk can't be undone by a receipt.
  - Rejected hard-blocking floor actions. Formatting a USB stick is legitimate, so the floor asks instead of refusing.
- **Why:** Reeve is not bound to its launch directory, so the only meaningful boundary is what an action can break.
- **Where:** `reeve-core/src/policy/` (M1)
- **Residual risk:**
  - The shell classifier reads the command's form, not its runtime behavior. A script file (`bash ./x.sh`) is classified as the tier of running an unknown program (T1 or T2), not by what is inside it.
  - A floor list is never complete.

### 2026-09-26: Root through `sudo -A`, one action at a time
- **Decision:** Reeve runs as the user. Root steps run `sudo -A` with Reeve's own askpass helper, which asks the TUI for the password over a private socket. `reeved` gets root only through narrow sudoers drop-ins the user installs per standing order.
- **Chosen vs rejected:**
  - Rejected running the harness as root: one bad tool call could then touch anything, and every file it wrote would be root-owned.
  - Rejected a privileged helper daemon for v1: more secure in the long run, but a large surface to build before anything useful works.
- **Why:** Per-action escalation keeps each root step visible, approved, and receipted, and the agent process never holds root.
- **Where:** `reeve-core/src/tools/sudo.rs`, `reeve-cli` `askpass` (M2)
- **Residual risk:**
  - sudo's timestamp cache means a second root command within the timeout doesn't re-prompt for the password. The approval card still gates it.

### 2026-09-26: Copy from Ryter, share nothing
- **Decision:** `llm/` and `spend.rs` are copied from Ryter as a starting point and adapted. There is no shared crate.
- **Chosen vs rejected:** Rejected a shared `zypher-llm` crate. It would couple release cycles of two young projects with different needs (Reeve has a daemon, global ledgers, and no crew).
- **Why:** User decision. Extraction stays possible later if both stabilize.
- **Where:** `reeve-core/src/llm/`, `reeve-core/src/spend.rs`
- **Residual risk:** Fixes to shared logic (SSE edge cases, price parsing) have to be ported by hand in both directions.

### 2026-09-26: Receipts are a hash chain, and root pre-images stay root-owned
- **Decision:**
  - Every action appends a receipt whose hash covers the previous receipt's hash.
  - File pre-images are content-addressed. Pre-images of root-owned files are stored in `/var/lib/reeve/undo` (root, 0600), not `~/.reeve`.
- **Chosen vs rejected:**
  - Rejected a plain log: an edit or deleted line would go unnoticed.
  - Rejected signing receipts: a signing key readable by the user adds nothing over the hash chain against the same user.
  - Rejected storing root pre-images in the home directory: that would copy files like `/etc/shadow` into a user-readable tree.
- **Why:** "Receipts for everything" means you can trust the record and act on it.
- **Where:** `reeve-core/src/receipts.rs`, `reeve-core/src/undo.rs` (M1)
- **Residual risk:** The chain is tamper-evident, not tamper-proof. Anyone running as the user can rewrite the whole chain from some point onward. A future option is to anchor the head hash in the journal.

### 2026-09-26: Memory is plain files, and the observer never acts on its own
- **Decision:**
  - Memory is four layers (facts, baselines, runbooks, preferences) stored as plain files with provenance, searched by keyword and tag.
  - `reeved` watches and notifies. It acts only inside the scope of a standing order the user wrote.
- **Chosen vs rejected:**
  - Rejected a vector database: it is opaque and can't be reviewed or edited by hand. Keyword and tag search is enough at this scale.
  - Rejected auto-fixing findings: the user asked for a notification plus a queued proposal, except where scheduled autonomous work covers the finding.
- **Why:** The user wants Reeve to get better by learning how the system runs, and the user must be able to see and correct what it learned.
- **Where:** `reeve-core/src/memory/`, `reeve-observer` (M3, M4)
- **Residual risk:** Findings and journal text are written by arbitrary processes, and a proposal drafted from them can carry injected instructions. Proposals are T0-only to draft and need approval to run, and standing-order scopes are hard allowlists.
