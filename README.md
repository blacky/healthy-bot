# Healthy Bot (Rust Rewrite)

A high-performance Discord bot rewritten in Rust, optimized for speed and memory efficiency.

## The Rust Rewrite

This project is a complete refactor of the original [healthy-bot](https://github.com/buracc/healthy-bot), which was built using Kotlin, Spring Boot, and JDA.

### Why the change?
The primary goal was to address high resource consumption. By moving to a native Rust implementation, we achieved significant optimizations:
- **Memory Usage:** Peak memory usage dropped from 1.8GB (JVM) to just 350MB (Rust).
- **Startup Time:** Near-instantaneous startup compared to the Spring Boot overhead.
- **Reliability:** Built-in memory safety and concurrency primitives of Rust ensure a stable long-running process.

## Core Features

- **Reminders:** A flexible reminder system (!remind) to keep track of tasks, events, or anything else.
- **Markov Chains:** Learns from user messages to generate hilarious (and often nonsensical) text chains (!markov).
- **OpenAI Integration:** Context-aware replies using GPT models. It can follow reply chains and respond naturally to mentions.
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

The project includes a `Dockerfile` and `compose.yaml` for easy deployment.

```bash
docker-compose up -d
```

Ensure your volumes are mapped correctly for persistence:
- `/healthybot/db`: Database storage.
- `/healthybot/data`: Markov model data.

## Commands

- `!remind <message> <time>`: Create a new reminder.
- `!markov [@user]`: Generate a markov chain.
- `!user inthards`: View the top list of inactive users.
- `!settings`: View or modify bot configuration.
- `!help`: Show this message.
