# Automatic Deployment & Rollout via GHCR + Komodo — Design

**Date:** 2026-08-03
**Branch:** `feat/auto-deploy`
**Status:** Approved (pending spec review)

## Goal

When code is merged to `main`, automatically build a tested Docker image,
publish it to the GitHub Container Registry (GHCR), and trigger a redeploy on
the Komodo instance that hosts the bot — with a clean rollback path.

## Constraints & context

- **Single-instance, stateful service.** The bot holds one Discord gateway
  connection and a single-writer SQLite file on a mounted volume. Two instances
  cannot run at once, so "rollout" here means *versioned images + stop-old /
  start-new recreate + easy rollback*, **not** zero-downtime blue/green.
- **Komodo is on a third party's server** (a friend's). This repo owns only the
  CI, the production compose file, and documentation. Komodo-side resources are
  configured by whoever runs Komodo, following the docs; no Komodo Resource Sync
  files are committed.
- **Upstream-safe & portable.** The workflow must be harmless to merge into
  `blacky/healthy-bot`: the image name is derived from the repository, and the
  deploy job is a no-op wherever the Komodo secrets are absent.

## Decisions (from brainstorming)

| Decision | Choice |
| --- | --- |
| Build topology | CI builds → GHCR → Komodo pulls |
| Deploy trigger | Every push to `main` (continuous deploy) |
| Registry visibility | Public GHCR image (no Komodo registry credentials) |
| Komodo config in repo | Repo artifacts + docs only (no Resource Sync TOML) |
| Portability | Upstream-safe; image name derived; deploy job guarded by secret presence |

## Pipeline overview

```
push to main
  └─ CI job (existing): fmt · clippy · build · test
       └─ deploy job (needs: ci, main only, secrets present):
            ├─ docker buildx build (Dockerfile, gha cache) → runtime image
            ├─ push ghcr.io/<owner>/<repo>:latest  +  :sha-<short>
            └─ signed POST to Komodo /deploy listener
                 └─ Komodo pulls new image, recreates the stack
```

No Rust source changes. The existing `ci` job is the test gate and remains
untouched; `deploy` runs only after it via `needs: ci`.

## Component 1 — GitHub Actions `deploy` job (in `.github/workflows/rust.yml`)

- **Triggers.** Loosen the current `paths-ignore` so infrastructure changes
  (`Dockerfile`, `Cargo.toml`, `Cargo.lock`, `compose*.yaml`, `src/**`,
  `migrations/**`) trigger the workflow, while pure-docs paths (`**.md`,
  `.gitignore`, `docs/**`) do not.
- **Job gating.**
  - `needs: ci` — deploy only if fmt/clippy/build/test passed.
  - `if: github.event_name == 'push' && github.ref == 'refs/heads/main'`.
  - `concurrency: { group: deploy-${{ github.ref }}, cancel-in-progress: true }`
    so back-to-back merges cancel superseded deploys.
  - **Secret guard:** a first step reads `secrets.KOMODO_WEBHOOK_URL` into an env
    var and writes `enabled=true|false` to `$GITHUB_OUTPUT`; all subsequent
    build/push/webhook steps are `if: steps.guard.outputs.enabled == 'true'`.
    Secrets cannot be referenced directly in job/step `if:` conditions, so this
    env-var indirection is required. On a fork/upstream without the secrets the
    job succeeds as a no-op.
- **Permissions:** `contents: read`, `packages: write` (for GHCR via the
  built-in `GITHUB_TOKEN`).
- **Build & push:**
  - `docker/login-action` → `ghcr.io`, username `${{ github.actor }}`, password
    `${{ secrets.GITHUB_TOKEN }}`.
  - `docker/metadata-action` → image `ghcr.io/${{ github.repository }}`
    (lowercased by the action), tags `type=raw,value=latest` and
    `type=sha,prefix=sha-,format=short`.
  - `docker/setup-buildx-action` + `docker/build-push-action` with
    `cache-from/to: type=gha`, `push: true`. Default target = `runtime` stage;
    the Dockerfile `tester` stage is intentionally skipped because the `ci` job
    already ran the tests.
- **Trigger Komodo:**
  ```sh
  payload='{"ref":"refs/heads/main"}'
  sig="sha256=$(printf '%s' "$payload" | openssl dgst -sha256 -hmac "$KOMODO_WEBHOOK_SECRET" | awk '{print $2}')"
  curl -fsS -X POST "$KOMODO_WEBHOOK_URL" \
    -H 'Content-Type: application/json' \
    -H "X-GitHub-Event: push" \
    -H "X-Hub-Signature-256: $sig" \
    -d "$payload"
  ```
  Komodo's `github`-auth listener validates the HMAC signature against
  `KOMODO_WEBHOOK_SECRET`, matches the branch in the payload against the stack's
  configured branch, and runs the stack's `/deploy` execution.

**New repository secrets:** `KOMODO_WEBHOOK_URL`, `KOMODO_WEBHOOK_SECRET`.
GHCR push uses the built-in `GITHUB_TOKEN`.

## Component 2 — `compose.prod.yaml` (new)

Separate from the dev `compose.yaml` (which keeps `build: .`). References the
published image; both the image path and tag are overridable for portability and
rollback:

```yaml
services:
  healthy-bot:
    image: ${HEALTHYBOT_IMAGE:-ghcr.io/buracc/healthy-bot-rust}:${IMAGE_TAG:-latest}
    container_name: healthy-bot
    env_file: [.env]
    environment:
      DB_FILE: /healthybot/db/healthybot.db
    volumes:
      - healthybot-db:/healthybot/db
      - healthybot-data:/healthybot/data
    restart: unless-stopped
volumes:
  healthybot-db:
  healthybot-data:
```

## Component 3 — DB persistence fix

`DB_FILE` defaults to `healthybot.db`, resolved relative to the image WORKDIR
`/app`, so the SQLite file would land at `/app/healthybot.db` — *not* on the
mounted `/healthybot/db` volume — and be lost on each redeploy. `compose.prod.yaml`
sets `DB_FILE=/healthybot/db/healthybot.db` so the database persists on the named
volume across deploys. (Markov legacy data already lives under `/healthybot/data`.)

## Component 4 — `docs/deployment.md` (new; linked from README)

Documents:
1. The two GitHub secrets and where to set them.
2. **Making the GHCR package public** — it is private on first publish; must be
   flipped to public once so Komodo can pull without credentials.
3. Komodo stack setup: create a Stack from `compose.prod.yaml`; provide `.env`
   (`DISCORD_TOKEN`, `OPENAI_SECRET`, `DISCORD_GUILD_ID`, optional `IMAGE_TAG`);
   set the webhook branch to `main`; copy the `/deploy` listener URL; set
   `KOMODO_WEBHOOK_SECRET`; enable pull-images-on-deploy.
4. **Rollback:** set `IMAGE_TAG=sha-<short>` on the stack and redeploy. Per-SHA
   images are immutable, so this reliably restores a prior build with no rebuild.

## Rollback model

Continuous deploy to `main` moves the mutable `:latest` tag; every build also
publishes an immutable `:sha-<short>`. Rollback = point the stack's `IMAGE_TAG`
at a known-good SHA and redeploy. No git revert or CI rebuild needed for an
emergency rollback.

## Verification

No unit-testable code is added. Verification steps:

- `docker compose -f compose.prod.yaml config` — parses and resolves variables.
- `docker build .` locally — confirms the Dockerfile still builds after any
  trigger/paths changes.
- `actionlint` on the workflow if available; otherwise careful manual review of
  the YAML.
- **End-to-end** (build → GHCR → Komodo redeploy) can only be confirmed by a real
  push after the Komodo side is wired and the secrets are added. This is an
  explicit manual acceptance step, not self-verifiable in this repo.

## Out of scope

- Zero-downtime / blue-green rollout (impossible for this single-instance,
  single-writer service).
- Komodo Resource Sync TOML (Komodo is configured by its operator via the docs).
- Private registry auth, multi-arch images, staging environments, DB migration
  gating beyond what `sqlx::migrate!()` already does at startup.
