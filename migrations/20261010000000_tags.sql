-- Tag system storage
CREATE TABLE IF NOT EXISTS tags (
    id TEXT PRIMARY KEY,
    server_id TEXT NOT NULL,
    name TEXT NOT NULL,
    name_normalized TEXT NOT NULL,
    content TEXT NOT NULL,
    asset_filename TEXT,
    author_id TEXT NOT NULL,
    author_name TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    last_used_at INTEGER,
    uses INTEGER NOT NULL DEFAULT 0
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_tags_server_name ON tags(server_id, name_normalized);
CREATE INDEX IF NOT EXISTS idx_tags_server_uses ON tags(server_id, uses DESC);
CREATE INDEX IF NOT EXISTS idx_tags_server_author ON tags(server_id, author_id);
