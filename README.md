# Reeve

An **operator harness**: an agent that runs on your computer and manages it for you. It isn't a
coding agent. It keeps the machine healthy, tidy, and configured. It gives receipts for everything
it does, and it learns how your system behaves.

> **Status: M0 (scaffold).** Reeve can chat, price, and cap spending, and it draws the
> mission-control TUI with live system panels. It has no tools yet, so it can explain and plan
> but not act. See [`design.md`](design.md) for the whole plan and [`DECISIONS.md`](DECISIONS.md)
> for the reasoning.

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

Configuration lives in `~/.reeve/config.toml` (see [`config.example.toml`](config.example.toml)). Reeve never
rewrites that file. Choices made in the TUI go to `~/.reeve/settings.toml`, which is layered on top.
Set `REEVE_HOME` to use a different state directory.

## Keys in the TUI

| key | does |
| --- | --- |
| `⏎` / `alt+⏎` | send / newline |
| `/` | commands: `/providers`, `/model`, `/new`, `/yolo`, `/help`, `/quit` |
| `^p` | `/providers` |
| `esc` | stop the running turn, or clear the composer |
| `^y` | YOLO: auto-approve T0–T2 actions. The safeguard floor still asks. |
| `^b` | on narrow terminals, switch between the chat and the live rail |
| `pgup` / `pgdn`, mouse wheel | scroll |
| `^c` | stop, clear, then quit |

## Layout

```
crates/reeve-core      config, keys, providers (OpenRouter + OpenAI-compatible), pricing, ledger, agent
crates/reeve-observer  /proc + /sys sampler (becomes the `reeved` observer in M4)
crates/reeve-tui       mission-control UI
crates/reeve-cli       the `reeve` binary
```
