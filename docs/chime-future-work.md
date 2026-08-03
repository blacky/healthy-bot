# Random Chime — Future Work

Potential additions and improvements to the spontaneous-interjection feature
(`src/chime.rs` + `try_random_chime` in `src/main.rs`). Grounded in the current
implementation's deliberate limitations; ordered roughly by value-to-effort
within each section. Nothing here is committed to — it's a backlog.

## Current behavior (baseline)

- Fires on human messages in the single `main_text_channel` that aren't
  mentions/replies/commands.
- Gate chain: `random_chime_chance` > 0 → eligibility → `random_chime_cooldown_seconds`
  → probability roll. Cooldown resets only on a successful chime.
- Context: last ~10 messages, text only. Persona reuses `ai_initial_prompt` /
  `ai_chat_model`. Posts a standalone message (no reply-ping).

## Behavior & quality

- **Relevance pre-check (highest value).** Today it chimes on a blind roll,
  regardless of whether it has anything worth adding. Add a cheap two-stage
  decision: once the roll passes, ask a small/cheap model "does this
  conversation warrant a spontaneous interjection? yes/no" and only proceed on
  yes. Dramatically improves signal-to-noise; the main cost lever.
- **Anti-repetition.** Track the last few chimes (in memory) and suppress a new
  one that's near-duplicate in content, or skip if the bot already spoke in the
  last N messages of context.
- **Lull/activity awareness.** Blend the random trigger with the lull-detection
  idea we set aside: bias the effective chance up when a channel is actively
  buzzing and down (or off) when it's dead, so interjections land where there's
  a live conversation to join.
- **Respond to a chosen message, not just the latest.** Optionally pick the most
  "interesting" recent message (e.g. longest, or a question) as the focal point
  rather than always the triggering one.
- **Optional reply threading.** A setting to reply-to the focal message instead
  of posting standalone, for cases where context attribution matters.

## Configuration & control

- **Channel allowlist.** Generalize scope from the single `main_text_channel` to
  a configurable list of channel IDs (`random_chime_channels`). Requires
  per-channel cooldown state (see Code below).
- **Dedicated chime persona.** A separate `random_chime_prompt` distinct from
  `ai_initial_prompt`, so spontaneous interjections can have their own voice
  without changing how the bot answers when directly addressed.
- **Separate (cheaper) chime model.** A `random_chime_model` setting so chimes
  can run on a cheaper model than direct replies.
- **Quiet hours.** Time-of-day windows (Amsterdam tz, matching the rest of the
  bot) during which chiming is suppressed.
- **Explicit enable flag.** Add a `random_chime_enabled` boolean instead of
  overloading `chance == 0` as the off switch, for clearer intent.

## Cost & safety

- **Daily/rolling cap.** A hard ceiling on chimes per day (or per rolling window)
  independent of the cooldown, to bound worst-case spend.
- **Per-user opt-out.** Let users exclude themselves so their messages are never
  used as chime context and never trigger a chime — likely a future request, and
  a good privacy default. Pairs with a general AI/Markov opt-out.
- **Content guardrails.** Skip chiming when recent context looks sensitive
  (moderation pre-filter), to avoid the bot butting into the wrong moment.

## Observability & tuning

- **Reaction feedback loop.** Watch for 👍/👎 (or a configurable pair) on the
  bot's chimes and log an acceptance rate; longer term, use it to auto-tune the
  effective chance.
- **Metrics/logging.** Count rolls, eligibility rejections, fires, and API
  errors so the trigger rate can be tuned from real data rather than guesswork.

## Code & testing

- **Per-channel cooldown state.** `last_random_chime` is a single `Mutex<Instant>`
  — fine for one channel, but a multi-channel allowlist needs a
  `Mutex<HashMap<ChannelId, Instant>>` (or a small LRU) so channels don't share
  one timer.
- **Extract a testable decision pipeline.** `try_random_chime` currently mixes
  settings reads, gating, and I/O. Factoring the gate chain into a pure
  `ChimeDecision::evaluate(...) -> Decision` (enum: `Skip(reason)` / `Fire`)
  would make the whole decision path unit-testable, not just the two leaf
  helpers.
- **Multi-guild settings.** Settings are global today. Supporting the bot across
  multiple guilds would require per-guild settings, which affects chime scope
  and channel config too.
