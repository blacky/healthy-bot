CREATE TABLE IF NOT EXISTS api_usage (
    user_id TEXT PRIMARY KEY,
    chat_tokens INTEGER NOT NULL DEFAULT 0,
    memory_tokens INTEGER NOT NULL DEFAULT 0,
    is_restricted BOOLEAN NOT NULL DEFAULT 0
);
