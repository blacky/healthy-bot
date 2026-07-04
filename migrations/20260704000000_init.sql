-- Create users table
CREATE TABLE IF NOT EXISTS users (
    discord_id TEXT PRIMARY KEY,
    authorized BOOLEAN NOT NULL DEFAULT 0,
    role TEXT NOT NULL DEFAULT 'USER',
    last_message INTEGER
);

-- Create reminder table
CREATE TABLE IF NOT EXISTS reminder (
    id INTEGER PRIMARY KEY,
    message TEXT NOT NULL,
    date INTEGER NOT NULL,
    owner_discord_id TEXT NOT NULL
);

-- Create setting table
CREATE TABLE IF NOT EXISTS setting (
    k TEXT PRIMARY KEY,
    v TEXT NOT NULL
);

-- Create markov_model table
CREATE TABLE IF NOT EXISTS markov_model (
    user_id TEXT NOT NULL,
    word1 TEXT NOT NULL,
    word2 TEXT NOT NULL,
    count INTEGER NOT NULL DEFAULT 1,
    PRIMARY KEY (user_id, word1, word2)
);

-- Create index on markov_model
CREATE INDEX IF NOT EXISTS idx_markov_model_user_word1 ON markov_model(user_id, word1);
