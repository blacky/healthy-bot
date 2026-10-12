use super::{guild_id_or_default, is_server_admin, resolve_user, Context, Error};
use crate::pagination::{paginate, paginate_async};
use crate::{db, split_message, tags, truncate_str, user_error};
use chrono::Utc;
use chrono_tz::Europe::Amsterdam;
use poise::futures_util::StreamExt;
use poise::serenity_prelude as serenity;
use serenity::builder::{CreateEmbed, CreateEmbedFooter};
use std::sync::LazyLock;

static ATTACH_REGEX: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r"\{attach:(https?://(?:cdn\.discordapp\.com|media\.discordapp\.net)/attachments/[^}]+)\}",
    )
    .unwrap()
});

static URL_REGEX: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r"https?://(?:cdn\.discordapp\.com|media\.discordapp\.net)/attachments/[^\s<>]+",
    )
    .unwrap()
});

pub fn get_tag_assets_dir() -> std::path::PathBuf {
    if let Ok(dir) = std::env::var("TAG_ASSETS_DIR") {
        std::path::PathBuf::from(dir)
    } else if std::path::Path::new("/healthybot/data").exists() {
        std::path::PathBuf::from("/healthybot/data/tag_assets")
    } else {
        std::path::PathBuf::from("data/tag_assets")
    }
}

pub fn validate_tag_name(name: &str) -> Result<(), Error> {
    if name.len() > 100 {
        return Err(user_error("Tag name is too long (max 100 characters)."));
    }
    if name.chars().any(char::is_whitespace) {
        return Err(user_error(
            "Tag names must be a single word without spaces.",
        ));
    }
    Ok(())
}

async fn save_attachment(attachment: &serenity::all::Attachment, filename: &str) -> Option<String> {
    let target_path = get_tag_assets_dir().join(filename);
    if let Ok(bytes) = attachment.download().await {
        tokio::fs::create_dir_all(get_tag_assets_dir()).await.ok();
        if tokio::fs::write(&target_path, &bytes).await.is_ok() {
            return Some(filename.to_string());
        }
    }
    None
}

async fn download_discord_cdn_url(
    client: &reqwest::Client,
    url: &str,
    tag_id: &str,
) -> Option<String> {
    let resp = client
        .get(url)
        .header(
            reqwest::header::USER_AGENT,
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64)",
        )
        .send()
        .await
        .ok()?;

    if !resp.status().is_success() {
        return None;
    }

    let bytes = resp.bytes().await.ok()?;

    let path_part = url.split('?').next().unwrap_or(url);
    let ext = path_part
        .split('.')
        .next_back()
        .unwrap_or("png")
        .split('/')
        .next()
        .unwrap_or("png");
    let ext = if ext.len() > 5 || ext.is_empty() {
        "png"
    } else {
        ext
    };

    let filename = format!("{}.{}", tag_id, ext);
    let target_path = get_tag_assets_dir().join(&filename);

    tokio::fs::create_dir_all(get_tag_assets_dir()).await.ok()?;
    tokio::fs::write(&target_path, &bytes).await.ok()?;

    Some(filename)
}

async fn extract_and_download_discord_cdn_asset(
    client: &reqwest::Client,
    content: &str,
    tag_id: &str,
) -> (String, Option<String>) {
    // 1. Check for {attach:https://cdn.discordapp.com/attachments/...}
    if let Some(caps) = ATTACH_REGEX.captures(content) {
        if let Some(m) = caps.get(1) {
            if let Some(filename) = download_discord_cdn_url(client, m.as_str(), tag_id).await {
                let cleaned = ATTACH_REGEX.replace(content, "").trim().to_string();
                return (cleaned, Some(filename));
            }
        }
    }

    // 2. Check for bare Discord CDN attachment URL in content
    if let Some(m) = URL_REGEX.find(content) {
        if let Some(filename) = download_discord_cdn_url(client, m.as_str(), tag_id).await {
            let cleaned = URL_REGEX.replace(content, "").trim().to_string();
            return (cleaned, Some(filename));
        }
    }

    (content.to_string(), None)
}

pub async fn autocomplete_tag_name(ctx: Context<'_>, partial: &str) -> Vec<String> {
    let server_id = guild_id_or_default(ctx);
    db::search_tag_names(&ctx.data().db_pool, &server_id, partial, 25).await
}

async fn send_tag(
    ctx: Context<'_>,
    tag: &db::Tag,
    args: &[String],
    is_random: bool,
) -> Result<(), Error> {
    let cooldown_secs = ctx.data().get_setting_u64("tag_cooldown_seconds", 5).await;

    let is_edit = match ctx {
        poise::Context::Prefix(p) => p.trigger == poise::MessageDispatchTrigger::MessageEdit,
        poise::Context::Application(_) => false,
    };

    let channel_id = ctx.channel_id();
    let decision = {
        let mut channels = ctx.data().last_tag_channel.lock().await;
        tags::check_and_update_tag_cooldown(
            &mut channels,
            channel_id,
            cooldown_secs,
            is_edit,
            std::time::Instant::now(),
        )
    };

    match decision {
        tags::TagCooldownDecision::Permitted => {}
        tags::TagCooldownDecision::RejectAndWarn { remaining_secs } => {
            match ctx {
                poise::Context::Prefix(p) => {
                    let _ = p
                        .msg
                        .react(
                            ctx.http(),
                            serenity::all::ReactionType::Unicode("⏳".to_string()),
                        )
                        .await;
                    let _ = ctx
                        .say(format!(
                            "⏳ Tag command is on cooldown. Please wait {remaining_secs}s."
                        ))
                        .await;
                }
                poise::Context::Application(_) => {
                    let _ = ctx
                        .send(
                            poise::CreateReply::default()
                                .content(format!(
                                    "⏳ Tag command is on cooldown in this channel. Please wait {remaining_secs}s."
                                ))
                                .ephemeral(true),
                        )
                        .await;
                }
            }
            return Ok(());
        }
        tags::TagCooldownDecision::RejectSilently => {
            return Ok(());
        }
    }

    let pool = &ctx.data().db_pool;
    let _ = db::increment_tag_uses(pool, &tag.id, Utc::now().timestamp_millis()).await;

    let tag_ctx = tags::TagContext {
        author_mention: format!("<@{}>", ctx.author().id),
        author_name: ctx.author().name.clone(),
        args,
    };
    let evaluated = tags::evaluate_tag(&tag.content, &tag_ctx, tag.asset_filename.as_deref());

    let attachment = if let Some(filename) = &evaluated.asset_filename {
        let asset_path = get_tag_assets_dir().join(filename);
        if asset_path.exists() {
            match serenity::all::CreateAttachment::path(&asset_path).await {
                Ok(att) => Some(att),
                Err(e) => {
                    log::error!("Failed to attach file {:?}: {}", asset_path, e);
                    None
                }
            }
        } else {
            None
        }
    } else {
        None
    };

    let text = if is_random {
        if evaluated.text.is_empty() {
            format!("Showing tag: {}", tag.name)
        } else {
            format!("Showing tag: {}\n\n{}", tag.name, evaluated.text)
        }
    } else {
        evaluated.text
    };

    if text.len() <= 2000 {
        let mut reply = poise::CreateReply::default();
        if !text.is_empty() {
            reply = reply.content(&text);
        }
        if let Some(att) = attachment {
            reply = reply.attachment(att);
        }
        ctx.send(reply).await?;
    } else {
        let chunks = split_message(&text, 2000);
        let mut first = true;
        for chunk in chunks {
            let mut reply = poise::CreateReply::default().content(chunk);
            if first {
                if let Some(att) = attachment.clone() {
                    reply = reply.attachment(att);
                }
                first = false;
            }
            ctx.send(reply).await?;
        }
    }

    Ok(())
}

async fn handle_tag_create_core(
    ctx: Context<'_>,
    server_id: &str,
    tag_name: &str,
    raw_content: &str,
    attachment: Option<&serenity::all::Attachment>,
) -> Result<(), Error> {
    validate_tag_name(tag_name)?;
    let pool = &ctx.data().db_pool;

    if db::get_tag(pool, server_id, tag_name).await.is_some() {
        return Err(user_error(format!(
            "A tag named '{}' already exists.",
            tag_name
        )));
    }

    let tag_id = Utc::now().timestamp_nanos_opt().unwrap_or(0).to_string();
    let mut asset_filename = None;

    if let Some(att) = attachment {
        let ext = att.filename.split('.').next_back().unwrap_or("png");
        let filename = format!("{}.{}", tag_id, ext);
        asset_filename = save_attachment(att, &filename).await;
    }

    let (content, cdn_asset) = if asset_filename.is_none() {
        extract_and_download_discord_cdn_asset(
            &ctx.data().openai_client.client,
            raw_content,
            &tag_id,
        )
        .await
    } else {
        (raw_content.to_string(), None)
    };
    if let Some(a) = cdn_asset {
        asset_filename = Some(a);
    }

    if content.is_empty() && asset_filename.is_none() {
        return Err(user_error("Tag must have content or an attachment."));
    }

    let new_tag = db::Tag {
        id: tag_id,
        server_id: server_id.to_string(),
        name: tag_name.to_string(),
        name_normalized: tag_name.trim().to_lowercase(),
        content,
        asset_filename,
        author_id: ctx.author().id.to_string(),
        author_name: ctx.author().name.clone(),
        created_at: Utc::now().timestamp_millis(),
        last_used_at: None,
        uses: 0,
    };

    db::create_tag(pool, &new_tag).await?;
    ctx.say(format!("Tag '{}' created!", tag_name)).await?;
    Ok(())
}

async fn handle_tag_edit_core(
    ctx: Context<'_>,
    server_id: &str,
    tag_name: &str,
    new_raw_content: Option<&str>,
    attachment: Option<&serenity::all::Attachment>,
) -> Result<(), Error> {
    validate_tag_name(tag_name)?;
    let pool = &ctx.data().db_pool;

    let Some(tag) = db::get_tag(pool, server_id, tag_name).await else {
        return Err(user_error(format!("Tag '{}' not found.", tag_name)));
    };

    let is_admin = is_server_admin(ctx).await;
    let is_author = tag.author_id == ctx.author().id.to_string();
    if !is_admin && !is_author {
        return Err(user_error(
            "You do not have permission to edit this tag (only the author or a server admin can edit it).",
        ));
    }

    let mut new_asset_filename = tag.asset_filename.clone();
    let mut uploaded_file = false;

    if let Some(att) = attachment {
        let ext = att.filename.split('.').next_back().unwrap_or("png");
        let filename = format!("{}_{}.{}", tag.id, Utc::now().timestamp_millis(), ext);
        if let Some(saved) = save_attachment(att, &filename).await {
            new_asset_filename = Some(saved);
            uploaded_file = true;
        }
    }

    let raw_content = new_raw_content.unwrap_or(&tag.content);
    let (new_content, cdn_asset) = if !uploaded_file {
        extract_and_download_discord_cdn_asset(
            &ctx.data().openai_client.client,
            raw_content,
            &tag.id,
        )
        .await
    } else {
        (raw_content.to_string(), None)
    };
    if let Some(a) = cdn_asset {
        new_asset_filename = Some(a);
    }

    if new_content.is_empty() && new_asset_filename.is_none() {
        return Err(user_error("Tag must have content or an attachment."));
    }

    db::update_tag(pool, &tag.id, &new_content, new_asset_filename.as_deref()).await?;
    ctx.say(format!("Tag '{}' updated!", tag.name)).await?;
    Ok(())
}

async fn handle_tag_rename_core(
    ctx: Context<'_>,
    server_id: &str,
    old_name: &str,
    new_name: &str,
) -> Result<(), Error> {
    validate_tag_name(new_name)?;
    let pool = &ctx.data().db_pool;

    let Some(tag) = db::get_tag(pool, server_id, old_name).await else {
        return Err(user_error(format!("Tag '{}' not found.", old_name)));
    };

    let is_admin = is_server_admin(ctx).await;
    let is_author = tag.author_id == ctx.author().id.to_string();
    if !is_admin && !is_author {
        return Err(user_error(
            "You do not have permission to rename this tag (only the author or a server admin can rename it).",
        ));
    }

    if old_name.trim().to_lowercase() != new_name.trim().to_lowercase()
        && db::get_tag(pool, server_id, new_name).await.is_some()
    {
        return Err(user_error(format!(
            "A tag named '{}' already exists.",
            new_name
        )));
    }

    db::rename_tag(pool, &tag.id, new_name).await?;
    ctx.say(format!("Tag '{}' renamed to '{}'.", tag.name, new_name))
        .await?;
    Ok(())
}

async fn handle_tag_alias_core(
    ctx: Context<'_>,
    server_id: &str,
    existing_name: &str,
    alias_name: &str,
) -> Result<(), Error> {
    validate_tag_name(alias_name)?;
    let pool = &ctx.data().db_pool;

    let Some(existing_tag) = db::get_tag(pool, server_id, existing_name).await else {
        return Err(user_error(format!("Tag '{}' not found.", existing_name)));
    };

    if db::get_tag(pool, server_id, alias_name).await.is_some() {
        return Err(user_error(format!(
            "A tag named '{}' already exists.",
            alias_name
        )));
    }

    let tag_id = Utc::now().timestamp_nanos_opt().unwrap_or(0).to_string();
    let alias_tag = db::Tag {
        id: tag_id,
        server_id: server_id.to_string(),
        name: alias_name.to_string(),
        name_normalized: alias_name.trim().to_lowercase(),
        content: existing_tag.content.clone(),
        asset_filename: existing_tag.asset_filename.clone(),
        author_id: ctx.author().id.to_string(),
        author_name: ctx.author().name.clone(),
        created_at: Utc::now().timestamp_millis(),
        last_used_at: None,
        uses: 0,
    };

    db::create_tag(pool, &alias_tag).await?;
    ctx.say(format!(
        "Created alias '{}' for tag '{}'.",
        alias_name, existing_tag.name
    ))
    .await?;
    Ok(())
}

async fn handle_tag_delete(ctx: Context<'_>, server_id: &str, tag_name: &str) -> Result<(), Error> {
    let pool = &ctx.data().db_pool;
    let Some(tag) = db::get_tag(pool, server_id, tag_name).await else {
        return Err(user_error(format!("Tag '{}' not found.", tag_name)));
    };

    let is_admin = is_server_admin(ctx).await;
    let is_author = tag.author_id == ctx.author().id.to_string();

    if !is_admin && !is_author {
        return Err(user_error(
            "You do not have permission to delete this tag (only the author or a server admin can delete it).",
        ));
    }

    // If deleting someone else's tag, require admin confirmation via buttons
    if !is_author {
        let embed = CreateEmbed::new()
            .title("⚠️ Warning: Force Delete Tag")
            .description(format!(
                "Are you sure you want to forcefully delete tag **{}**?\n\n**Owner:** <@{}> ({})\n**Uses:** {}",
                tag.name, tag.author_id, tag.author_name, tag.uses
            ))
            .color(0xFF4444)
            .footer(CreateEmbedFooter::new("Healthy Bot"));

        let components = vec![serenity::CreateActionRow::Buttons(vec![
            serenity::CreateButton::new("confirm_force_delete")
                .label("Force Delete")
                .style(serenity::ButtonStyle::Danger),
            serenity::CreateButton::new("cancel_force_delete")
                .label("Cancel")
                .style(serenity::ButtonStyle::Secondary),
        ])];

        let reply = ctx
            .send(
                poise::CreateReply::default()
                    .embed(embed)
                    .components(components),
            )
            .await?;

        let mut collector = reply
            .message()
            .await?
            .await_component_interactions(ctx)
            .author_id(ctx.author().id)
            .timeout(std::time::Duration::from_secs(60))
            .stream();

        if let Some(interaction) = collector.next().await {
            if interaction.data.custom_id == "confirm_force_delete" {
                if let Some(filename) = &tag.asset_filename {
                    db::delete_tag_asset_if_unused(pool, filename, &tag.id, &get_tag_assets_dir())
                        .await;
                }
                db::delete_tag(pool, &tag.id).await?;
                let _ = interaction
                    .create_response(
                        ctx,
                        serenity::CreateInteractionResponse::UpdateMessage(
                            serenity::CreateInteractionResponseMessage::new()
                                .content(format!("Tag '{}' was forcefully deleted.", tag.name))
                                .embeds(vec![])
                                .components(vec![]),
                        ),
                    )
                    .await;
            } else {
                let _ = interaction
                    .create_response(
                        ctx,
                        serenity::CreateInteractionResponse::UpdateMessage(
                            serenity::CreateInteractionResponseMessage::new()
                                .content(format!("Deletion of tag '{}' cancelled.", tag.name))
                                .embeds(vec![])
                                .components(vec![]),
                        ),
                    )
                    .await;
            }
        } else {
            let _ = reply
                .edit(
                    ctx,
                    poise::CreateReply::default()
                        .content(format!("Deletion of tag '{}' timed out.", tag.name))
                        .components(vec![]),
                )
                .await;
        }

        return Ok(());
    }

    if let Some(filename) = &tag.asset_filename {
        db::delete_tag_asset_if_unused(pool, filename, &tag.id, &get_tag_assets_dir()).await;
    }

    db::delete_tag(pool, &tag.id).await?;
    ctx.say(format!("Tag '{}' deleted.", tag.name)).await?;
    Ok(())
}

async fn handle_tag_info(ctx: Context<'_>, server_id: &str, tag_name: &str) -> Result<(), Error> {
    let pool = &ctx.data().db_pool;
    let Some(tag) = db::get_tag(pool, server_id, tag_name).await else {
        return Err(user_error(format!("Tag '{}' not found.", tag_name)));
    };

    let created_dt = chrono::DateTime::from_timestamp_millis(tag.created_at)
        .map(|dt| {
            dt.with_timezone(&Amsterdam)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_else(|| "Unknown".to_string());

    let last_used_dt = tag
        .last_used_at
        .and_then(chrono::DateTime::from_timestamp_millis)
        .map(|dt| {
            dt.with_timezone(&Amsterdam)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_else(|| "Never".to_string());

    let embed = CreateEmbed::new()
        .title(format!("Tag: {}", tag.name))
        .field(
            "Author",
            format!("<@{}> ({})", tag.author_id, tag.author_name),
            true,
        )
        .field("Uses", tag.uses.to_string(), true)
        .field(
            "Has Local Media",
            if tag.asset_filename.is_some() {
                "Yes"
            } else {
                "No"
            },
            true,
        )
        .field("Created", created_dt, true)
        .field("Last Used", last_used_dt, true)
        .field("Content Preview", truncate_str(&tag.content, 200), false)
        .color(0x00FF88)
        .footer(CreateEmbedFooter::new("Healthy Bot"));

    ctx.send(poise::CreateReply::default().embed(embed)).await?;
    Ok(())
}

async fn fetch_tag_list_page(
    pool: &sqlx::sqlite::SqlitePool,
    server_id: &str,
    author_id: &str,
    page: u32,
    page_size: u32,
) -> sqlx::Result<Vec<db::Tag>> {
    let offset = (page - 1) * page_size;
    db::list_tags_by_author(pool, server_id, author_id, offset, page_size).await
}

async fn handle_tag_list(
    ctx: Context<'_>,
    server_id: &str,
    target_user: Option<(String, String)>,
    mut page: u32,
) -> Result<(), Error> {
    let pool = &ctx.data().db_pool;
    let page_size: u32 = 15;
    page = if page == 0 { 1 } else { page };

    let (target_id, target_name) = match target_user {
        Some((uid, uname)) => (uid, uname),
        None => (ctx.author().id.to_string(), ctx.author().name.clone()),
    };

    let total = db::count_tags_by_author(pool, server_id, &target_id)
        .await
        .unwrap_or(0);

    if total == 0 {
        let msg = if target_id == ctx.author().id.to_string() {
            "You haven't created any tags on this server yet.".to_string()
        } else {
            format!("No tags found for {}.", target_name)
        };
        ctx.say(msg).await?;
        return Ok(());
    }

    let total_pages = ((total + page_size as i64 - 1) / page_size as i64).max(1) as u32;
    if page > total_pages {
        page = total_pages;
    }

    let server_id_owned = server_id.to_string();
    let target_id_owned = target_id.clone();
    let target_name_owned = target_name.clone();

    paginate_async(ctx, total_pages as usize, page as usize, false, move |p| {
        let pool_clone = ctx.data().db_pool.clone();
        let sid = server_id_owned.clone();
        let tid = target_id_owned.clone();
        let tname = target_name_owned.clone();
        async move {
            let tags = fetch_tag_list_page(&pool_clone, &sid, &tid, p as u32, page_size)
                .await
                .unwrap_or_default();

            let title = if total_pages > 1 {
                format!("Tags by {} (Page {}/{})", tname, p, total_pages)
            } else {
                format!("Tags by {}", tname)
            };

            let mut desc = format!("**Total Tags:** {}\n\n", total);
            let offset = (p as u32 - 1) * page_size;
            for (i, t) in tags.iter().enumerate() {
                desc.push_str(&format!(
                    "{}. **{}** — {} uses\n",
                    offset + i as u32 + 1,
                    t.name,
                    t.uses
                ));
            }

            CreateEmbed::new()
                .title(title)
                .description(desc)
                .color(0x00FF88)
                .footer(CreateEmbedFooter::new("Healthy Bot"))
        }
    })
    .await?;

    Ok(())
}

async fn handle_tag_list_prefix(
    ctx: Context<'_>,
    server_id: &str,
    args: &[String],
) -> Result<(), Error> {
    let mut page: u32 = 1;
    let mut target_user: Option<(String, String)> = None;

    for arg in args {
        if let Ok(p) = arg.parse::<u32>() {
            page = p;
        } else if target_user.is_none() {
            if let Ok(u) = resolve_user(ctx, arg).await {
                target_user = Some(u);
            }
        }
    }

    handle_tag_list(ctx, server_id, target_user, page).await
}

async fn handle_tag_search(
    ctx: Context<'_>,
    server_id: &str,
    query: &str,
    target_user: Option<(String, String)>,
) -> Result<(), Error> {
    let pool = &ctx.data().db_pool;
    let author_id = target_user.as_ref().map(|(id, _)| id.as_str());
    let page_size = 15;
    let results = db::search_tags(pool, server_id, query, author_id, 150).await?;

    if results.is_empty() {
        let msg = match &target_user {
            Some((_, uname)) => format!("No tags matching '{}' found by {}.", query, uname),
            None => format!("No tags found matching '{}'.", query),
        };
        ctx.say(msg).await?;
        return Ok(());
    }

    let total_results = results.len();
    let total_pages = total_results.div_ceil(page_size);

    paginate(ctx, total_pages, 1, false, |p| {
        let title = match &target_user {
            Some((_, uname)) if total_pages > 1 => {
                format!(
                    "Tag Search: '{}' (by {}) (Page {}/{})",
                    query, uname, p, total_pages
                )
            }
            Some((_, uname)) => format!("Tag Search: '{}' (by {})", query, uname),
            None if total_pages > 1 => {
                format!("Tag Search: '{}' (Page {}/{})", query, p, total_pages)
            }
            None => format!("Tag Search: '{}'", query),
        };

        let start_idx = (p - 1) * page_size;
        let end_idx = std::cmp::min(start_idx + page_size, results.len());

        let mut desc = format!(
            "Found {} matching tag{}:\n\n",
            results.len(),
            if results.len() == 1 { "" } else { "s" }
        );
        for (i, t) in results[start_idx..end_idx].iter().enumerate() {
            desc.push_str(&format!(
                "{}. **{}** — {} uses (by {})\n",
                start_idx + i + 1,
                t.name,
                t.uses,
                t.author_name
            ));
        }

        CreateEmbed::new()
            .title(title)
            .description(desc)
            .color(0x00FF88)
            .footer(CreateEmbedFooter::new("Healthy Bot"))
    })
    .await?;

    Ok(())
}

async fn handle_tag_search_prefix(
    ctx: Context<'_>,
    server_id: &str,
    args: &[String],
) -> Result<(), Error> {
    if args.is_empty() {
        return Err(user_error("Usage: `!t search <query> [-user <user>]`"));
    }

    let mut query_parts = Vec::new();
    let mut target_user: Option<(String, String)> = None;
    let mut i = 0;

    while i < args.len() {
        if args[i] == "-user" && i + 1 < args.len() {
            if let Ok(u) = resolve_user(ctx, &args[i + 1]).await {
                target_user = Some(u);
            }
            i += 2;
        } else {
            query_parts.push(args[i].clone());
            i += 1;
        }
    }

    let query = query_parts.join(" ");
    if query.is_empty() {
        return Err(user_error("Search query cannot be empty."));
    }

    handle_tag_search(ctx, server_id, &query, target_user).await
}

async fn handle_tag_export(
    ctx: Context<'_>,
    server_id: &str,
    target_user: Option<(String, String)>,
) -> Result<(), Error> {
    if !is_server_admin(ctx).await {
        return Err(user_error("Only server administrators can export tags."));
    }

    let pool = &ctx.data().db_pool;
    let author_id = target_user.as_ref().map(|(id, _)| id.as_str());
    let tags = db::export_all_tags(pool, server_id, author_id).await?;

    if tags.is_empty() {
        ctx.say("No tags found to export.").await?;
        return Ok(());
    }

    let json_bytes = serde_json::to_vec_pretty(&tags)?;
    let filename = match &target_user {
        Some((uid, _)) => format!("tags-export-{}-{}.json", server_id, uid),
        None => format!("tags-export-{}.json", server_id),
    };

    let attachment = serenity::all::CreateAttachment::bytes(json_bytes, filename);
    let reply = poise::CreateReply::default()
        .content(format!("Exported {} tags.", tags.len()))
        .attachment(attachment);

    ctx.send(reply).await?;
    Ok(())
}

async fn handle_tag_export_prefix(
    ctx: Context<'_>,
    server_id: &str,
    args: &[String],
) -> Result<(), Error> {
    if !is_server_admin(ctx).await {
        return Err(user_error("Only server administrators can export tags."));
    }

    let mut target_user: Option<(String, String)> = None;
    let mut i = 0;
    while i < args.len() {
        if args[i] == "-user" && i + 1 < args.len() {
            if let Ok(u) = resolve_user(ctx, &args[i + 1]).await {
                target_user = Some(u);
            }
            i += 2;
        } else if target_user.is_none() {
            if let Ok(u) = resolve_user(ctx, &args[i]).await {
                target_user = Some(u);
            }
            i += 1;
        } else {
            i += 1;
        }
    }

    handle_tag_export(ctx, server_id, target_user).await
}

async fn handle_tag_remove_all(
    ctx: Context<'_>,
    server_id: &str,
    target_user: Option<(String, String)>,
) -> Result<(), Error> {
    if !is_server_admin(ctx).await {
        return Err(user_error(
            "Only server administrators can remove all tags.",
        ));
    }

    let pool = &ctx.data().db_pool;
    let author_id = target_user.as_ref().map(|(id, _)| id.as_str());
    let (deleted_count, asset_files) = db::delete_all_tags(pool, server_id, author_id).await?;

    for f in asset_files.into_iter().flatten() {
        db::delete_tag_asset_if_unused(pool, &f, "", &get_tag_assets_dir()).await;
    }

    if let Some((_, name)) = target_user {
        ctx.say(format!(
            "Deleted {} tags authored by {}.",
            deleted_count, name
        ))
        .await?;
    } else {
        ctx.say(format!(
            "Deleted all {} tags on this server.",
            deleted_count
        ))
        .await?;
    }
    Ok(())
}

async fn handle_tag_remove_all_prefix(
    ctx: Context<'_>,
    server_id: &str,
    args: &[String],
) -> Result<(), Error> {
    if !is_server_admin(ctx).await {
        return Err(user_error(
            "Only server administrators can remove all tags.",
        ));
    }

    let mut target_user: Option<(String, String)> = None;
    let mut i = 0;
    while i < args.len() {
        if args[i] == "-user" && i + 1 < args.len() {
            if let Ok(u) = resolve_user(ctx, &args[i + 1]).await {
                target_user = Some(u);
            }
            i += 2;
        } else if target_user.is_none() {
            if let Ok(u) = resolve_user(ctx, &args[i]).await {
                target_user = Some(u);
            }
            i += 1;
        } else {
            i += 1;
        }
    }

    handle_tag_remove_all(ctx, server_id, target_user).await
}

async fn handle_tag_cleanup(ctx: Context<'_>) -> Result<(), Error> {
    if !is_server_admin(ctx).await {
        return Err(user_error(
            "Only server administrators can clean up tag assets.",
        ));
    }

    let pool = &ctx.data().db_pool;
    let active_assets = db::get_all_asset_filenames(pool).await?;

    let assets_dir = get_tag_assets_dir();
    if !assets_dir.exists() {
        ctx.say("Asset directory does not exist. No files to clean up.")
            .await?;
        return Ok(());
    }

    let mut dir_entries = tokio::fs::read_dir(&assets_dir).await?;
    let mut removed_count = 0;
    let mut reclaimed_bytes: u64 = 0;

    while let Ok(Some(entry)) = dir_entries.next_entry().await {
        let file_name = entry.file_name().to_string_lossy().to_string();
        if !active_assets.contains(&file_name) {
            if let Ok(metadata) = entry.metadata().await {
                reclaimed_bytes += metadata.len();
            }
            if tokio::fs::remove_file(entry.path()).await.is_ok() {
                removed_count += 1;
            }
        }
    }

    let reclaimed_mb = reclaimed_bytes as f64 / 1_048_576.0;
    ctx.say(format!(
        "Asset cleanup complete. Removed {} orphaned file(s), reclaiming {:.2} MB.",
        removed_count, reclaimed_mb
    ))
    .await?;
    Ok(())
}

async fn handle_tag_seed(ctx: Context<'_>) -> Result<(), Error> {
    if !is_server_admin(ctx).await {
        return Err(user_error("Only server administrators can run seed."));
    }
    let pool = &ctx.data().db_pool;
    match db::seed_tags_from_file_if_present(pool).await {
        Ok(Some(path)) => {
            ctx.say(format!(
                "Successfully seeded tags from `{}` and removed the seed file.",
                path.display()
            ))
            .await?;
            Ok(())
        }
        Ok(None) => {
            ctx.say("No seed file found. Place `tags_seed.sql` in `/healthybot/data/tags_seed.sql` or `data/tags_seed.sql`.").await?;
            Ok(())
        }
        Err(e) => Err(user_error(format!("Failed to seed tags: {e}"))),
    }
}

/// Tag command: invoke, create, delete, or inspect tags
#[poise::command(
    prefix_command,
    track_edits,
    track_deletion,
    rename = "t",
    aliases("tag")
)]
pub async fn t_cmd(ctx: Context<'_>, #[rest] input: Option<String>) -> Result<(), Error> {
    let pool = &ctx.data().db_pool;
    let server_id = guild_id_or_default(ctx);

    let input_str = input.unwrap_or_default();
    let trimmed = input_str.trim();
    if trimmed.is_empty() {
        ctx.say("Usage: `!t <tagname> [args]` | `!t random` | `!t add <name> <content>` | `!t edit <name> <content>` | `!t rename <old> <new>` | `!t alias <tag> <alias>` | `!t delete <name>` | `!t info <name>` (or `!t raw` / `!t owner <name>`) | `!t list [user] [page]` | `!t search <query>` | `!t export` | `!t remove all` | `!t cleanup` | `!t seed`").await?;
        return Ok(());
    }

    let mut parts = trimmed.split_whitespace();
    let first = parts.next().unwrap_or("");
    let rest_vec: Vec<String> = parts.map(|s| s.to_string()).collect();

    match first.to_lowercase().as_str() {
        "random" | "rand" => {
            if let Some(tag) = db::get_random_tag(pool, &server_id).await {
                send_tag(ctx, &tag, &rest_vec, true).await
            } else {
                Err(user_error("No tags found on this server."))
            }
        }
        "add" | "create" => {
            let Some(tag_name) = rest_vec.first() else {
                return Err(user_error(
                    "Usage: `!t add <name> [content]` (or attach a file)",
                ));
            };
            let raw_content = if rest_vec.len() > 1 {
                rest_vec[1..].join(" ")
            } else {
                String::new()
            };
            let attachment = match ctx {
                poise::Context::Prefix(p_ctx) => p_ctx.msg.attachments.first(),
                poise::Context::Application(_) => None,
            };
            handle_tag_create_core(ctx, &server_id, tag_name, &raw_content, attachment).await
        }
        "edit" => {
            let Some(tag_name) = rest_vec.first() else {
                return Err(user_error(
                    "Usage: `!t edit <name> [new_content]` (or attach a file)",
                ));
            };
            let raw_content = if rest_vec.len() > 1 {
                Some(rest_vec[1..].join(" "))
            } else {
                None
            };
            let attachment = match ctx {
                poise::Context::Prefix(p_ctx) => p_ctx.msg.attachments.first(),
                poise::Context::Application(_) => None,
            };
            handle_tag_edit_core(
                ctx,
                &server_id,
                tag_name,
                raw_content.as_deref(),
                attachment,
            )
            .await
        }
        "rename" => {
            if rest_vec.len() < 2 {
                return Err(user_error("Usage: `!t rename <old_name> <new_name>`"));
            }
            handle_tag_rename_core(ctx, &server_id, &rest_vec[0], &rest_vec[1]).await
        }
        "alias" => {
            if rest_vec.len() < 2 {
                return Err(user_error("Usage: `!t alias <existing_name> <alias_name>`"));
            }
            handle_tag_alias_core(ctx, &server_id, &rest_vec[0], &rest_vec[1]).await
        }
        "cleanup" => handle_tag_cleanup(ctx).await,
        "seed" => handle_tag_seed(ctx).await,
        "delete" | "del" | "remove" | "rm" => {
            if rest_vec.first().map(|s| s.as_str()) == Some("all") {
                handle_tag_remove_all_prefix(ctx, &server_id, &rest_vec[1..]).await
            } else {
                let clean_args: Vec<String> = rest_vec
                    .into_iter()
                    .filter(|s| s != "tag" && s != "-force")
                    .collect();
                let Some(tag_name) = clean_args.first() else {
                    return Err(user_error("Please specify a tag name to delete."));
                };
                handle_tag_delete(ctx, &server_id, tag_name).await
            }
        }
        "info" | "raw" | "owner" => {
            let Some(tag_name) = rest_vec.first() else {
                return Err(user_error("Please specify a tag name."));
            };
            handle_tag_info(ctx, &server_id, tag_name).await
        }
        "list" => handle_tag_list_prefix(ctx, &server_id, &rest_vec).await,
        "search" => handle_tag_search_prefix(ctx, &server_id, &rest_vec).await,
        "export" => handle_tag_export_prefix(ctx, &server_id, &rest_vec).await,
        _ => {
            if let Some(tag) = db::get_tag(pool, &server_id, first).await {
                send_tag(ctx, &tag, &rest_vec, false).await
            } else if let Some(tag) = db::get_tag(pool, &server_id, trimmed).await {
                send_tag(ctx, &tag, &[], false).await
            } else {
                Err(user_error(format!(
                    "Tag '{}' not found.",
                    truncate_str(first, 50)
                )))
            }
        }
    }
}

/// Tag management commands
#[poise::command(
    slash_command,
    rename = "tag",
    subcommands(
        "tag_get_slash",
        "tag_random_slash",
        "tag_create_slash",
        "tag_edit_slash",
        "tag_rename_slash",
        "tag_alias_slash",
        "tag_delete_slash",
        "tag_info_slash",
        "tag_raw_slash",
        "tag_owner_slash",
        "tag_list_slash",
        "tag_search_slash",
        "tag_export_slash",
        "tag_remove_all_slash",
        "tag_cleanup_slash",
        "tag_seed_slash"
    )
)]
pub async fn tag_slash(_: Context<'_>) -> Result<(), Error> {
    Ok(())
}

/// Retrieve and send a tag
#[poise::command(slash_command, rename = "get")]
pub async fn tag_get_slash(
    ctx: Context<'_>,
    #[autocomplete = "autocomplete_tag_name"]
    #[description = "Tag name"]
    name: String,
    #[description = "Arguments to pass to the tag"] args: Option<String>,
) -> Result<(), Error> {
    let pool = &ctx.data().db_pool;
    let server_id = guild_id_or_default(ctx);

    let Some(tag) = db::get_tag(pool, &server_id, &name).await else {
        return Err(user_error(format!(
            "Tag '{}' not found.",
            truncate_str(&name, 50)
        )));
    };

    let args_vec: Vec<String> = args
        .map(|s| s.split_whitespace().map(|w| w.to_string()).collect())
        .unwrap_or_default();

    send_tag(ctx, &tag, &args_vec, false).await
}

/// Retrieve and send a random tag
#[poise::command(slash_command, rename = "random")]
pub async fn tag_random_slash(
    ctx: Context<'_>,
    #[description = "Arguments to pass to the tag"] args: Option<String>,
) -> Result<(), Error> {
    let pool = &ctx.data().db_pool;
    let server_id = guild_id_or_default(ctx);

    if let Some(tag) = db::get_random_tag(pool, &server_id).await {
        let args_vec: Vec<String> = args
            .map(|s| s.split_whitespace().map(|w| w.to_string()).collect())
            .unwrap_or_default();
        send_tag(ctx, &tag, &args_vec, true).await
    } else {
        Err(user_error("No tags found on this server."))
    }
}

/// Create a new tag
#[poise::command(slash_command, rename = "create")]
pub async fn tag_create_slash(
    ctx: Context<'_>,
    #[description = "Tag name"] name: String,
    #[description = "Tag text content"] content: Option<String>,
    #[description = "Optional file attachment"] attachment: Option<serenity::all::Attachment>,
) -> Result<(), Error> {
    let server_id = guild_id_or_default(ctx);
    let raw_content = content.unwrap_or_default();
    handle_tag_create_core(ctx, &server_id, &name, &raw_content, attachment.as_ref()).await
}

/// Edit an existing tag
#[poise::command(slash_command, rename = "edit")]
pub async fn tag_edit_slash(
    ctx: Context<'_>,
    #[autocomplete = "autocomplete_tag_name"]
    #[description = "Tag name to edit"]
    name: String,
    #[description = "New text content"] content: Option<String>,
    #[description = "New file attachment"] attachment: Option<serenity::all::Attachment>,
) -> Result<(), Error> {
    let server_id = guild_id_or_default(ctx);
    handle_tag_edit_core(
        ctx,
        &server_id,
        &name,
        content.as_deref(),
        attachment.as_ref(),
    )
    .await
}

/// Rename an existing tag
#[poise::command(slash_command, rename = "rename")]
pub async fn tag_rename_slash(
    ctx: Context<'_>,
    #[autocomplete = "autocomplete_tag_name"]
    #[description = "Existing tag name"]
    old_name: String,
    #[description = "New tag name"] new_name: String,
) -> Result<(), Error> {
    let server_id = guild_id_or_default(ctx);
    handle_tag_rename_core(ctx, &server_id, &old_name, &new_name).await
}

/// Create an alias pointing to an existing tag
#[poise::command(slash_command, rename = "alias")]
pub async fn tag_alias_slash(
    ctx: Context<'_>,
    #[autocomplete = "autocomplete_tag_name"]
    #[description = "Existing tag name"]
    target: String,
    #[description = "New alias name"] alias_name: String,
) -> Result<(), Error> {
    let server_id = guild_id_or_default(ctx);
    handle_tag_alias_core(ctx, &server_id, &target, &alias_name).await
}

/// Delete an existing tag
#[poise::command(slash_command, rename = "delete")]
pub async fn tag_delete_slash(
    ctx: Context<'_>,
    #[autocomplete = "autocomplete_tag_name"]
    #[description = "Tag name to delete"]
    name: String,
) -> Result<(), Error> {
    let server_id = guild_id_or_default(ctx);
    handle_tag_delete(ctx, &server_id, &name).await
}

/// Show statistics and info for a tag
#[poise::command(slash_command, rename = "info")]
pub async fn tag_info_slash(
    ctx: Context<'_>,
    #[autocomplete = "autocomplete_tag_name"]
    #[description = "Tag name to inspect"]
    name: String,
) -> Result<(), Error> {
    let server_id = guild_id_or_default(ctx);
    handle_tag_info(ctx, &server_id, &name).await
}

/// View a tag's raw content and metadata
#[poise::command(slash_command, rename = "raw")]
pub async fn tag_raw_slash(
    ctx: Context<'_>,
    #[autocomplete = "autocomplete_tag_name"]
    #[description = "Tag name to inspect"]
    name: String,
) -> Result<(), Error> {
    let server_id = guild_id_or_default(ctx);
    handle_tag_info(ctx, &server_id, &name).await
}

/// View a tag's owner and metadata
#[poise::command(slash_command, rename = "owner")]
pub async fn tag_owner_slash(
    ctx: Context<'_>,
    #[autocomplete = "autocomplete_tag_name"]
    #[description = "Tag name to inspect"]
    name: String,
) -> Result<(), Error> {
    let server_id = guild_id_or_default(ctx);
    handle_tag_info(ctx, &server_id, &name).await
}

/// List tags ordered by popularity
#[poise::command(slash_command, rename = "list")]
pub async fn tag_list_slash(
    ctx: Context<'_>,
    #[description = "Filter by author"] user: Option<serenity::User>,
    #[description = "Page number (defaults to 1)"] page: Option<u32>,
) -> Result<(), Error> {
    let server_id = guild_id_or_default(ctx);
    let target_user = user.map(|u| (u.id.to_string(), u.name));
    handle_tag_list(ctx, &server_id, target_user, page.unwrap_or(1)).await
}

/// Search for tags by name
#[poise::command(slash_command, rename = "search")]
pub async fn tag_search_slash(
    ctx: Context<'_>,
    #[description = "Search term"] query: String,
    #[description = "Filter by author"] user: Option<serenity::User>,
) -> Result<(), Error> {
    let server_id = guild_id_or_default(ctx);
    let target_user = user.map(|u| (u.id.to_string(), u.name));
    handle_tag_search(ctx, &server_id, &query, target_user).await
}

/// Export all tags to a JSON file (Admin only)
#[poise::command(
    slash_command,
    rename = "export",
    default_member_permissions = "ADMINISTRATOR"
)]
pub async fn tag_export_slash(
    ctx: Context<'_>,
    #[description = "Filter by author"] user: Option<serenity::User>,
) -> Result<(), Error> {
    let server_id = guild_id_or_default(ctx);
    let target_user = user.map(|u| (u.id.to_string(), u.name));
    handle_tag_export(ctx, &server_id, target_user).await
}

/// Delete all tags on the server, optionally filtered by user (Admin only)
#[poise::command(
    slash_command,
    rename = "remove_all",
    default_member_permissions = "ADMINISTRATOR"
)]
pub async fn tag_remove_all_slash(
    ctx: Context<'_>,
    #[description = "Filter by author"] user: Option<serenity::User>,
    #[description = "Confirm deletion (must be True)"] confirm: bool,
) -> Result<(), Error> {
    if !confirm {
        return Err(user_error("Please set confirm to True to delete all tags."));
    }
    let server_id = guild_id_or_default(ctx);
    let target_user = user.map(|u| (u.id.to_string(), u.name));
    handle_tag_remove_all(ctx, &server_id, target_user).await
}

/// Clean up unreferenced media files from disk (Admin only)
#[poise::command(
    slash_command,
    rename = "cleanup",
    default_member_permissions = "ADMINISTRATOR"
)]
pub async fn tag_cleanup_slash(ctx: Context<'_>) -> Result<(), Error> {
    handle_tag_cleanup(ctx).await
}

/// Seed tags from SQL file on the bot host/volume (Admin only)
#[poise::command(
    slash_command,
    rename = "seed",
    default_member_permissions = "ADMINISTRATOR"
)]
pub async fn tag_seed_slash(ctx: Context<'_>) -> Result<(), Error> {
    handle_tag_seed(ctx).await
}
