import os
import re
import sys
import json
import sqlite3
import urllib.request
import urllib.error
from datetime import datetime

sys.stdout.reconfigure(encoding='utf-8')
sys.stderr.reconfigure(encoding='utf-8')

DEFAULT_JSON = r"C:\Users\Kai\Downloads\tags-exported-182937695515967488-2031.json"
DEFAULT_DB = "healthybot.db"
DEFAULT_ASSETS_DIR = os.path.join("data", "tag_assets")

def get_discord_token():
    if os.path.exists(".env"):
        with open(".env", "r", encoding="utf-8") as f:
            for line in f:
                if line.startswith("DISCORD_TOKEN="):
                    return line.strip().split("=", 1)[1].strip('"\'')
    return os.environ.get("DISCORD_TOKEN")

def iso_to_ms(iso_str):
    if not iso_str:
        return None
    try:
        dt = datetime.fromisoformat(iso_str.replace("Z", "+00:00"))
        return int(dt.timestamp() * 1000)
    except Exception:
        return None

def init_db(db_path):
    conn = sqlite3.connect(db_path)
    cur = conn.cursor()
    cur.execute("""
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
    """)
    cur.execute("CREATE UNIQUE INDEX IF NOT EXISTS idx_tags_server_name ON tags(server_id, name_normalized);")
    cur.execute("CREATE INDEX IF NOT EXISTS idx_tags_server_uses ON tags(server_id, uses DESC);")
    cur.execute("CREATE INDEX IF NOT EXISTS idx_tags_server_author ON tags(server_id, author_id);")
    conn.commit()
    return conn

def refresh_discord_urls(urls, token):
    """Refreshes Discord CDN URLs in batches of 50 via Discord API."""
    url_map = {}
    batch_size = 50
    total_batches = (len(urls) + batch_size - 1) // batch_size
    print(f"Refreshing {len(urls)} Discord CDN URLs across {total_batches} batches...")

    for i in range(0, len(urls), batch_size):
        batch = urls[i:i + batch_size]
        req = urllib.request.Request(
            "https://discord.com/api/v10/attachments/refresh-urls",
            data=json.dumps({"attachment_urls": batch}).encode("utf-8"),
            headers={
                "Authorization": f"Bot {token}",
                "Content-Type": "application/json",
                "User-Agent": "DiscordBot (healthy-bot, 1.0)"
            }
        )
        try:
            with urllib.request.urlopen(req, timeout=15) as resp:
                data = json.loads(resp.read().decode("utf-8"))
                for item in data.get("refreshed_urls", []):
                    url_map[item["original"]] = item["refreshed"]
        except Exception as e:
            print(f"  Batch {i//batch_size + 1}/{total_batches} failed: {e}")

        if (i // batch_size + 1) % 5 == 0 or (i // batch_size + 1) == total_batches:
            print(f"  Processed batch {i//batch_size + 1}/{total_batches}")

    return url_map

def download_file(url, out_path):
    """Downloads a file to disk with User-Agent header."""
    req = urllib.request.Request(url, headers={"User-Agent": "Mozilla/5.0"})
    try:
        with urllib.request.urlopen(req, timeout=20) as resp:
            with open(out_path, "wb") as f:
                f.write(resp.read())
        return True
    except Exception as e:
        # HTTP 404, 403, 410, etc.
        return False

def get_extension(url):
    clean = url.split("?")[0]
    m = re.search(r"\.([a-zA-Z0-9]{2,5})$", clean)
    if m:
        ext = m.group(1).lower()
        if ext in ["png", "jpg", "jpeg", "gif", "webp", "mp4", "mov", "webm", "mp3", "ogg"]:
            return ext
    return "bin"

def main():
    json_path = sys.argv[1] if len(sys.argv) > 1 else DEFAULT_JSON
    db_path = sys.argv[2] if len(sys.argv) > 2 else DEFAULT_DB
    assets_dir = DEFAULT_ASSETS_DIR

    os.makedirs(assets_dir, exist_ok=True)
    token = get_discord_token()

    if not os.path.exists(json_path):
        print(f"Export file not found: {json_path}")
        return

    print(f"Loading {json_path}...")
    with open(json_path, "r", encoding="utf-8") as f:
        export_data = json.load(f)

    server_id = export_data.get("serverId", "182937695515967488")
    raw_tags = export_data.get("tags", [])
    print(f"Total raw tags: {len(raw_tags)}")

    # Step 1: Discard Lua tags
    non_lua_tags = []
    lua_discarded = 0
    for t in raw_tags:
        content = t.get("content") or ""
        if "{lua:" in content:
            lua_discarded += 1
        else:
            non_lua_tags.append(t)
    print(f"Discarded Lua script tags: {lua_discarded}")

    # Step 2: Identify Discord CDN URLs to refresh
    discord_urls = set()
    for t in non_lua_tags:
        content = t.get("content") or ""
        for u in re.findall(r"https://(?:cdn\.discordapp\.com|media\.discordapp\.net)/[^\s\)\}]+", content):
            discord_urls.add(u)

    url_map = {}
    if discord_urls and token:
        url_map = refresh_discord_urls(list(discord_urls), token)
        print(f"Successfully refreshed {len(url_map)} / {len(discord_urls)} Discord URLs.")
    elif not token:
        print("WARNING: No DISCORD_TOKEN found. Cannot refresh Discord CDN URLs!")

    # Step 3: Process tags and download assets
    conn = init_db(db_path)
    cur = conn.cursor()

    imported_count = 0
    dead_media_discarded = 0
    assets_downloaded = 0

    print("Processing and verifying tags...")
    for idx, t in enumerate(non_lua_tags):
        tag_id = t["id"]
        name = t["name"]
        name_norm = name.strip().lower()
        content = t.get("content") or ""
        author = t.get("user") or {}
        author_id = author.get("id", "0")
        author_name = author.get("username", "unknown")
        created_ms = iso_to_ms(t.get("created")) or int(datetime.now().timestamp() * 1000)
        last_used_ms = iso_to_ms(t.get("last_used"))
        uses = t.get("uses", 0)

        # Check if content has an attachment wrapper or Discord attachment
        attach_match = re.search(r"\{attach:(https?://[^\}]+)\}", content)
        discord_match = re.search(r"https://(?:cdn\.discordapp\.com|media\.discordapp\.net)/[^\s\)\}]+", content)

        asset_filename = None

        if attach_match or discord_match:
            # Need to download media
            target_url = attach_match.group(1) if attach_match else discord_match.group(0)

            # If it's a Discord URL, get refreshed version
            download_url = url_map.get(target_url, target_url)
            if not download_url:
                dead_media_discarded += 1
                continue

            ext = get_extension(download_url)
            local_filename = f"{tag_id}.{ext}"
            local_path = os.path.join(assets_dir, local_filename)

            # Download if not already cached
            success = False
            if os.path.exists(local_path) and os.path.getsize(local_path) > 0:
                success = True
            else:
                success = download_file(download_url, local_path)
                if success:
                    assets_downloaded += 1

            if not success:
                # Dead media! Discard this tag per user decision.
                dead_media_discarded += 1
                continue

            asset_filename = local_filename

            # If tag content was ONLY `{attach:...}`, empty the text or keep clean
            if attach_match and content.strip() == f"{{attach:{attach_match.group(1)}}}":
                content = ""
            elif discord_match and content.strip() == discord_match.group(0):
                content = ""

        # Insert into DB
        cur.execute("""
        INSERT OR REPLACE INTO tags (
            id, server_id, name, name_normalized, content, asset_filename,
            author_id, author_name, created_at, last_used_at, uses
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        """, (
            tag_id, server_id, name, name_norm, content, asset_filename,
            author_id, author_name, created_ms, last_used_ms, uses
        ))
        imported_count += 1

    conn.commit()
    conn.close()

    print("\n=== IMPORT COMPLETE ===")
    print(f"Total Raw Tags:               {len(raw_tags)}")
    print(f"Pruned Lua Tags:              {lua_discarded}")
    print(f"Pruned Dead-Media Tags:       {dead_media_discarded}")
    print(f"Downloaded Local Assets:      {assets_downloaded}")
    print(f"Successfully Imported Tags:   {imported_count}")

if __name__ == "__main__":
    main()
