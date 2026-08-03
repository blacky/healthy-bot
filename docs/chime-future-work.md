# Random Chime — Future Work

Potential additions and improvements to the spontaneous-interjection feature
(`src/chime.rs` + `try_random_chime` in `src/main.rs`). Grounded in the current
implementation's deliberate limitations; ordered roughly by value-to-effort
within each section. Nothing here is committed to — it's a backlog.

## Current behavior (baseline)

- Fires on human messages in the single `main_text_channel` that aren't
  mentions/replies/commands.
- Gate chain (pure `chime::evaluate`): enabled (`random_chime_chance` > 0) →
  eligibility → daily cap (`random_chime_daily_cap`) → cooldown
  (`random_chime_cooldown_seconds`) → probability roll. A fire consumes the
  cooldown and daily-cap tally before any API call.
- When it fires, an optional relevance pre-check (`random_chime_relevance_check`)
  asks a cheap model whether an interjection is warranted, failing closed.
- Context: last ~10 messages, text only. Persona/model reuse `ai_initial_prompt`
  / `ai_chat_model`, overridable per-feature via `random_chime_prompt` /
  `random_chime_model`. Posts a standalone message (no reply-ping).

## Behavior & quality

- **Anti-repetition.** Track the last few chimes (in memory) and suppress a new
  one that's near-duplicate in content, or skip if the bot already spoke in the
  last N messages of context.
- **Lull/activity awareness.** Blend the random trigger with lull detection: bias
  the effective chance up when a channel is actively buzzing and down (or off)
  when it's dead, so interjections land where there's a live conversation.
- **Respond to a chosen message, not just the latest.** Optionally pick the most
  "interesting" recent message (e.g. a question) as the focal point rather than
  always the triggering one.
- **Optional reply threading.** A setting to reply-to the focal message instead
  of posting standalone, for cases where context attribution matters.

## Configuration & control

- **Channel allowlist.** Generalize scope from the single `main_text_channel` to
  a configurable list of channel IDs. Requires per-channel cooldown state (see
  Code below).
- **Quiet hours.** Time-of-day windows (Amsterdam tz, matching the rest of the
  bot) during which chiming is suppressed.
- **Explicit enable flag.** Add a `random_chime_enabled` boolean instead of
  overloading `chance == 0` as the off switch, for clearer intent.

## Cost & safety

- **Per-user opt-out.** Let users exclude themselves so their messages are never
  used as chime context and never trigger a chime — likely a future request, and
  a good privacy default. Pairs with a general AI/Markov opt-out.
- **Content guardrails.** A moderation pre-filter on recent context (beyond the
  relevance check's soft judgement) to hard-block chiming on sensitive material.

## Observability & tuning

- **Reaction feedback loop.** Watch for 👍/👎 (or a configurable pair) on the
  bot's chimes and log an acceptance rate; longer term, use it to auto-tune the
  effective chance.
- **Metrics/logging.** Count rolls, each `SkipReason`, relevance declines, fires,
  and API errors so the trigger rate can be tuned from real data.

## Code & testing

- **Per-channel cooldown state.** `last_random_chime` is a single `Mutex<Instant>`
  — fine for one channel, but a multi-channel allowlist needs a
  `Mutex<HashMap<ChannelId, Instant>>` (or a small LRU) so channels don't share
  one timer. The same applies to the daily-cap tally.
- **Multi-guild settings.** Settings are global today. Supporting the bot across
  multiple guilds would require per-guild settings, which affects chime scope
  and channel config too.
