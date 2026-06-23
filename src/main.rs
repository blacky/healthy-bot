use healthy_bot::markov::MarkovRepository;
use healthy_bot::openai::{sanitize_name, ChatMessage, OpenAIClient};
use healthy_bot::{commands, db, tasks, Data, Error, UserError};
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

            let bot_name = ctx.cache.current_user().name.clone();
            let bot_id = ctx.cache.current_user().id;

            // Build conversation context by walking up the reply chain. Discord
            // only includes the immediate parent inline (`referenced_message`);
            // deeper ancestors come back with an empty `referenced_message`, so
            // each one must be fetched explicitly via its `message_reference`
            // pointer. Capped at 10 hops for token/rate-limit safety.
            let mut messages = Vec::new();
            let mut parent = new_message.referenced_message.as_deref().cloned();
            for _ in 0..10 {
                let Some(msg) = parent else {
                    break;
                };

                let role = if msg.author.id == bot_id {
                    "assistant"
                } else {
                    "user"
                };
                messages.push(ChatMessage {
                    role: role.to_string(),
                    name: Some(sanitize_name(&msg.author.name)),
                    content: msg.content.clone(),
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

            // Add the developer prompt at the very beginning
            messages.insert(
                0,
                ChatMessage {
                    role: "developer".to_string(),
                    name: Some(sanitize_name(&bot_name)),
                    content: bot_context,
                },
            );

            let prompt = new_message.content.trim();
            if prompt.is_empty() {
                return Ok(());
            }

            messages.push(ChatMessage {
                role: "user".to_string(),
                name: Some(sanitize_name(&new_message.author.name)),
                content: prompt.to_string(),
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
                        "  [{}] Role: {} | Name: {:?} | Content: {}",
                        i,
                        msg.role,
                        msg.name,
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
                            let mut start = 0;
                            while start < content.len() {
                                let mut end = std::cmp::min(start + 2000, content.len());
                                while !content.is_char_boundary(end) {
                                    end -= 1;
                                }
                                let chunk = &content[start..end];
                                let _ = new_message.channel_id.say(ctx, chunk).await;
                                start = end;
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

    db::init_schema(&pool)
        .await
        .expect("Failed to initialize database schema");

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
                commands::settings(),
                commands::user_cmd(),
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

                tasks::start_tasks(data.db_pool.clone(), ctx.http.clone()).await;
                log::info!("Background tasks started (Reminders, VC Updates)");

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
