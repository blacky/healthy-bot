use healthy_bot::chime;
use healthy_bot::events;
use healthy_bot::markov::MarkovRepository;
use healthy_bot::openai::OpenAIClient;
use healthy_bot::{commands, db, tasks, Data, Error, UserError};
use poise::serenity_prelude as serenity;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool};
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::RwLock;

type Context<'a> = poise::Context<'a, Data, Error>;

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

    match db::seed_tags_from_file_if_present(&pool).await {
        Ok(Some(path)) => log::info!("Successfully seeded tags from file: {}", path.display()),
        Ok(None) => {}
        Err(e) => log::error!("Failed to seed tags from file: {}", e),
    }

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
        last_tag_channel: tokio::sync::Mutex::new(HashMap::new()),
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
                commands::remember_cmd(),
                commands::inthards(),
                commands::status(),
                commands::help(),
                commands::register(),
                commands::usage_leaderboard(),
                commands::restrict(),
                commands::t_cmd(),
                commands::tag_slash(),
            ],
            event_handler: |ctx, event, framework, data| {
                Box::pin(events::event_handler(ctx, event, framework, data))
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
                edit_tracker: Some(Arc::new(poise::EditTracker::for_timespan(
                    std::time::Duration::from_secs(60),
                ))),
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
