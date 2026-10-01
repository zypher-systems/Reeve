# Decisions

Why, not what. Newest first. Each entry: Decision / Chosen vs rejected / Why / Where / Residual risk.

### 2026-09-30: Arch (M6): pacman undo from its log, the AUR on the owner's word, Omarchy's theme
- **Decision:**
  - pacman has no transaction ids, so a package action records what pacman logged after the log's size before it (`Undo::Pacman`: name, action, versions). Undo restores the old versions from the package cache (`pacman -U`), then removes what was new (`pacman -R`). A version the cache lost is named and the undo refused before anything runs. A failed command still records what it changed.
  - A package the repos can't satisfy (`pacman -Sp`: a name, a group, or a provided name) is built from the AUR with paru or yay, with no prompts (`--skipreview`, or yay's `--answer* None`), through Reeve's sudo wrapper. The card shows each PKGBUILD as code (`paru|yay -Gp`, 60 lines, with the rest's command) and names any `install=` script. AUR builds are `owner_only`, through `pkg_install` or a raw `paru`/`yay` in `shell`.
  - An empty `pkg_upgrade` stays `pacman -Syu`: AUR packages are listed apart (`aur:`) and rebuilt by name.
  - With snap-pac, Reeve takes no pair around a pacman command and records snap-pac's instead.
  - reeved on Arch: a reboot is pending when `/usr/lib/modules/$(uname -r)` is gone; `arch-audit -u` supplies security updates.
  - `[ui] theme` defaults to `auto`: Omarchy's current `colors.toml`, re-read every second, or Slate off Omarchy. Custom themes use the same file format.
  - Fixed on the way: `Assessment::merge` dropped `owner_only`, so a compound shell command could lose it.
- **Chosen vs rejected:**
  - Rejected rolling back with snapper alone: it needs btrfs and a root config, which many Arch installs don't have, and it rolls back everything else on the disk too.
  - Rejected `pacman -S <name>` to undo a removal: it installs today's version, not the one removed. The cache has the exact build.
  - Rejected letting YOLO cover AUR builds: a PKGBUILD runs arbitrary code as you and its package installs as root, and nobody reviewed it. The owner reads it each time.
  - Rejected `paru -Syu` for system upgrades: it would build every AUR update unattended on one yes.
  - Rejected a one-time theme import as the default: following Omarchy live is what an Omarchy user expects when they switch themes.
  - A name pacman resolves through a provider (pfetch → pfetch-rs) stays a repo install: the reviewed package wins over an AUR build.
- **Why:** M6 makes Arch and Omarchy as safe to hand to Reeve as Fedora: every package change undoable, and nothing from the AUR without the owner reading it.
- **Where:** `reeve-core/src/pacman.rs`, `distro.rs` (`aur_install`, search/info/updates), `tools/sys.rs` (capture, split, revert), `undo.rs` (`Undo::Pacman`), `snapshots.rs` + `agent.rs` (snap-pac), `policy/shell.rs` and `policy/mod.rs` (`owner_only` for helpers, `merge`), `reeve-observer/src/daemon.rs` + `detect.rs` (`kernel_replaced`), `reeve-tui/src/theme.rs` (`Palette`, `from_palette`, `ThemeSource`), `cards.rs` (code lines)
- **Residual risks:**
  - Undo needs the old packages in pacman's cache; `paccache` or `pacman -Sc` can remove them first.
  - Only `-Gp`'s PKGBUILD is shown: an `install=` script and patches in the AUR repo aren't, though the card names the script.
  - Tested live on Arch in a container: pacman, its undo, and yay. paru (its prebuilt package lagged pacman's library) and snap-pac (needs btrfs and snapper) are covered by unit tests only, and the Omarchy theme by its real `colors.toml` files and screenshots, not a running Omarchy.

### 2026-09-30: Reeve checks for releases and updates itself the way it was installed
- **Decision:**
  - reeved asks GitHub's latest-release endpoint a minute after it starts and every 12 hours (`[updates] check`, on by default). The TUI asks when reeved isn't running and the last answer is over a day old. The answer goes to `observer/update.json`, and drafts and pre-releases never count.
  - The TUI shows `↑ 0.5.0` in the header, or `restart for 0.5.0` when the new version is installed but the window predates it. `/update` and ⌃K say what's out and how to get it. No desktop notification.
  - `reeve update` works out how this copy was installed (`update::classify`). Copied by install.sh: download the release tarball and `SHA256SUMS`, refuse a mismatch, check that the archive's binary is the expected version, and run that release's own install.sh with flags matching this install (`--prefix`, `--user`, `--no-service` where no unit was installed, always `--no-start`). An RPM: the release RPM through `sudo dnf upgrade` or `downgrade`. Pacman: the AUR helper. A cargo build or an unknown location: refused, with what to do.
  - It restarts reeved only when the running reeved's `ExecStart` is the binary it replaced.
  - Every attempt leaves a `reeve_update` receipt (T1 for a home install, T2 otherwise), failures included. `--rollback` installs the version the last update replaced, from its own release, checked the same way.
  - What's on disk afterwards decides what happened, not the installer's exit code: whenever the binary's version changed, the version it replaced is recorded for `--rollback`, even if the installer then failed. An AUR helper that changed nothing is a failure, and AUR installs are pointed at the pacman cache instead of `--rollback`.
  - `update.json` is only changed under a lock (`UpdateState::modify`), so reeved's check can't drop the rollback target an update just wrote.
  - The install is classified against the home Reeve uses everywhere (`dirs::home_dir`, which reads passwd when `HOME` is unset); with no home, nothing counts as a home install. An RPM install on image-based Fedora is refused, as the package tools refuse there.
  - install.sh says "updated (old → new)" when a copy was already there, and skips the first-install notes.
- **Chosen vs rejected:**
  - Rejected replacing the binary from Rust: install.sh already knows the layout (binary, unit, `ExecStart`, sudo only where it's needed). Running the new release's own installer keeps one install path, and a release can change its layout without the old binary knowing.
  - Rejected keeping the old binary in `~/.reeve` for rollback: copying a file from a user-writable directory into `/usr/local/bin` would let anything running as the user plant a binary that `sudo reeve root` later runs as root. Rolling back from the release costs a download and keeps the checksum check.
  - Rejected a desktop notification for new releases: popups are only for proposed fixes.
  - Rejected off by default: few would ever see the tag. The check is one request to GitHub every 12 hours, with user agent `reeve/<version>`.
- **Why:** The owner wants to know a new version is out without watching GitHub, and to install it with one command that respects how Reeve got onto the machine.
- **Where:** `reeve-core/src/update.rs`, `reeve-core/src/config.rs` (`[updates]`), `reeve-observer/src/daemon.rs`, `reeve-cli/src/update.rs`, `reeve-cli/src/main.rs` (`ask_for_updates`, `doctor`), `reeve-tui/src/board.rs` (the header), `reeve-tui/src/run.rs` (`/update`, ⌃K), `install.sh`
- **Residual risks:**
  - `SHA256SUMS` comes from the same release as the files. It catches corruption and a bad mirror, not a compromised account or release. Signing releases would close that.
  - The flags `reeve update` passes are now a contract: every future install.sh must accept `--from`, `--prefix`, `--user`, `--no-service`, and `--no-start`.
  - The RPM and AUR paths aren't exercised by tests (they need sudo, or an Arch machine). The install.sh path is tested end to end against a local fake release.

### 2026-09-29: The model writes standing orders, asking the owner each time
- **Decision:**
  - New tools `order_save` (make, or change by id; fields the model leaves out keep their values) and `order_delete`. The model is told to make an order whenever the owner wants something done regularly or whenever something happens.
  - They're T2 with a new `Assessment::owner_only`: the owner is asked every time, and nothing answers for them (YOLO, session and request rules, "yes to the rest of this change", the undoable setting). A broad answer to the card counts only for that card. Unattended runs (orders, the drafter) are refused outright, so an order can't make an order.
  - The card shows the order in plain words (`Order::describe`, `ApprovalRequest::details`) in place of the file's diff: when it runs, what it does, the exact commands and files, the most it spends a run and a day, and notes (sudoers needed, limits that slow the schedule).
  - A new order from the model starts on: approving it is the owner's consent. Its tier defaults to what its commands need (none: T0; sudo: T2; else T1), and its limits to the schedule (`limits_for`: every 30m is 48 a day, 0.45 h apart). Limits the model sets that would slow the schedule are sent back.
  - Saves go through `Orders::write_file` with `update` (edits keep comments) and `Expect` (nothing is overwritten); the receipt carries the undo like any file change.
  - Answer to "reeved or cron?": reeved. It checks due orders every 10 s. Rejected per-order cron jobs or systemd timers: state (runs today, cooldowns, handled findings, budgets) would be split across units, and findings triggers need the observer anyway.
- **Chosen vs rejected:**
  - Reverses "order files are floor files, so the model can't give itself unattended powers" (2026-09-27): the owner wants orders to be a flagship ("people will use it more if you can just tell it to do something every so often"). The owner still decides each one; what changed is that it's one `⏎` on a card that reads in plain words, not a typed yes on a TOML file.
  - Rejected T3 (typed yes): the floor is for what could destroy the system or leak secrets; an order is reviewed, bounded, and undoable, and one keypress on a plain description is the right weight.
  - Rejected T1 or T2 without `owner_only`: YOLO or "yes to the rest" would let a prompt-injected model create persistent automation without the owner ever seeing it.
  - Raw file writes to orders stay T3, and so does `reeve undo` of an order change, so the structured tool is the only easy path and always shows the card.
- **Why:** An order is how Reeve acts like a bot: told once, it keeps doing it. The owner must still see exactly what they're agreeing to.
- **Where:** `reeve-core/src/tools/order.rs`, `tools/mod.rs` (specs, `Plan::details`), `policy/mod.rs` (`owner_only`), `agent.rs` (`approve`, `ask`, unattended refusal, the prompt's "Standing orders"), `orders.rs` (`describe`, `pace_warning`, `limits_for`, `Schedule::words`, `finding_words`, `slug`, `Orders::write_file`/`free_id`), `reeve-tui/src/cards.rs` (details on the card)
- **Residual risks:**
  - A prompt-injected conversation can still propose an order; the defense is the owner reading the card. The card shows commands and paths exactly, and root needs a sudoers rule the owner adds by hand.
  - reeved is a user service without linger: logged out, nothing runs until the next login.
  - An order that watches findings has no schedule to fit limits to; it gets two runs a day, 12 hours apart, unless set.

### 2026-09-29: Standing orders are written in a form
- **Decision:**
  - `n` in F7 orders opens a form instead of a template in `$EDITOR`; `e` edits an order in the same form, and `E` opens the file.
  - The form has five sections (what, when, what it may do, limits, after), a line of help under every field, and a live "What it will do" in plain words, with what's missing, sudoers, and whether reeved is running.
  - Schedules are chosen (every day/week at a time, every few hours or minutes) and findings are ticked by kind in plain words (`disk-full:*` is "a disk is nearly full"). Tools are ticked in groups (`fs_write`, `fs_edit`, `fs_move` are "write, edit, and move files"). Anything the form has no box for (other finding ids, other tools) is kept as it is.
  - Saving a new order writes it with `orders::render` (commented TOML, 0600), never over another file, and checks it with `orders::parse` first. New orders start off.
  - Nothing done to an order is destructive:
    - Saving an existing order is `orders::update`: a `toml_edit` rewrite of only the fields that changed since the form opened, so comments, unknown keys, and changes made in the file meanwhile stay. A field changed both places stops the save once and names it.
    - Every change from the TUI (save, on/off, delete, `$EDITOR`) goes through `Orders::commit`/`record`: the old file goes in the undo store and a receipt (`order_new`, `order_edit`, `order_toggle`, `order_delete`; T3, approved by `user`) records both sides. `u` in Orders or F4 puts it back.
    - Picking another example over typed text asks first, and a form closed with changes is kept until quit.
    - The examples are written once (`.examples` marker), so deleting them sticks.
  - `reeve undo <n>` by the model is judged by what the undo writes: restoring an order or Reeve's config is T3, as writing it would be.
- **Chosen vs rejected:**
  - The owner found three examples and a file with no direction too little to set up automations with.
  - Rejected a step-by-step wizard: one page with sections lets you see the whole order and change any part, and the plain-words summary does the explaining a wizard would.
  - Rejected writing cron syntax: the schedule grammar already avoids it, and choices avoid typing the grammar.
  - The first version re-rendered the whole file on save, dropping comments added by hand, and delete removed the file outright. The owner wants order-making to be a flagship feature and non-destructive, so both were replaced. `toml_edit` was already in the tree (under `toml`).
  - Rejected a trash folder for deleted orders: receipts and the undo store already keep copies, and undo is how every other change comes back.
  - Rejected locking the file while the form is open: an edit in another editor shouldn't block, and a field-level merge keeps both.
- **Why:** An order is the only way Reeve acts unattended, so what it may do has to be easy to set and easy to read back.
- **Where:** `reeve-tui/src/orderform.rs`, `reeve-core/src/orders.rs` (`render`, `update`, `Orders::save`/`commit`/`record`/`delete`/`set_enabled`/`seed_examples_once`, `FINDING_KINDS`, `TOOL_GROUPS`), `policy/shell.rs` (`reeve undo`), `run.rs` (`Action::OrderForm`, `Action::SaveOrder`, `order_changed`, `order_drafts`), `overlay.rs` (keys in the orders panel)
- **Residual risks:**
  - The finding kinds and tool groups are lists kept by hand next to the rules and tools they name. A new rule or tool shows up in the form only when it's added to them; until then it's still kept, as an "other" finding or tool.
  - A changed list (`tools`, `paths`) is written on one line, so a list laid out over several lines by hand loses its layout (not its comments above it) when the form changes it.
  - Drafts live in memory: quitting Reeve with a form closed unsaved loses it.
  - `reeve orders examples` (T0) can still write the examples into an empty orders folder; they're fixed text and off.

### 2026-09-29: Less asking: reads are reads, scratch is free, and a yes can cover more
- **Decision:**
  - The shell classifier reads grammar and programs: `if`/`for`/`case`/functions and builtins are structure; awk and sed programs are parsed, and only writing, piping to a command, or running one asks; `tool --help`/`--version` is T0 for installed programs; Reeve's own read-only commands are T0.
  - Each session has a scratch folder (`~/.reeve/scratch/<session>`, `$REEVE_SCRATCH`) where writes are T0, and creating a new file in /tmp is T0. Both are marked `quiet_write`, so standing orders and the drafter still treat them as writes.
  - "Allow for this session" (`s`) remembers what an action does (`write:~/notes`, `delete:~/Downloads`, `run:flatpak`), not its exact text. An action with a part that can't be named that way falls back to its exact text.
  - New: "yes to the rest of this request" (`a`), T1 only, ends when the turn ends.
  - New setting `[approvals] undoable`, off by default: T1 changes with an undo run without asking.
- **Chosen vs rejected:**
  - The owner asked Reeve to gather installed packages into a Markdown file and pressed a key eight times; the receipts show three of the four asks in that session were reads or scratch files. Fixing precision (what counts as a change) came first; broader yeses second.
  - Rejected making T1 run silently by default: shell changes can't be undone, and the tiers are the product's promise. The undoable setting is opt-in.
  - Rejected keying session yeses by program name alone for known programs: the effect (a folder, a kind of change) is what the owner is agreeing to.
  - Rejected treating all of /tmp as scratch: an existing file there may belong to something running; only new files are free.
- **Why:** A request is one intent; the owner should be asked about the change it makes to their files, not about every read and intermediate file on the way.
- **Where:** `policy/shell.rs` (`assess_awk`, `awk_effects`, `assess_sed`, `sed_effects`, keywords, builtins, `RUNS_OTHERS`, `assess_reeve`), `policy/mod.rs` (`Assessment::keys`, `session_keys`, `quiet_write`, `describe_key`, `write`), `policy/paths.rs` (`PathClass::Scratch`, `show_dir`), `scratch.rs`, `agent.rs` (`approve`, `Decision::AllowTurn`, `turn_allowed`), `orders.rs` (`quiet_write` isn't a read), TUI keys `a`/`s`
- **Residual risks:**
  - The awk and sed readers are conservative parsers, not full grammars: something they can't read asks; a print redirect hidden in an odd construct could pass as a read.
  - `--help` is trusted for installed programs by name; a program that ignores it would run.
  - A new /tmp file is judged at planning time; a symlink planted between planning and running isn't seen.

### 2026-09-29: The TUI is a board of tiles
- **Decision:**
  - Home is a board of eight tiles, one per F-key: needs you, health, findings, activity, spend, changed, orders, memory.
  - Opening a tile (or the chat) gives it the main area; the other tiles fold into a strip of live numbers along the top.
  - The composer is on every screen; on a tile, `?` asks about what's selected, and the question says what it's about.
  - Tiles are filled surfaces with half-block edges; the default theme is slate.
- **Chosen vs rejected:**
  - The owner found the ledger-and-tabs UI hard to navigate ("you have to swap tabs constantly to see useful information") and still dated. From four new directions (tiled, inspector, blocks, bento; see the round-two canvas), they chose bento.
  - F-keys over the mockups' letters: letters would fight typing in the composer, and the owner asked for F-key labels before.
  - Rejected keeping tabs with more on each: the complaint was the switching, not the content.
  - Kept the ledger as the chat: its timeline, brackets, and money columns were the part that worked.
- **Why:** The numbers that matter (an approval waiting, swap full, today's spend) should be visible whatever you're doing. The strip keeps them in view on every screen, and the board shows everything at once.
- **Where:** `reeve-tui/src/board.rs` (board, strip, surfaces, composer), `screens.rs` (each tile opened), `view.rs` (`Tile`, `Screen`, `Board`), `overlay.rs` (`NeedsPanel`, `tile()`), `run.rs` (keys, `open_tile`, `refresh_board`), `theme.rs` (`slate`)
- **Residual risks:**
  - Half blocks depend on the terminal drawing block elements edge to edge; Konsole, kitty, and foot do. With 16 colors or none the tiles get borders.
  - The board reads today's spend, orders, and memory every ten seconds and asks for the day's report every fifteen minutes.

### 2026-09-28: The TUI is a ledger with tabs
- **Decision:**
  - The conversation is drawn as a ledger: one timeline where tool calls branch off, verified changes are brackets, reeved's and the drafter's activity appear as rows, and each model round carries its cost and the session's running total.
  - The panels that floated (findings, orders, memory) and two new screens (spend, system) are full-screen tabs.
  - ⌃K searches everything.
  - Inside a verified change, `a` approves the rest of the change.
  - The default theme is ink; brass stays available.
- **Chosen vs rejected:**
  - From five directions (cockpit, ledger, quiet, workbench, briefing; see the design canvas), the owner chose ledger + quiet + workbench: the ledger's timeline and cost column, quiet chrome with ⌃K, and the workbench's tabs.
  - Rejected keeping the right rail: its live numbers now live in the status sentence and the system tab, and the ledger gets the width.
  - Rejected a text-less "reeve" row for rounds that only call tools: that round's cost goes on its first tool call.
  - Rejected letting "yes to the rest of the change" cover T3, or outlive the change: it ends at `change_commit`, and T3 still asks every time.
- **Why:** Cost, receipts, and verification were already there, but spread across a rail, popups, and the receipts panel. On one spine they read as a record of what happened and what it cost.
- **Where:** `reeve-tui/src/ledger.rs`, `screens.rs`, `draw.rs` (tabs, status), `overlay.rs` (`Spend`, `System`, `Everything`), `view.rs` (`Tab`, `RoundCost`), `theme.rs` (`ink`), `reeve-core/src/ledger.rs` (`since`, `statement`), `agent.rs` (`Decision::AllowChange`)
- **Residual risks:**
  - The ledger shows this session only; earlier sessions are statement lines pointing to their `report.md`.
  - Findings reach the ledger only when they're new since Reeve opened, so a finding that recurs stays in the findings tab.

### 2026-09-28: The state of the machine is drawn, not written by a model
- **Decision:**
  - `reeve report` and `/report` render one HTML page locally from reeved's minute metrics, findings, receipts, the spend ledger, memory, and package/unit/`/etc` drift.
  - It uses inline SVG and a tiny inline script: no model call, no network, and no external assets.
  - reeved keeps 31 days of metrics (was 14) and a daily package and unit snapshot (60 days).
  - Pages are written 0600 to `~/.reeve/reports/`, and the last 20 are kept.
- **Chosen vs rejected:**
  - Rejected a "skill" where the model writes the HTML: it would cost tokens on every run, look different each time, and send the machine's details out just to draw a chart.
  - Rejected a charting library from a CDN: the page would reach the network, and it wouldn't open offline.
  - Rejected waiting for a snapshot history before showing drift: rpm install times, pacman's log, and file times give a useful answer on day one.
- **Why:** The owner asked for a visual state of the system. Everything it needs was already on disk, so the only work is presenting it.
- **Where:** `reeve-observer/src/report/` (`mod.rs` gather and headlines, `drift.rs`, `svg.rs`, `html.rs`), `daemon.rs` (retention, `save_daily`), `reeve-cli` (`report`), `reeve-tui` (`/report`)
- **Residual risks:**
  - Without a snapshot from before the window, Fedora can't tell an install from an upgrade, and removals and unit changes don't show.
  - `/etc` changes are by modification time, so a package update that rewrites a config shows up as an edit.
  - The page holds the machine's details in plain text: it's the owner's file, like the receipts.

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
  - Order files and Reeve's `config.toml`/`settings.toml` are floor files for tools. (Since 2026-09-29 the model writes orders through `order_save`, which asks the owner each time; see that entry.)
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
