use healthy_bot::chime;
use healthy_bot::markov::MarkovRepository;
use healthy_bot::openai::{
    sanitize_name, ChatMessage, ChatMessageRequestContent, ContentPart, ImageUrlTarget,
    OpenAIClient,
};
use healthy_bot::{
    commands, db, memory, recent, split_message, tasks, truncate_str, Data, Error, UserError,
};
use poise::serenity_prelude as serenity;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool};
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::RwLock;

type Context<'a> = poise::Context<'a, Data, Error>;

async fn event_handler(
    ctx: &serenity::Context,
    event: &serenity::FullEvent,
    _framework: poise::FrameworkContext<'_, Data, Error>,
    data: &Data,
) -> Result<(), Error> {
    if let serenity::FullEvent::Message { new_message } = event {
        let is_allowed_bot = if new_message.author.bot {
            let cache = data.settings_cache.read().await;
            cache
                .get("allowed_bot_id")
                .map(|id| id == &new_message.author.id.to_string())
                .unwrap_or(false)
        } else {
            false
        };

        if new_message.author.bot && !is_allowed_bot {
            return Ok(());
        }

        let author_id_str = new_message.author.id.to_string();
        let bot_id_str = ctx.cache.current_user().id.to_string();

        // Update last message time
        let _ = db::create_user_if_not_exists(&data.db_pool, &author_id_str).await;
        let _ = db::update_last_message(&data.db_pool, &author_id_str, chrono::Utc::now()).await;

        // Store Markov
        data.markov_repo
            .store(&author_id_str, &bot_id_str, &new_message.content)
            .await;

        if new_message.author.bot {
            return Ok(());
        }

        // OpenAI Reply Logic (AiListener.kt)
        let self_id = ctx.cache.current_user().id;
        let mentioned = new_message.mentions.iter().any(|u| u.id == self_id);
        let replied = new_message
            .referenced_message
            .as_ref()
            .map(|m| m.author.id == self_id)
            .unwrap_or(false);

        if mentioned || replied {
            let cooldown_secs: u64 = {
                let cache = data.settings_cache.read().await;
                cache
                    .get("ai_cooldown_seconds")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0)
            };

            let mut last_inv = data.last_openai.lock().await;
            if last_inv.elapsed().as_secs() < cooldown_secs {
                return Ok(());
            }
            *last_inv = std::time::Instant::now();
            drop(last_inv);

            log::info!(
                "OpenAI trigger from {} in channel {}: {}",
                new_message.author.name,
                new_message.channel_id,
                new_message.content.trim()
            );

            let (bot_context, chat_model) = {
                let cache = data.settings_cache.read().await;
                (
                    cache
                        .get("ai_initial_prompt")
                        .cloned()
                        .unwrap_or_else(|| "You are a helpful assistant.".to_string()),
                    cache
                        .get("ai_chat_model")
                        .cloned()
                        .unwrap_or_else(|| "gpt-3.5-turbo".to_string()),
                )
            };

            // Prepend what the bot remembers about the participants, if anything.
            let bot_context = match build_memory_prefix(data, new_message).await {
                Some(mem) => format!("{}\n\n{}", bot_context, mem),
                None => bot_context,
            };

            let bot_name = ctx.cache.current_user().name.clone();
            let bot_id = ctx.cache.current_user().id;

            let supports_vision = {
                let model_lower = chat_model.to_lowercase();
                model_lower.contains("gpt-4o")
                    || model_lower.contains("gpt-5")
                    || model_lower.contains("o1")
                    || model_lower.contains("o3")
            };

            // Build conversation context by walking up the reply chain. Discord
            // only includes the immediate parent inline (`referenced_message`);
            // deeper ancestors come back with an empty `referenced_message`, so
            // each one must be fetched explicitly via its `message_reference`
            // pointer. Capped at 10 hops for token/rate-limit safety.
            let mut messages = Vec::new();
            let mut parent = new_message.referenced_message.as_deref().cloned();
            for hop in 0..10 {
                let Some(msg) = parent else {
                    break;
                };

                let content_trimmed = msg.content.trim();
                if content_trimmed.is_empty() && msg.attachments.is_empty() {
                    // Fetch parent to keep traversing, but don't add empty message to vector
                    parent = match &msg.message_reference {
                        Some(reference) => match reference.message_id {
                            Some(mid) => reference.channel_id.message(ctx, mid).await.ok(),
                            None => None,
                        },
                        None => None,
                    };
                    continue;
                }

                let role = if msg.author.id == bot_id {
                    "assistant"
                } else {
                    "user"
                };

                // Strip images from older ancestors (hop >= 1) to save tokens and costs; only keep for the immediate parent (hop == 0)
                let include_images = supports_vision && hop == 0;
                let content_payload = build_message_content(
                    &data.openai_client.client,
                    content_trimmed,
                    &msg.attachments,
                    include_images,
                )
                .await;

                messages.push(ChatMessage {
                    role: role.to_string(),
                    name: Some(sanitize_name(&msg.author.name)),
                    content: content_payload,
                });

                // Fetch this message's own parent (the gateway didn't include it).
                parent = match &msg.message_reference {
                    Some(reference) => match reference.message_id {
                        Some(mid) => reference.channel_id.message(ctx, mid).await.ok(),
                        None => None,
                    },
                    None => None,
                };
            }
            messages.reverse(); // Reverse the history to be in chronological order

            // Add the system prompt at the very beginning
            messages.insert(
                0,
                ChatMessage {
                    role: "system".to_string(),
                    name: Some(sanitize_name(&bot_name)),
                    content: ChatMessageRequestContent::Text(bot_context),
                },
            );

            let prompt = new_message.content.trim();
            if prompt.is_empty() && new_message.attachments.is_empty() {
                return Ok(());
            }

            let prompt_payload = build_message_content(
                &data.openai_client.client,
                prompt,
                &new_message.attachments,
                supports_vision,
            )
            .await;

            messages.push(ChatMessage {
                role: "user".to_string(),
                name: Some(sanitize_name(&new_message.author.name)),
                content: prompt_payload,
            });

            let ai_debug = {
                let cache = data.settings_cache.read().await;
                cache
                    .get("ai_debug")
                    .map(|v| v.trim().to_lowercase() == "true")
                    .unwrap_or(false)
            };

            if ai_debug {
                log::info!("=== OpenAI Message Chain ===");
                for (i, msg) in messages.iter().enumerate() {
                    log::info!(
                        "  [{}] Role: {} | Name: {} | Content: {}",
                        i,
                        msg.role,
                        msg.name.as_deref().unwrap_or("-"),
                        msg.content
                    );
                }
                log::info!("============================");
            }

            let thinking_reaction = serenity::all::ReactionType::Unicode("💭".to_string());
            let _ = new_message.react(ctx, thinking_reaction.clone()).await;
            let _typing = new_message.channel_id.start_typing(&ctx.http);

            match data.openai_client.create_chat(&chat_model, messages).await {
                Ok(response) => {
                    if let Some(choice) = response.choices.first() {
                        let content = &choice.message.content;
                        if content.is_empty() {
                            let _ = new_message.reply(ctx, "OpenAI did not respond.").await;
                        } else if content.len() <= 2000 {
                            // Discord 2000 char limit handling
                            let _ = new_message.reply(ctx, content).await;
                            log::info!("OpenAI replied to {}", new_message.author.name);
                        } else {
                            log::info!(
                                "OpenAI response too long ({} chars), splitting...",
                                content.len()
                            );
                            let mut last_sent: Option<serenity::Message> = None;
                            for chunk in split_message(content, 2000) {
                                let sent = if let Some(ref prev) = last_sent {
                                    prev.reply(ctx, chunk).await.ok()
                                } else {
                                    new_message.reply(ctx, chunk).await.ok()
                                };
                                last_sent = sent;
                            }
                            log::info!("OpenAI replied to {}", new_message.author.name);
                        }
                    }
                }
                Err(e) => {
                    log::error!("OpenAI API error: {:?}", e);
                    let _ = new_message
                        .reply(ctx, "An error occurred while communicating with OpenAI")
                        .await;
                }
            }

            // Remove the "thinking" reaction now that we're done (success or error),
            // so the bot never leaves a 💭 hanging on the message.
            let _ = new_message
                .delete_reaction(&ctx.http, None, thinking_reaction)
                .await;
        }

        // Not addressed directly — consider a spontaneous random chime.
        if !mentioned && !replied {
            try_random_chime(ctx, data, new_message).await;
        }
    }
    Ok(())
}

#[tokio::main]
async fn main() {
    if std::env::var("RUST_LOG").is_err() {
        std::env::set_var(
            "RUST_LOG",
            "none,healthy_bot=info,poise=warn,serenity=error,sqlx=warn,tracing=error",
        );
    }

    let mut builder = env_logger::Builder::from_default_env();
    builder.format(|buf, record| {
        use chrono_tz::Europe::Amsterdam;
        use std::io::Write;
        let now = chrono::Utc::now().with_timezone(&Amsterdam);

        let level_color = match record.level() {
            log::Level::Error => "\x1b[31m",
            log::Level::Warn => "\x1b[33m",
            log::Level::Info => "\x1b[32m",
            _ => "\x1b[34m",
        };
        let reset = "\x1b[0m";

        writeln!(
            buf,
            "[{} {}{:5}{}] {:<20} | {}",
            now.format("%Y-%m-%d %H:%M:%S"),
            level_color,
            record.level(),
            reset,
            record.target(),
            record.args()
        )
    });
    builder.init();

    log::info!("🚀 Starting HealthyBot (Rust Rewrite)...");

    let token = std::env::var("DISCORD_TOKEN").expect("missing DISCORD_TOKEN");
    let db_url = std::env::var("DB_FILE").unwrap_or_else(|_| "healthybot.db".to_string());
    let openai_key = std::env::var("OPENAI_SECRET").expect("missing OPENAI_SECRET");

    log::info!("Connecting to database: {}", db_url);
    let connect_options = SqliteConnectOptions::from_str(&format!("sqlite:{}", db_url))
        .expect("Invalid DB_FILE path")
        .create_if_missing(true)
        // WAL lets readers and writers run concurrently; busy_timeout makes a
        // contended writer wait-and-retry instead of instantly failing with
        // "database is locked" on the write-heavy message hot path.
        .journal_mode(SqliteJournalMode::Wal)
        .busy_timeout(std::time::Duration::from_secs(5));
    let pool = SqlitePool::connect_with(connect_options)
        .await
        .expect("Failed to connect to database");

    sqlx::migrate!()
        .run(&pool)
        .await
        .expect("Failed to run database migrations");

    // Load initial settings cache
    let settings: Vec<db::Setting> = sqlx::query_as::<_, db::Setting>("SELECT k, v FROM setting")
        .fetch_all(&pool)
        .await
        .unwrap_or_default();
    let mut cache_map = HashMap::new();
    for s in settings {
        cache_map.insert(s.k, s.v);
    }
    let settings_cache = Arc::new(RwLock::new(cache_map));

    log::info!("Loading Markov repository...");
    let markov_repo = MarkovRepository::new(pool.clone(), "/healthybot/data/markovs").await;

    let openai_client = OpenAIClient::new(openai_key);

    let data = Data {
        db_pool: pool.clone(),
        markov_repo,
        openai_client,
        last_markov: tokio::sync::Mutex::new(
            std::time::Instant::now() - std::time::Duration::from_secs(3600),
        ),
        last_openai: tokio::sync::Mutex::new(
            std::time::Instant::now() - std::time::Duration::from_secs(3600),
        ),
        last_tldr: tokio::sync::Mutex::new(
            std::time::Instant::now() - std::time::Duration::from_secs(3600),
        ),
        last_random_chime: tokio::sync::Mutex::new(
            std::time::Instant::now() - std::time::Duration::from_secs(3600),
        ),
        chime_tally: tokio::sync::Mutex::new(chime::ChimeTally::new(
            chrono::Utc::now()
                .with_timezone(&chrono_tz::Europe::Amsterdam)
                .date_naive(),
        )),
        settings_cache: settings_cache.clone(),
    };

    let framework = poise::Framework::builder()
        .options(poise::FrameworkOptions {
            command_check: Some(|ctx: Context<'_>| {
                Box::pin(async move {
                    if ctx.author().bot {
                        let cache = ctx.data().settings_cache.read().await;
                        let is_allowed = cache
                            .get("allowed_bot_id")
                            .map(|id| id == &ctx.author().id.to_string())
                            .unwrap_or(false);
                        return Ok(is_allowed && ctx.command().name == "markov");
                    }
                    Ok(true)
                })
            }),
            owners: {
                let mut owners = std::collections::HashSet::new();
                owners.insert(serenity::UserId::new(210531463932674050));
                owners
            },
            commands: vec![
                commands::remind_cmd(),
                commands::markov(),
                commands::tldr(),
                commands::settings(),
                commands::user_cmd(),
                commands::memory(),
                commands::inthards(),
                commands::status(),
                commands::help(),
                commands::register(),
            ],
            event_handler: |ctx, event, framework, data| {
                Box::pin(event_handler(ctx, event, framework, data))
            },
            pre_command: |ctx| {
                Box::pin(async move {
                    let args = match ctx {
                        poise::Context::Prefix(p) => p.args.trim().to_string(),
                        poise::Context::Application(_) => "[Slash Command]".to_string(),
                    };
                    log::info!(
                        "INVOKE | user: {} | cmd: {} | args: {}",
                        ctx.author().name,
                        ctx.command().name,
                        if args.is_empty() { "-" } else { &args }
                    );
                })
            },
            post_command: |ctx| {
                Box::pin(async move {
                    log::info!(
                        "DONE   | user: {} | cmd: {}",
                        ctx.author().name,
                        ctx.command().name
                    );
                })
            },
            on_error: |error| {
                Box::pin(async move {
                    match error {
                        poise::FrameworkError::UnknownCommand { .. } => (),
                        poise::FrameworkError::Command { error, ctx, .. } => {
                            log::error!(
                                "Command error | user: {} | cmd: {} | error: {}",
                                ctx.author().name,
                                ctx.command().name,
                                error
                            );
                            // Only show the message to the user if it was raised as a
                            // UserError; anything else (DB/parse/internal failures) is
                            // logged above and replaced with a generic message so its
                            // raw text never leaks into chat.
                            let description = if error.downcast_ref::<UserError>().is_some() {
                                error.to_string()
                            } else {
                                "Something went wrong while running that command.".to_string()
                            };
                            let embed = serenity::builder::CreateEmbed::new()
                                .title("Error")
                                .description(description)
                                .color(0xFFFF00)
                                .footer(serenity::builder::CreateEmbedFooter::new("Healthy Bot"));
                            let _ = ctx.send(poise::CreateReply::default().embed(embed)).await;
                        }
                        poise::FrameworkError::ArgumentParse { error, ctx, .. } => {
                            let embed = serenity::builder::CreateEmbed::new()
                                .title("Invalid Arguments")
                                .description(error.to_string())
                                .color(0xFFFF00)
                                .footer(serenity::builder::CreateEmbedFooter::new("Healthy Bot"));
                            let _ = ctx.send(poise::CreateReply::default().embed(embed)).await;
                        }
                        _ => log::error!("Poise error: {:?}", error),
                    }
                })
            },
            prefix_options: poise::PrefixFrameworkOptions {
                prefix: Some("!".into()), // Default prefix
                ignore_bots: false,
                dynamic_prefix: Some(|ctx| {
                    Box::pin(async move {
                        let cache = ctx.data.settings_cache.read().await;
                        Ok(cache
                            .get("command_prefix")
                            .cloned()
                            .or(Some("!".to_string())))
                    })
                }),
                ..Default::default()
            },
            ..Default::default()
        })
        .setup(|ctx, ready, _framework| {
            Box::pin(async move {
                log::info!("Bot is logged in as {}", ready.user.tag());

                let guild_count = ready.guilds.len();
                log::info!("Active in {} guilds", guild_count);

                poise::builtins::register_globally(ctx, &_framework.options().commands).await?;
                log::info!("Slash commands registered globally");

                tasks::start_tasks(
                    data.db_pool.clone(),
                    ctx.http.clone(),
                    data.openai_client.clone(),
                )
                .await;
                log::info!("Background tasks started (Reminders, VC Updates, Memory)");

                // One-time database cleanup for legacy settings
                let pool = data.db_pool.clone();
                tokio::spawn(async move {
                    let res = sqlx::query(
                        "DELETE FROM setting WHERE k LIKE 'food_%' OR k = 'bday_announce_channel'",
                    )
                    .execute(&pool)
                    .await;
                    if let Ok(r) = res {
                        if r.rows_affected() > 0 {
                            log::info!(
                                "Cleaned up {} legacy settings from database.",
                                r.rows_affected()
                            );
                        }
                    }
                });

                // Load and set initial status
                let settings_cache_clone = data.settings_cache.clone();
                let ctx_clone = ctx.clone();
                tokio::spawn(async move {
                    let (status_type, status_msg) = {
                        let cache = settings_cache_clone.read().await;
                        (
                            cache.get("bot_status_type").cloned(),
                            cache.get("bot_status_message").cloned(),
                        )
                    };

                    if let (Some(t), Some(m)) = (status_type, status_msg) {
                        let activity = match t.as_str() {
                            "playing" => serenity::ActivityData::playing(&m),
                            "watching" => serenity::ActivityData::watching(&m),
                            "listening" => serenity::ActivityData::listening(&m),
                            "competing" => serenity::ActivityData::competing(&m),
                            _ => serenity::ActivityData::playing(&m),
                        };
                        ctx_clone.set_activity(Some(activity));
                        log::info!("Restored bot status: {} {}", t, m);
                    }
                });

                Ok(data)
            })
        })
        .build();

    let mut client = serenity::ClientBuilder::new(token, serenity::GatewayIntents::all())
        .framework(framework)
        .await
        .unwrap();

    let shard_manager = client.shard_manager.clone();

    tokio::spawn(async move {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};
            let mut sigterm = signal(SignalKind::terminate()).unwrap();
            let mut sigint = signal(SignalKind::interrupt()).unwrap();

            tokio::select! {
                _ = sigterm.recv() => log::info!("Received SIGTERM, shutting down..."),
                _ = sigint.recv() => log::info!("Received SIGINT, shutting down..."),
            }
        }

        #[cfg(not(unix))]
        {
            tokio::signal::ctrl_c().await.unwrap();
            log::info!("Received Ctrl-C, shutting down...");
        }

        shard_manager.shutdown_all().await;
    });

    log::info!("Starting client...");
    if let Err(why) = client.start().await {
        log::error!("Client error: {:?}", why);
    }
}

/// Gather stored facts for the message author and any users they mention, and
/// format them as a memory block to prepend to a system prompt. Returns `None`
/// when memory is disabled, everyone involved is opted out, or nothing is known.
async fn build_memory_prefix(data: &Data, new_message: &serenity::Message) -> Option<String> {
    let enabled = {
        let cache = data.settings_cache.read().await;
        cache
            .get("memory_enabled")
            .map(|v| v.trim().to_lowercase() != "false")
            .unwrap_or(true) // on by default
    };
    if !enabled {
        return None;
    }

    let mut people: Vec<(String, Vec<String>)> = Vec::new();
    let mut seen: std::collections::HashSet<u64> = std::collections::HashSet::new();
    for user in std::iter::once(&new_message.author).chain(new_message.mentions.iter()) {
        if !seen.insert(user.id.get()) {
            continue;
        }
        let id = user.id.to_string();
        if db::is_opted_out(&data.db_pool, &id).await {
            continue;
        }
        let facts: Vec<String> = db::get_facts(&data.db_pool, &id)
            .await
            .into_iter()
            .map(|f| f.fact)
            .collect();
        if !facts.is_empty() {
            people.push((user.name.clone(), facts));
        }
    }

    memory::format_memory_context(&people)
}

/// System prompt for the relevance pre-check — a cheap yes/no gate that runs
/// after the probability roll but before generating a full interjection.
const RELEVANCE_SYSTEM_PROMPT: &str =
    "You decide whether a chat bot should spontaneously chime into an ongoing \
     conversation. Given the recent messages, answer with only 'YES' if a short \
     interjection would be welcome and add value, or 'NO' otherwise. Prefer NO \
     when the conversation is private, sensitive, heated, or would not benefit \
     from a bot interjecting.";

/// Occasionally join a conversation on the bot's own initiative.
///
/// Called for human messages in which the bot was *not* mentioned or replied to.
/// The pure gate chain ([`chime::evaluate`]) decides whether to act based on the
/// enable flag, eligibility, a daily cap, a cooldown, and a probability roll.
/// When it fires, an optional relevance pre-check asks a cheap model whether an
/// interjection is actually warranted (failing closed) before the recent
/// conversation is fed to OpenAI and a standalone message is posted. All
/// failures are logged and swallowed — a chime is best-effort.
async fn try_random_chime(ctx: &serenity::Context, data: &Data, new_message: &serenity::Message) {
    let (settings, main_channel, prefix, base_prompt, chat_model, relevance_check) = {
        let cache = data.settings_cache.read().await;
        let settings = chime::ChimeSettings {
            chance: cache
                .get("random_chime_chance")
                .and_then(|v| v.parse::<f64>().ok())
                .unwrap_or(0.0),
            cooldown_secs: cache
                .get("random_chime_cooldown_seconds")
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(600),
            daily_cap: cache
                .get("random_chime_daily_cap")
                .and_then(|v| v.parse::<u32>().ok())
                .unwrap_or(0),
        };
        // A dedicated chime persona/model falls back to the shared AI settings.
        let base_prompt = cache
            .get("random_chime_prompt")
            .filter(|v| !v.trim().is_empty())
            .or_else(|| cache.get("ai_initial_prompt"))
            .cloned()
            .unwrap_or_else(|| "You are a helpful assistant.".to_string());
        let chat_model = cache
            .get("random_chime_model")
            .filter(|v| !v.trim().is_empty())
            .or_else(|| cache.get("ai_chat_model"))
            .cloned()
            .unwrap_or_else(|| "gpt-3.5-turbo".to_string());
        let relevance_check = cache
            .get("random_chime_relevance_check")
            .map(|v| v.trim().to_lowercase() != "false")
            .unwrap_or(true);
        (
            settings,
            cache.get("main_text_channel").cloned(),
            cache
                .get("command_prefix")
                .cloned()
                .unwrap_or_else(|| "!".to_string()),
            base_prompt,
            chat_model,
            relevance_check,
        )
    };

    let in_main_channel =
        main_channel.as_deref() == Some(new_message.channel_id.to_string().as_str());
    let today = chrono::Utc::now()
        .with_timezone(&chrono_tz::Europe::Amsterdam)
        .date_naive();

    // Evaluate the gate chain under the state locks. A fire consumes the cooldown
    // and records the daily-cap tally *now* — before the relevance check and the
    // reply — so both extra API calls stay bounded to ~one per cooldown window,
    // even when relevance often declines. `mentioned`/`replied` are already false
    // here (checked by the caller).
    let fire = {
        let mut last = data.last_random_chime.lock().await;
        let mut tally = data.chime_tally.lock().await;
        let inputs = chime::ChimeInputs {
            content: &new_message.content,
            prefix: &prefix,
            in_main_channel,
            mentioned: false,
            replied: false,
            elapsed_since_last: last.elapsed(),
            chimes_today: tally.count_for(today),
            roll: rand::random::<f64>(),
        };
        match chime::evaluate(&settings, &inputs) {
            chime::ChimeDecision::Fire => {
                *last = std::time::Instant::now();
                tally.record(today);
                true
            }
            chime::ChimeDecision::Skip(_) => false,
        }
    };
    if !fire {
        return;
    }

    let recent_messages = match new_message
        .channel_id
        .messages(&ctx.http, serenity::builder::GetMessages::new().limit(10))
        .await
    {
        Ok(msgs) => msgs,
        Err(e) => {
            log::error!("Random chime: failed to fetch recent messages: {:?}", e);
            return;
        }
    };

    let bot_id = ctx.cache.current_user().id;
    let entries: Vec<recent::RecentMessage> = recent_messages
        .iter()
        .map(|m| recent::RecentMessage {
            author_name: m.author.name.clone(),
            content: m.content_safe(&ctx.cache),
            is_bot: m.author.id == bot_id,
        })
        .collect();

    // Relevance pre-check: a cheap yes/no on whether an interjection is warranted.
    // Fails closed — an error, an empty transcript, or a "no" keeps the bot quiet.
    if relevance_check {
        let Some(transcript) = recent::build_transcript(&entries) else {
            return;
        };
        let judge = vec![
            ChatMessage {
                role: "system".to_string(),
                name: None,
                content: ChatMessageRequestContent::Text(RELEVANCE_SYSTEM_PROMPT.to_string()),
            },
            ChatMessage {
                role: "user".to_string(),
                name: None,
                content: ChatMessageRequestContent::Text(transcript),
            },
        ];
        let welcome = match data.openai_client.create_chat(&chat_model, judge).await {
            Ok(resp) => resp
                .choices
                .first()
                .map(|c| chime::is_affirmative(&c.message.content))
                .unwrap_or(false),
            Err(e) => {
                log::error!("Random chime: relevance check failed: {:?}", e);
                false
            }
        };
        if !welcome {
            log::info!("Random chime: relevance check declined; staying quiet.");
            return;
        }
    }

    let mut system_prompt = format!(
        "{}\n\nYou are spontaneously joining an ongoing conversation in a chat channel. \
         Keep your interjection short, casual, and relevant to what is being discussed. \
         Do not introduce yourself or explain that you are a bot.",
        base_prompt
    );
    if let Some(mem) = build_memory_prefix(data, new_message).await {
        system_prompt = format!("{}\n\n{}", system_prompt, mem);
    }
    let messages = recent::build_chat_messages(&system_prompt, &entries);

    log::info!(
        "Random chime triggered in channel {}",
        new_message.channel_id
    );
    let _typing = new_message.channel_id.start_typing(&ctx.http);

    match data.openai_client.create_chat(&chat_model, messages).await {
        Ok(response) => {
            if let Some(choice) = response.choices.first() {
                let content = choice.message.content.trim();
                if !content.is_empty() {
                    // Post as a standalone message (no reply-ping) so it reads as
                    // casually joining in. Chimes are meant to be short; truncate
                    // to Discord's 2000-char limit rather than splitting.
                    let _ = new_message
                        .channel_id
                        .say(&ctx.http, truncate_str(content, 2000).into_owned())
                        .await;
                }
            }
        }
        Err(e) => log::error!("Random chime OpenAI error: {:?}", e),
    }
}

async fn build_message_content(
    client: &reqwest::Client,
    text: &str,
    attachments: &[serenity::all::Attachment],
    supports_vision: bool,
) -> ChatMessageRequestContent {
    if !supports_vision {
        return ChatMessageRequestContent::Text(text.to_string());
    }

    let mut parts = Vec::new();
    if !text.is_empty() {
        parts.push(ContentPart::Text {
            text: text.to_string(),
        });
    }

    let mut image_count = 0;

    // 1. Process attachments
    for attachment in attachments {
        if image_count >= 2 {
            break;
        }

        let is_image = attachment
            .content_type
            .as_deref()
            .map(|ct| ct.starts_with("image/"))
            .unwrap_or_else(|| {
                let name = attachment.filename.to_lowercase();
                name.ends_with(".png")
                    || name.ends_with(".jpg")
                    || name.ends_with(".jpeg")
                    || name.ends_with(".webp")
                    || name.ends_with(".gif")
            });

        if is_image {
            match attachment.download().await {
                Ok(bytes) => {
                    use base64::{engine::general_purpose, Engine as _};
                    let base64_data = general_purpose::STANDARD.encode(&bytes);
                    let mime_type = attachment.content_type.as_deref().unwrap_or("image/jpeg");
                    let data_url = format!("data:{};base64,{}", mime_type, base64_data);
                    parts.push(ContentPart::ImageUrl {
                        image_url: ImageUrlTarget {
                            url: data_url,
                            detail: Some("low".to_string()),
                        },
                    });
                    image_count += 1;
                }
                Err(e) => {
                    log::error!(
                        "Failed to download attachment {}: {:?}",
                        attachment.filename,
                        e
                    );
                }
            }
        }
    }

    // 2. Process image URLs from text
    if image_count < 2 {
        static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        let regex = RE.get_or_init(|| {
            regex::Regex::new(r"(?i)https?://\S+?\.(?:png|jpg|jpeg|webp|gif)(?:\?\S*)?").unwrap()
        });

        for mat in regex.find_iter(text) {
            if image_count >= 2 {
                break;
            }

            let url = mat.as_str();
            match client.get(url).send().await {
                Ok(resp) => {
                    let content_type = resp
                        .headers()
                        .get(reqwest::header::CONTENT_TYPE)
                        .and_then(|val| val.to_str().ok())
                        .map(|s| s.to_string());

                    let is_image = content_type
                        .as_deref()
                        .map(|ct| ct.starts_with("image/"))
                        .unwrap_or(false);

                    if is_image {
                        match resp.bytes().await {
                            Ok(bytes) => {
                                use base64::{engine::general_purpose, Engine as _};
                                let base64_data = general_purpose::STANDARD.encode(&bytes);
                                let mime_type =
                                    content_type.unwrap_or_else(|| "image/jpeg".to_string());
                                let data_url = format!("data:{};base64,{}", mime_type, base64_data);
                                parts.push(ContentPart::ImageUrl {
                                    image_url: ImageUrlTarget {
                                        url: data_url,
                                        detail: Some("low".to_string()),
                                    },
                                });
                                image_count += 1;
                            }
                            Err(e) => {
                                log::error!("Failed to read bytes from image URL {}: {:?}", url, e);
                            }
                        }
                    }
                }
                Err(e) => {
                    log::error!("Failed to fetch image URL {}: {:?}", url, e);
                }
            }
        }
    }

    if parts.is_empty() {
        ChatMessageRequestContent::Text(text.to_string())
    } else if parts.len() == 1 {
        match parts.remove(0) {
            ContentPart::Text { text } => ChatMessageRequestContent::Text(text),
            other => ChatMessageRequestContent::Parts(vec![other]),
        }
    } else {
        ChatMessageRequestContent::Parts(parts)
    }
}
