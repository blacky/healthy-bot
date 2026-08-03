-- Per-user memory: durable facts extracted from conversation.
CREATE TABLE IF NOT EXISTS user_fact (
    id INTEGER PRIMARY KEY,
    discord_id TEXT NOT NULL,
    fact TEXT NOT NULL,
    created_at INTEGER NOT NULL
);

-- Dedup identical facts per user; speed up per-user lookups.
CREATE UNIQUE INDEX IF NOT EXISTS idx_user_fact_unique ON user_fact(discord_id, fact);
CREATE INDEX IF NOT EXISTS idx_user_fact_discord ON user_fact(discord_id);

-- Per-user opt-out from memory extraction and injection.
ALTER TABLE users ADD COLUMN memory_opt_out BOOLEAN NOT NULL DEFAULT 0;
