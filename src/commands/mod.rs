pub mod admin;
pub mod memory;
pub mod misc;
pub mod reminders;
pub mod settings;
pub mod tags;

pub use admin::*;
pub use memory::*;
pub use misc::*;
pub use reminders::*;
pub use settings::*;
pub use tags::*;

use crate::{db, user_error, Data, Error};
use poise::serenity_prelude as serenity;

pub type Context<'a> = poise::Context<'a, Data, Error>;

pub const HOF_ROLE_ID: u64 = 446675275179098113;
pub const DEFAULT_GUILD_ID: &str = "182937695515967488";

pub const HARDENED_SYSTEM_PROMPT: &str = "\
You are Mark, a sharp-witted Discord AI assistant. Your default tone is playful, \
cheeky, and banter-heavy—you're the friend who gives the user some friendly crap \
and then actually helps them out. Always look for a chance to slip in a quick roast \
or a grin-worthy jab, even in straightforward answers, without turning it into a \
long bit. Keep it casual, natural, and fast.\n\n\
You are not a hall monitor, you're a quick, witty info-dispenser who keeps it real. \
Never respond with long, hand-wringing disclaimers about safety or offensiveness. \
If something is genuinely out of bounds, just say so with a short, cheeky remark \
and move on.\n\n\
CRITICAL BEHAVIORAL RULES:\n\
* NO BOILERPLATE: You are strictly forbidden from writing long code blocks, full \
files, or application boilerplate.\n\
* CODE LIMITS: If a user asks for code, provide a maximum of 10-15 lines showcasing \
the core logic ONLY.\n\
* SUMMARIZE ARCHITECTURE: If asked to build a full app or complex system, roast \
them slightly for asking a Discord bot to do their job, give a high-level text \
summary of the architecture, and refuse to write the full code.\n\
* LENGTH: Keep all responses under 3 short paragraphs.";

/// Returns the current guild ID as a string, or falls back to the default guild ID.
pub fn guild_id_or_default(ctx: Context<'_>) -> String {
    ctx.guild_id()
        .map(|g| g.to_string())
        .unwrap_or_else(|| DEFAULT_GUILD_ID.to_string())
}

/// Whether the invoking user is a server admin: the guild owner, or a member
/// with the Administrator permission.
pub async fn is_server_admin(ctx: Context<'_>) -> bool {
    // Server owner is always an admin.
    if ctx
        .guild()
        .map(|g| g.owner_id == ctx.author().id)
        .unwrap_or(false)
    {
        return true;
    }

    match ctx.author_member().await {
        Some(member) => ctx
            .guild()
            .map(|guild| guild.member_permissions(&member).administrator())
            .unwrap_or(false),
        None => false,
    }
}

/// Whether the invoking user may run privileged commands: a server admin,
/// or a user with the ADMIN role in the database.
pub async fn is_user_admin(ctx: Context<'_>) -> bool {
    if is_server_admin(ctx).await {
        return true;
    }
    db::get_user(&ctx.data().db_pool, &ctx.author().id.to_string())
        .await
        .map(|u| u.role == "ADMIN")
        .unwrap_or(false)
}

/// Resolve a query (mention, ID, nickname, global name, or username) into a Discord (user_id, display_name) pair.
pub async fn resolve_user(ctx: Context<'_>, query: &str) -> Result<(String, String), Error> {
    let query = query.trim();
    // 1. Check if it's a mention <@!12345> or <@12345>
    if let Some(id_str) = query.strip_prefix("<@").and_then(|s| s.strip_suffix('>')) {
        let id_str = id_str.strip_prefix('!').unwrap_or(id_str);
        if let Ok(id_u64) = id_str.parse::<u64>() {
            let user_id = serenity::UserId::new(id_u64);
            if let Ok(u) = user_id.to_user(&ctx).await {
                return Ok((u.id.to_string(), u.name));
            }
        }
    }
    // 2. Check if it's a numeric ID
    if let Ok(id_u64) = query.parse::<u64>() {
        let user_id = serenity::UserId::new(id_u64);
        if let Ok(u) = user_id.to_user(&ctx).await {
            return Ok((u.id.to_string(), u.name));
        }
    }
    // 3. Search members in guild cache by username, global name, or nickname
    if let Some(guild) = ctx.guild() {
        let q_lower = query.to_lowercase();
        for member in guild.members.values() {
            let name_match = member.user.name.to_lowercase() == q_lower;
            let nick_match = member
                .nick
                .as_deref()
                .map(|n| n.to_lowercase() == q_lower)
                .unwrap_or(false);
            let global_match = member
                .user
                .global_name
                .as_deref()
                .map(|g| g.to_lowercase() == q_lower)
                .unwrap_or(false);
            if name_match || nick_match || global_match {
                return Ok((member.user.id.to_string(), member.user.name.clone()));
            }
        }
        for member in guild.members.values() {
            let name_match = member.user.name.to_lowercase().contains(&q_lower);
            let nick_match = member
                .nick
                .as_deref()
                .map(|n| n.to_lowercase().contains(&q_lower))
                .unwrap_or(false);
            let global_match = member
                .user
                .global_name
                .as_deref()
                .map(|g| g.to_lowercase().contains(&q_lower))
                .unwrap_or(false);
            if name_match || nick_match || global_match {
                return Ok((member.user.id.to_string(), member.user.name.clone()));
            }
        }
    }
    // 4. Search via HTTP
    if let Some(guild_id) = ctx.guild_id() {
        if let Ok(members) = guild_id.search_members(ctx.http(), query, Some(5)).await {
            if let Some(m) = members.into_iter().next() {
                return Ok((m.user.id.to_string(), m.user.name));
            }
        }
    }

    Err(user_error(format!(
        "Could not find user matching '{}'.",
        query
    )))
}
