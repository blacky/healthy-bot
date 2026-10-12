use super::{is_server_admin, is_user_admin, Context, Error};
use crate::{create_embed, db, user_error};
use poise::serenity_prelude as serenity;

/// User and database management commands
#[poise::command(
    slash_command,
    prefix_command,
    rename = "user",
    aliases("users"),
    subcommands("authorize", "sync", "latest_message", "inthards")
)]
pub async fn user_cmd(ctx: Context<'_>) -> Result<(), Error> {
    ctx.say("Available subcommands: `authorize`, `sync`, `latest_message`, `inthards`.")
        .await?;
    Ok(())
}

/// Toggle a user's authorized status (admin only)
#[poise::command(slash_command, prefix_command)]
pub async fn authorize(
    ctx: Context<'_>,
    #[description = "The user to toggle authorization for"] user: serenity::User,
) -> Result<(), Error> {
    if !is_user_admin(ctx).await {
        return Err(user_error("Unauthorized"));
    }

    let pool = &ctx.data().db_pool;
    let target_id = user.id.to_string();
    db::create_user_if_not_exists(pool, &target_id).await?;
    sqlx::query("UPDATE users SET authorized = NOT authorized WHERE discord_id = ?")
        .bind(&target_id)
        .execute(pool)
        .await?;

    let target = db::get_user(pool, &target_id)
        .await
        .ok_or("Failed to load user after update")?;
    ctx.say(format!(
        "<@{}> is {} authorized.",
        target_id,
        if target.authorized {
            "now"
        } else {
            "no longer"
        }
    ))
    .await?;
    Ok(())
}

/// Sync guild members with the database (admin only)
#[poise::command(slash_command, prefix_command)]
pub async fn sync(ctx: Context<'_>) -> Result<(), Error> {
    if !is_user_admin(ctx).await {
        return Err(user_error("Unauthorized"));
    }

    let pool = &ctx.data().db_pool;
    let guild_id = ctx
        .guild_id()
        .ok_or_else(|| user_error("Must be run in a guild"))?;
    let mut synced_count = 0;

    let allowed_bot_id = ctx.data().get_setting("allowed_bot_id").await;

    let members = guild_id.members(ctx.http(), None, None).await?;
    for member in members {
        let is_allowed_bot = allowed_bot_id.as_deref() == Some(&member.user.id.to_string());

        if !member.user.bot || is_allowed_bot {
            let _ = db::create_user_if_not_exists(pool, &member.user.id.to_string()).await;
            synced_count += 1;
        } else {
            let _ = sqlx::query("DELETE FROM users WHERE discord_id = ?")
                .bind(member.user.id.to_string())
                .execute(pool)
                .await;
        }
    }

    ctx.say(format!("Synced {} members with database.", synced_count))
        .await?;
    Ok(())
}

/// Show a user's most recent message in the main channel (admin only)
#[poise::command(slash_command, prefix_command)]
pub async fn latest_message(
    ctx: Context<'_>,
    #[description = "The user to look up"] user: serenity::User,
) -> Result<(), Error> {
    if !is_user_admin(ctx).await {
        return Err(user_error("Unauthorized"));
    }

    let target_id = user.id.get();

    let channel_id_str = ctx
        .data()
        .get_setting("main_text_channel")
        .await
        .ok_or_else(|| user_error("main_text_channel setting not found"))?;
    let channel_id: u64 = channel_id_str.parse()?;

    let messages = serenity::ChannelId::new(channel_id)
        .messages(ctx.http(), serenity::builder::GetMessages::new().limit(100))
        .await?;

    if let Some(msg) = messages.iter().find(|m| m.author.id.get() == target_id) {
        ctx.say(format!(
            "<@{}>'s latest message was: {} at <t:{}>",
            target_id,
            msg.content_safe(ctx.cache()),
            msg.timestamp.unix_timestamp()
        ))
        .await?;
    } else {
        ctx.say(format!(
            "No message found for member <@{}> in the last 100 messages.",
            target_id
        ))
        .await?;
    }
    Ok(())
}

/// Show the most inactive users
#[poise::command(slash_command, prefix_command)]
pub async fn inthards(
    ctx: Context<'_>,
    #[description = "How many users to show (default 5)"] count: Option<i64>,
) -> Result<(), Error> {
    let pool = &ctx.data().db_pool;
    let top_x = count.unwrap_or(5).unsigned_abs() as i64;

    let users: Vec<db::User> =
        sqlx::query_as::<_, db::User>("SELECT * FROM users ORDER BY last_message ASC LIMIT ?")
            .bind(top_x)
            .fetch_all(pool)
            .await?;

    let mut embed =
        create_embed("Inthards Top List").description(format!("Top {} inactive inters", top_x));

    for (idx, user) in users.iter().enumerate() {
        let ts = user.last_message_utc().map(|m| m.timestamp()).unwrap_or(0);
        embed = embed.field(
            format!("{}.", idx + 1),
            format!("<@{}>: <t:{}:R>", user.discord_id, ts),
            false,
        );
    }
    ctx.send(poise::CreateReply::default().embed(embed)).await?;
    Ok(())
}

fn format_tokens(n: i64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{}k", n / 1_000)
    } else {
        n.to_string()
    }
}

/// View API token usage leaderboard.
#[poise::command(
    slash_command,
    prefix_command,
    rename = "usage_leaderboard",
    guild_only
)]
pub async fn usage_leaderboard(ctx: Context<'_>) -> Result<(), Error> {
    if !is_server_admin(ctx).await {
        return Err(user_error(
            "Only server administrators can view the usage leaderboard.",
        ));
    }

    let usage_list = db::get_usage_leaderboard(&ctx.data().db_pool, 10).await?;

    if usage_list.is_empty() {
        let embed = create_embed("API Usage Leaderboard").description("No API usage recorded yet.");
        ctx.send(poise::CreateReply::default().embed(embed)).await?;
        return Ok(());
    }

    let mut lines = Vec::new();
    for (i, row) in usage_list.iter().enumerate() {
        let total = row.chat_tokens + row.memory_tokens;
        let formatted_total = format_tokens(total);
        let formatted_chat = format_tokens(row.chat_tokens);
        let formatted_mem = format_tokens(row.memory_tokens);

        let restricted_flag = if row.is_restricted {
            " 🚫 [Restricted]"
        } else {
            ""
        };

        lines.push(format!(
            "{}. <@{}> - Total: {} (Chat: {} | Mem: {}){}",
            i + 1,
            row.user_id,
            formatted_total,
            formatted_chat,
            formatted_mem,
            restricted_flag
        ));
    }

    let embed = create_embed("API Usage Leaderboard").description(lines.join("\n"));
    ctx.send(poise::CreateReply::default().embed(embed)).await?;
    Ok(())
}

/// Restrict or unrestrict a user from accessing AI features.
#[poise::command(slash_command, prefix_command, rename = "restrict", guild_only)]
pub async fn restrict(
    ctx: Context<'_>,
    #[description = "The target user to restrict or unrestrict"] target: serenity::User,
    #[description = "Set to true to restrict, false to unrestrict"] restricted: bool,
) -> Result<(), Error> {
    if !is_server_admin(ctx).await {
        return Err(user_error(
            "Only server administrators can restrict users from using AI.",
        ));
    }

    let user_id = target.id.to_string();
    db::set_user_restricted(&ctx.data().db_pool, &user_id, restricted).await?;

    let action_str = if restricted {
        "restricted from"
    } else {
        "unrestricted for"
    };
    ctx.send(
        poise::CreateReply::default()
            .content(format!(
                "User <@{}> has been {} AI access.",
                user_id, action_str
            ))
            .ephemeral(true),
    )
    .await?;

    Ok(())
}

/// Register slash commands (Owner only)
#[poise::command(prefix_command, hide_in_help)]
pub async fn register(ctx: Context<'_>) -> Result<(), Error> {
    let is_owner = ctx
        .guild()
        .map(|g| g.owner_id == ctx.author().id)
        .unwrap_or(false);
    let is_authorized_user = ctx.author().id.get() == 210531463932674050;

    if !is_owner && !is_authorized_user {
        return Err(user_error(
            "Only the server owner or authorized administrators can register commands.",
        ));
    }

    poise::builtins::register_application_commands_buttons(ctx).await?;
    Ok(())
}
