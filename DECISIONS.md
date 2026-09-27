# Decisions

Why, not what. Newest first. Each entry: Decision / Chosen vs rejected / Why / Where / Residual risk.

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
