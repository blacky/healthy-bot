# Healthy Bot (Rust Rewrite)

A high-performance Discord bot rewritten in Rust, optimized for speed and memory efficiency.

## The Rust Rewrite

This project is a complete refactor of the original [healthy-bot](https://github.com/buracc/healthy-bot), which was built using Kotlin, Spring Boot, and JDA.

### Why the change?
The primary goal was to address high resource consumption. By moving to a native Rust implementation and migrating data structures to SQLite, we achieved massive optimizations:
- **Memory Usage:** Peak memory usage dropped from 1.8GB (JVM) to just **~15MB** (Rust) during steady state.
- **Startup Time:** Near-instantaneous startup compared to the Spring Boot overhead.
- **Reliability:** Built-in memory safety and concurrency primitives of Rust ensure a stable long-running process.

## Core Features

- **Reminders:** A flexible reminder system (!remind) to keep track of tasks, events, or anything else.
- **Markov Chains:** Learns from user messages to generate hilarious (and often nonsensical) text chains (!markov). Now fully database-backed for extreme memory efficiency.
- **OpenAI Integration:** Context-aware replies using GPT models. It can follow reply chains and respond naturally to mentions.
- **Random Interjections:** Occasionally joins conversations on its own in the main channel, gated by a relevance check so it only speaks when it would add value. Off by default; enable with `!settings set random_chime_chance <percent>` (e.g. `2`). Tunable via `random_chime_cooldown_seconds` (minimum gap), `random_chime_daily_cap` (0 = unlimited), `random_chime_relevance_check` (`true`/`false`), and an optional dedicated `random_chime_prompt` / `random_chime_model`.
- **Member Management:** Track user activity ("Inthards") and manage authorized users.
- **Dynamic Settings:** Configure bot behavior (cooldowns, prompts, status) on-the-fly via commands.

## Tech Stack

- **[Serenity](https://github.com/serenity-rs/serenity):** Discord API library.
- **[Poise](https://github.com/serenity-rs/poise):** A powerful framework for Serenity bots.
- **[SQLx](https://github.com/launchbadge/sqlx):** Async, type-safe SQLite interaction.
- **[Tokio](https://github.com/tokio-rs/tokio):** The industry-standard async runtime for Rust.
- **[OpenAI](https://openai.com/):** Powering the AI conversational features.

## Configuration

The bot is configured via environment variables. You can set these in a .env file or directly in your environment.

| Variable | Description | Default |
| :--- | :--- | :--- |
| `DISCORD_TOKEN` | Your Discord Bot Token (Required) | - |
| `OPENAI_SECRET` | Your OpenAI API Key (Required) | - |
| `DB_FILE` | Path to the SQLite database file | `healthybot.db` |
| `RUST_LOG` | Logging level (info, debug, etc.) | `info` |

## Deployment

The project includes a `Dockerfile` and `compose.yaml` for easy local deployment.

```bash
docker-compose up -d
```

Ensure your volumes are mapped correctly for persistence:
- `/healthybot/db`: Database storage (includes Markov chains).
- `/healthybot/data`: Legacy Markov model data for migration.

For automated build-and-deploy (push to `main` → GHCR → Komodo redeploy),
including rollback and the production compose file (`compose.prod.yaml`), see
[docs/deployment.md](docs/deployment.md).

## Commands

- `!remind <message> <time>`: Create a new reminder.
- `!markov [@user]`: Generate a markov chain.
- `!user inthards`: View the top list of inactive users.
- `!settings`: View or modify bot configuration.
- `!help`: Show this message.

## Bot Exception

If you want to allow another bot in your server to use `!markov` and have its messages tracked in the database, you can set the `allowed_bot_id` setting:

```bash
!settings set allowed_bot_id 123456789012345678
```

This will allow that specific bot to:
- Contribute to Markov chain word-pairs.
- Trigger the `!markov` command.

All other commands and AI interactions remain restricted for all bots.
