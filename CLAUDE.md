# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Overview

A Discord bot (Serenity + Poise framework) with SQLite persistence via SQLx. It provides reminders, per-user Markov chain generation, and context-aware OpenAI chat replies. This is a Rust rewrite of a former Kotlin/Spring/JDA bot, optimized for low memory (~15MB steady state).

## Commands

```bash
cargo build                       # build
cargo test                        # run all tests
cargo test --test db_tests        # run one test file (integration tests live in tests/)
cargo test test_db_update_last_message   # run a single test by name
cargo fmt --all -- --check        # formatting check (CI enforces this)
cargo clippy -- -D warnings       # lint; CI treats all warnings as errors
cargo run                         # run locally (needs env vars below)
```

CI (`.github/workflows/rust.yml`) runs, in order: `fmt --check`, `clippy -D warnings`, `build`, `test`. The Dockerfile also runs `cargo test --release` as a build stage, so failing tests break the image build.

### Required environment variables

`DISCORD_TOKEN` and `OPENAI_SECRET` are required or the process panics at startup. `DB_FILE` defaults to `healthybot.db`. `DISCORD_GUILD_ID` is required specifically for the reminder-VC background task (not documented in the README) — without it that task errors out each tick but the bot otherwise runs. `RUST_LOG` overrides the default filter (`none,healthy_bot=info,poise=warn,...`).

## Architecture

Binary (`main.rs`) + library (`lib.rs`) split; tests depend on the `healthy_bot` library crate.

- **`main.rs`** — startup, DB pool config, the Poise `Framework` builder (command list, error handler, dynamic prefix), and the `event_handler` that drives the OpenAI reply flow. Also holds `build_message_content` (attachment/URL → OpenAI image parts).
- **`lib.rs`** — the shared `Data` struct (passed to every command), the `Error`/`UserError` types, and small helpers (`truncate_str`, `user_error`).
- **`commands.rs`** — all slash+prefix commands and their autocomplete/permission helpers.
- **`db.rs`** — row structs (`User`, `Reminder`, `Setting`) and low-level query helpers.
- **`markov.rs`** — `MarkovRepository`, fully DB-backed Markov chain storage/generation.
- **`openai.rs`** — `OpenAIClient` and the request/response types.
- **`tasks.rs`** — background tokio loops (reminder announcer, reminder-VC updater).

### Key cross-cutting patterns

**Settings are dual-stored.** Every setting lives in the `setting` table *and* an in-memory `settings_cache: Arc<RwLock<HashMap<String,String>>>` on `Data`, loaded once at startup. The hot path (message events, cooldown checks) reads only the cache. **Any code that changes a setting must write both the DB row and the cache** (see `settings`/`status` commands) — otherwise the change won't take effect until restart.

**SQLx is used in runtime mode, not compile-time.** All queries use `sqlx::query`/`query_as` with string SQL, *not* the `query!` macros. There is no `DATABASE_URL` or `.sqlx` offline data — the schema is not checked at compile time. Schema changes go in `migrations/` and are applied at startup and in tests via `sqlx::migrate!()` (which embeds the `migrations/` dir at compile time).

**Error visibility is deliberate.** `UserError` (in `lib.rs`) is the only error type whose message is shown to users verbatim. The Poise `on_error` handler replaces every other error with a generic message so DB/parse/internal error text never leaks into chat. Use `user_error("...")` for anything a user should read; return raw errors (`?`) for internal failures.

**All human-facing times use `Europe/Amsterdam`.** Reminder parsing/display, VC channel names, and log timestamps are hardcoded to Amsterdam time, with explicit DST-gap handling (`from_local_datetime` → `LocalResult`). Reminder timestamps are stored as **milliseconds**; `db::to_utc` heuristically handles both seconds and millis for legacy data.

**Discord field-length limits.** User-controlled strings placed into embeds/channel names must pass through `truncate_str` first (Discord rejects the whole payload if any field exceeds its limit — 256 name / 1024 value / 100 channel name).

### OpenAI reply flow (`event_handler` in `main.rs`)

Triggered when the bot is mentioned or a message replies to the bot. It walks the reply chain upward up to 10 hops — Discord only inlines the immediate parent, so deeper ancestors are fetched explicitly via `message_reference`. Vision (image parts) is gated on the model name containing `gpt-4o`/`gpt-5`/`o1`/`o3`; images are only attached for the current message and the immediate parent to save tokens. An `ai_cooldown_seconds` setting throttles calls. Responses over 2000 chars are split across chained replies.

**Threads act like a standing ChatGPT-style conversation.** The first @mention or reply to the bot inside a thread channel registers that thread's `ChannelId` in `Data.active_ai_threads` (`RwLock<HashSet<ChannelId>>`); every later message in that thread then triggers a reply too, without needing another mention. Context for a thread is built differently from the top-level case: instead of walking `referenced_message`, it pulls the last 10 messages from the channel directly via `build_thread_history_messages` (thread messages are a flat sequence, not a reply chain). Known limitations, left unaddressed as acceptable trade-offs:
- `active_ai_threads` is pruned only on `ThreadDelete`, not on archival (Discord's normal end-of-life for an idle thread is `THREAD_UPDATE`, not deletion) — the set can grow unboundedly over a long-running process on a thread-heavy server.
- `ai_cooldown_seconds` is a single global cooldown (`Data.last_openai`), not per-channel/thread. If an admin sets it above the default of 0, a burst of messages inside an active thread can have some silently dropped, which cuts against "respond to every message" for that thread.
- Activation state is in-memory only and does not survive a restart; users must @mention the bot once more to resume a standing conversation after a deploy.

### Markov storage (`markov.rs`)

Each message's word-pairs are tallied in-memory then written in a **single transaction** under **both** the author's ID and the bot's own ID (skipping the author==bot case). `_start`/`_end` are sentinel tokens. Generation is a weighted random walk. Discord mentions are stripped via a shared `MENTION_REGEX`. Bots are ignored except one optional `allowed_bot_id` setting, which lets a whitelisted bot contribute to and trigger Markov only.

### Permissions

Two overlapping checks in `commands.rs`: `is_server_admin` (guild owner or Administrator permission, computed from roles so it works for prefix commands too) and `is_user_admin` (server admin OR DB `role == "ADMIN"`). Reminder creation additionally accepts an authorized flag or a hardcoded "Hall of Fame" role. A hardcoded owner user ID (`210531463932674050`) gates the `register` command and is in the Poise `owners` set.

## Testing conventions

Integration tests are in `tests/` (one file per module area). DB tests spin up a fresh temp-file SQLite via `tempfile::NamedTempFile` and run the real `migrations/` so they exercise the production schema. Tests are `#[tokio::test]`.
