# Deployment & Rollout

This project deploys continuously: every push to `main` builds a tested image,
publishes it to the GitHub Container Registry (GHCR), and triggers a redeploy on
[Komodo](https://komo.do).

```
push to main
  └─ CI: fmt · clippy · build · test
       └─ deploy: build image → push ghcr.io/<owner>/<repo>:latest + :sha-<short>
            └─ signed webhook → Komodo pulls the image and recreates the stack
```

Because the bot is a single-instance, stateful service (one Discord gateway
connection + a single-writer SQLite file), a deploy is a brief stop-old /
start-new recreate — not a zero-downtime rolling update.

## 1. GitHub setup

### Secrets

Set these in **Settings → Secrets and variables → Actions**:

| Secret | Description |
| --- | --- |
| `KOMODO_WEBHOOK_URL` | The stack's `/deploy` listener URL from Komodo (see §2). |
| `KOMODO_WEBHOOK_SECRET` | Shared secret; must equal the `KOMODO_WEBHOOK_SECRET` configured on the Komodo server. |

Pushing the image uses the built-in `GITHUB_TOKEN` — no extra registry secret is
needed. The `deploy` job automatically no-ops if `KOMODO_WEBHOOK_URL` is unset,
so forks without these secrets build nothing and stay green.

### Make the GHCR package public

The first successful deploy creates the package **private**. Komodo pulls it
without credentials, so make it public once:

1. Open the package at `https://github.com/users/<owner>/packages/container/<repo>/settings`.
2. **Danger Zone → Change visibility → Public**.

(To keep it private instead, configure GHCR registry credentials on the Komodo
server so it can authenticate the pull.)

## 2. Komodo setup

1. **Create a Stack** pointing at `compose.prod.yaml` (paste it, or point Komodo
   at this repo).
2. **Environment** — provide the runtime config the stack's `.env` needs:
   - `DISCORD_TOKEN`, `OPENAI_SECRET` (required)
   - `DISCORD_GUILD_ID` (required for the reminder voice-channel task)
   - `IMAGE_TAG` (optional; defaults to `latest`)
   - `HEALTHYBOT_IMAGE` (optional; only if deploying an image under a different
     owner/repo than the default in `compose.prod.yaml`)
3. **Enable pull-on-deploy** so a deploy pulls the newest `latest` image.
4. **Webhook** — on the Stack's config page under *Webhooks*, set the branch to
   `main`, copy the `/deploy` listener URL into `KOMODO_WEBHOOK_URL`, and set the
   server's `KOMODO_WEBHOOK_SECRET` to match the GitHub secret. Komodo validates
   the `X-Hub-Signature-256` HMAC on each call. See the
   [Komodo webhook docs](https://komo.do/docs/resources/webhooks).

## 3. Rollback

Every build publishes an immutable `:sha-<short>` tag alongside the moving
`:latest`. To roll back:

1. Set `IMAGE_TAG=sha-<short>` on the Komodo stack (the SHA of a known-good
   build — see the image tags in GHCR or the commit history).
2. Redeploy the stack.

Because per-SHA images are immutable, this restores an exact prior build with no
rebuild or git revert. To return to normal, clear `IMAGE_TAG` (back to `latest`)
and redeploy.

## Local / manual run

`compose.yaml` still builds from source for local development
(`docker compose up -d --build`). `compose.prod.yaml` runs the published image
and is what Komodo uses.
