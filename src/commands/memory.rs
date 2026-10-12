use super::{is_user_admin, resolve_user, Context, Error};
use crate::{create_embed, db, memory, truncate_str, user_error};
use poise::serenity_prelude as serenity;
use serenity::builder::CreateEmbedFooter;

/// Render a user's stored facts as an ephemeral embed.
pub async fn reply_facts(ctx: Context<'_>, discord_id: &str, self_view: bool) -> Result<(), Error> {
    let pool = &ctx.data().db_pool;
    let opted_out = db::is_opted_out(pool, discord_id).await;
    let facts = db::get_facts(pool, discord_id).await;

    let mut embed = create_embed("Memory");
    if opted_out {
        embed = embed.description(if self_view {
            "You are opted out of memory — nothing is stored about you. Use `optin` to re-enable."
        } else {
            "This user is opted out of memory."
        });
    } else if facts.is_empty() {
        embed = embed.description("No memories stored.");
    } else {
        let body = facts
            .iter()
            .enumerate()
            .map(|(i, f)| format!("{}. {}", i + 1, f.fact))
            .collect::<Vec<_>>()
            .join("\n");
        embed = embed.description(truncate_str(&body, 4000).into_owned());
        if self_view {
            embed = embed.footer(CreateEmbedFooter::new(
                "forget <number> deletes one · forget all clears them",
            ));
        }
    }
    ctx.send(poise::CreateReply::default().embed(embed).ephemeral(true))
        .await?;
    Ok(())
}

/// View or manage what the bot remembers about you
#[poise::command(
    slash_command,
    prefix_command,
    subcommands(
        "memory_view",
        "memory_forget_slash",
        "memory_forget_prefix",
        "memory_clear",
        "memory_optout",
        "memory_optin",
        "memory_insert",
        "memory_add"
    )
)]
pub async fn memory(ctx: Context<'_>) -> Result<(), Error> {
    reply_facts(ctx, &ctx.author().id.to_string(), true).await
}

/// View stored facts (yours, or another user's if you're an admin)
#[poise::command(slash_command, prefix_command, rename = "view")]
pub async fn memory_view(
    ctx: Context<'_>,
    #[description = "User to view (admin only)"] user: Option<serenity::User>,
) -> Result<(), Error> {
    match user {
        Some(u) => {
            if !is_user_admin(ctx).await {
                return Err(user_error(
                    "You are not authorized to view others' memories.",
                ));
            }
            reply_facts(ctx, &u.id.to_string(), false).await
        }
        None => reply_facts(ctx, &ctx.author().id.to_string(), true).await,
    }
}

pub async fn execute_forget(
    ctx: Context<'_>,
    target_user_id: &str,
    target_user_name: &str,
    target_is_self: bool,
    target_spec: &str,
) -> Result<(), Error> {
    let pool = &ctx.data().db_pool;
    let target_spec = target_spec.trim();

    if target_spec.eq_ignore_ascii_case("all") {
        let n = db::clear_facts(pool, target_user_id).await?;
        let msg = if target_is_self {
            format!("Cleared {} memory item(s).", n)
        } else {
            format!("Cleared {} memory item(s) for {}.", n, target_user_name)
        };
        ctx.send(
            poise::CreateReply::default()
                .content(msg)
                .ephemeral(true)
                .allowed_mentions(serenity::builder::CreateAllowedMentions::new().empty_users()),
        )
        .await?;
        return Ok(());
    }

    let facts = db::get_facts(pool, target_user_id).await;
    if facts.is_empty() {
        let msg = if target_is_self {
            "You have no memories stored."
        } else {
            "This user has no memories stored."
        };
        return Err(user_error(msg));
    }

    let indices = memory::parse_fact_indices(target_spec, facts.len()).map_err(user_error)?;

    let to_delete: Vec<&db::UserFact> = indices.iter().map(|&idx| &facts[idx - 1]).collect();
    let ids: Vec<i64> = to_delete.iter().map(|f| f.id).collect();
    let fact_texts: Vec<String> = to_delete.iter().map(|f| f.fact.clone()).collect();

    let _ = db::delete_facts_by_ids(pool, target_user_id, &ids).await?;

    let msg = if target_is_self {
        if fact_texts.len() == 1 {
            format!("Forgot: {}", fact_texts[0])
        } else {
            let list = fact_texts
                .iter()
                .map(|t| format!("• {}", t))
                .collect::<Vec<_>>()
                .join("\n");
            format!("Forgot {} items:\n{}", fact_texts.len(), list)
        }
    } else {
        if fact_texts.len() == 1 {
            format!("Forgot for {}: \"{}\"", target_user_name, fact_texts[0])
        } else {
            let list = fact_texts
                .iter()
                .map(|t| format!("• {}", t))
                .collect::<Vec<_>>()
                .join("\n");
            format!(
                "Forgot {} item(s) for {}:\n{}",
                fact_texts.len(),
                target_user_name,
                list
            )
        }
    };

    ctx.send(
        poise::CreateReply::default()
            .content(msg)
            .ephemeral(true)
            .allowed_mentions(serenity::builder::CreateAllowedMentions::new().empty_users()),
    )
    .await?;

    Ok(())
}

/// Forget stored facts by number (e.g. 1, 1 2, 2-4), or all of them
#[poise::command(slash_command, rename = "forget")]
pub async fn memory_forget_slash(
    ctx: Context<'_>,
    #[description = "'all', or numbers from view (e.g. '1', '1, 2', '2-4')"] target: String,
    #[description = "User whose facts to forget (admin only)"] user: Option<serenity::User>,
) -> Result<(), Error> {
    let (target_id, target_name, target_is_self) = match user {
        Some(u) => {
            if !is_user_admin(ctx).await {
                return Err(user_error(
                    "You are not authorized to forget others' memories.",
                ));
            }
            let is_self = u.id == ctx.author().id;
            (u.id.to_string(), u.name, is_self)
        }
        None => (ctx.author().id.to_string(), ctx.author().name.clone(), true),
    };

    execute_forget(ctx, &target_id, &target_name, target_is_self, &target).await
}

/// Forget stored facts by number (e.g. 1, 1 2, 2-4), or all of them
#[poise::command(prefix_command, rename = "forget")]
pub async fn memory_forget_prefix(
    ctx: Context<'_>,
    #[rest]
    #[description = "'all', or numbers like '1', '1, 2', '1 2 3', optionally with user if admin"]
    input: Option<String>,
) -> Result<(), Error> {
    let raw = input.unwrap_or_default();
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(user_error(
            "Usage: `forget <number(s)>` (e.g. `forget 1` or `forget 1, 2`) or `forget all`.",
        ));
    }

    let is_admin = is_user_admin(ctx).await;
    let tokens: Vec<&str> = raw.split_whitespace().collect();

    // Check if the first or last token is a user specification (admin only)
    if is_admin && tokens.len() >= 2 {
        // Try first token as user
        if let Ok((uid, name)) = resolve_user(ctx, tokens[0]).await {
            let spec = tokens[1..].join(" ");
            let is_self = uid == ctx.author().id.to_string();
            return execute_forget(ctx, &uid, &name, is_self, &spec).await;
        }
        // Try last token as user
        let last_idx = tokens.len() - 1;
        if let Ok((uid, name)) = resolve_user(ctx, tokens[last_idx]).await {
            let spec = tokens[..last_idx].join(" ");
            let is_self = uid == ctx.author().id.to_string();
            return execute_forget(ctx, &uid, &name, is_self, &spec).await;
        }
    }

    let self_id = ctx.author().id.to_string();
    let self_name = ctx.author().name.clone();
    execute_forget(ctx, &self_id, &self_name, true, raw).await
}

/// Clear all of a user's stored facts (admin only)
#[poise::command(slash_command, prefix_command, rename = "clear")]
pub async fn memory_clear(
    ctx: Context<'_>,
    #[description = "User whose facts to clear (admin only)"] user: Option<serenity::User>,
) -> Result<(), Error> {
    if !is_user_admin(ctx).await {
        return Err(user_error(
            "You are not authorized to clear others' memories.",
        ));
    }

    let (target_id, target_name) = match user {
        Some(u) => (u.id.to_string(), u.name),
        None => (ctx.author().id.to_string(), ctx.author().name.clone()),
    };

    let n = db::clear_facts(&ctx.data().db_pool, &target_id).await?;
    ctx.say(format!("Cleared {} memory item(s) for {}.", n, target_name))
        .await?;
    Ok(())
}

/// Opt out of memory storage (deletes all facts and stops future extraction)
#[poise::command(slash_command, prefix_command, rename = "optout")]
pub async fn memory_optout(ctx: Context<'_>) -> Result<(), Error> {
    let pool = &ctx.data().db_pool;
    let author_id = ctx.author().id.to_string();
    db::create_user_if_not_exists(pool, &author_id).await?;
    db::set_opt_out(pool, &author_id, true).await?;
    let deleted = db::clear_facts(pool, &author_id).await?;

    let msg = if deleted > 0 {
        format!(
            "You have opted out of memory. Deleted {} previously stored fact(s). No new facts will be recorded.",
            deleted
        )
    } else {
        "You have opted out of memory. No facts will be recorded about you.".to_string()
    };
    ctx.send(poise::CreateReply::default().content(msg).ephemeral(true))
        .await?;
    Ok(())
}

/// Opt back into memory storage
#[poise::command(slash_command, prefix_command, rename = "optin")]
pub async fn memory_optin(ctx: Context<'_>) -> Result<(), Error> {
    let pool = &ctx.data().db_pool;
    let author_id = ctx.author().id.to_string();
    db::create_user_if_not_exists(pool, &author_id).await?;
    db::set_opt_out(pool, &author_id, false).await?;

    ctx.send(
        poise::CreateReply::default()
            .content(
                "You have opted back into memory. The bot may record facts about you from chat.",
            )
            .ephemeral(true),
    )
    .await?;
    Ok(())
}

async fn handle_memory_insert(
    ctx: Context<'_>,
    user_id: &str,
    user_name: &str,
    is_bot: bool,
    fact: &str,
) -> Result<(), Error> {
    if !is_user_admin(ctx).await {
        return Err(user_error("Unauthorized"));
    }
    if is_bot {
        return Err(user_error("Cannot add memories for bot accounts."));
    }

    let pool = &ctx.data().db_pool;

    if db::is_opted_out(pool, user_id).await {
        return Err(user_error(format!("{} is opted out of memory.", user_name)));
    }

    let fact_trimmed = fact.trim();
    if fact_trimmed.is_empty() {
        return Err(user_error("Fact cannot be empty."));
    }
    if fact_trimmed.len() > 1000 {
        return Err(user_error("Fact must be under 1000 characters."));
    }

    let now_ms = chrono::Utc::now().timestamp_millis();
    db::create_user_if_not_exists(pool, user_id).await?;
    let inserted = db::add_fact(pool, user_id, fact_trimmed, now_ms).await?;

    let max_facts = ctx
        .data()
        .get_setting_u32("memory_max_facts_per_user", 20)
        .await;
    let _ = db::prune_facts(pool, user_id, max_facts).await;

    let content = if inserted {
        format!("Remembered for {}: \"{}\"", user_name, fact_trimmed)
    } else {
        format!("That fact is already remembered for {}.", user_name)
    };

    ctx.send(
        poise::CreateReply::default()
            .content(content)
            .ephemeral(true)
            .allowed_mentions(serenity::builder::CreateAllowedMentions::new().empty_users()),
    )
    .await?;

    Ok(())
}

/// Insert a fact for a user without notifying them (admin only)
#[poise::command(prefix_command, hide_in_help, rename = "insert")]
pub async fn memory_insert(
    ctx: Context<'_>,
    #[description = "The user (name, nickname, mention, or ID)"] target: String,
    #[rest]
    #[description = "The fact to remember about the user"]
    fact: String,
) -> Result<(), Error> {
    let (uid, name) = resolve_user(ctx, &target).await?;
    let is_bot = ctx
        .http()
        .get_user(serenity::UserId::new(uid.parse().unwrap_or(0)))
        .await
        .map(|u| u.bot)
        .unwrap_or(false);
    handle_memory_insert(ctx, &uid, &name, is_bot, &fact).await
}

/// Add a fact for a user without notifying them (admin only)
#[poise::command(prefix_command, hide_in_help, rename = "add")]
pub async fn memory_add(
    ctx: Context<'_>,
    #[description = "The user (name, nickname, mention, or ID)"] target: String,
    #[rest]
    #[description = "The fact to remember about the user"]
    fact: String,
) -> Result<(), Error> {
    let (uid, name) = resolve_user(ctx, &target).await?;
    let is_bot = ctx
        .http()
        .get_user(serenity::UserId::new(uid.parse().unwrap_or(0)))
        .await
        .map(|u| u.bot)
        .unwrap_or(false);
    handle_memory_insert(ctx, &uid, &name, is_bot, &fact).await
}

/// Remember a fact about a user without notifying them (admin only)
#[poise::command(
    slash_command,
    default_member_permissions = "ADMINISTRATOR",
    hide_in_help,
    rename = "remember"
)]
pub async fn remember_cmd(
    ctx: Context<'_>,
    #[description = "The user to remember a fact about"] user: serenity::User,
    #[rest]
    #[description = "The fact to remember about the user"]
    fact: String,
) -> Result<(), Error> {
    handle_memory_insert(ctx, &user.id.to_string(), &user.name, user.bot, &fact).await
}
