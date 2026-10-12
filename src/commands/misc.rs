use super::{Context, Error};
use crate::openai::{ChatMessage, ChatMessageRequestContent};
use crate::{recent, split_message, user_error};
use poise::serenity_prelude as serenity;

/// Generate a markov chain message for a user
#[poise::command(slash_command, prefix_command)]
pub async fn markov(ctx: Context<'_>, user: Option<serenity::User>) -> Result<(), Error> {
    // Check cooldown from cache
    let cooldown_secs = ctx
        .data()
        .get_setting_u64("markov_cooldown_seconds", 0)
        .await;

    let mut last_inv = ctx.data().last_markov.lock().await;
    if last_inv.elapsed().as_secs() < cooldown_secs {
        return Ok(());
    }
    *last_inv = std::time::Instant::now();

    let target_id = user
        .map(|u| u.id.to_string())
        .unwrap_or_else(|| ctx.author().id.to_string());
    if let Some(generated) = ctx.data().markov_repo.generate(&target_id).await {
        log::info!("Markov generated for {}: {}", target_id, generated);
        ctx.say(generated).await?;
    } else {
        log::warn!("Markov generation failed or no data for {}", target_id);
        ctx.say("Crack cocaine").await?;
    }
    Ok(())
}

/// Summarize the recent conversation in the current channel
#[poise::command(slash_command, prefix_command)]
pub async fn tldr(
    ctx: Context<'_>,
    #[description = "How many recent messages to summarize (default 50, max 100)"] count: Option<
        i64,
    >,
) -> Result<(), Error> {
    // Cooldown check, mirroring the markov / AI throttling pattern.
    let cooldown_secs = ctx.data().get_setting_u64("tldr_cooldown_seconds", 0).await;
    {
        let mut last_inv = ctx.data().last_tldr.lock().await;
        if last_inv.elapsed().as_secs() < cooldown_secs {
            return Ok(());
        }
        *last_inv = std::time::Instant::now();
    }

    // 100 is Discord's single-fetch cap, so we never paginate.
    let limit = count.unwrap_or(50).clamp(1, 100) as u8;

    let messages = ctx
        .channel_id()
        .messages(
            ctx.http(),
            serenity::builder::GetMessages::new().limit(limit),
        )
        .await?;

    let entries: Vec<recent::RecentMessage> = messages
        .iter()
        .map(|m| recent::RecentMessage {
            author_name: m.author.name.clone(),
            // content_safe renders mentions as names instead of raw <@id> tokens.
            content: m.content_safe(ctx.cache()),
            is_bot: m.author.bot,
        })
        .collect();

    let transcript = recent::build_transcript(&entries)
        .ok_or_else(|| user_error("There's nothing recent to summarize here."))?;

    let chat_model = ctx
        .data()
        .get_setting_or("ai_chat_model", "gpt-4o-mini")
        .await;

    let request = vec![
        ChatMessage {
            role: "system".to_string(),
            name: None,
            content: ChatMessageRequestContent::Text(
                "You are a summarizer for a Discord channel. You are given a chronological \
                 transcript of messages formatted as `Name: message`. Produce a concise \
                 summary of the discussion as a few short bullet points, focusing on the key \
                 topics, decisions, and open questions. Do not invent information that is not \
                 present in the transcript."
                    .to_string(),
            ),
        },
        ChatMessage {
            role: "user".to_string(),
            name: None,
            content: ChatMessageRequestContent::Text(transcript),
        },
    ];

    let _typing = ctx.channel_id().start_typing(&ctx.serenity_context().http);

    let response = ctx
        .data()
        .openai_client
        .create_chat(&chat_model, request)
        .await?;

    let summary = response
        .choices
        .first()
        .map(|c| c.message.content_text().to_string())
        .unwrap_or_default();

    if summary.trim().is_empty() {
        return Err(user_error("The summarizer returned an empty response."));
    }

    // Discord caps a single message at 2000 chars; send the summary in pieces.
    for chunk in split_message(&summary, 2000) {
        ctx.say(chunk).await?;
    }

    Ok(())
}

/// Show help for all commands
#[poise::command(slash_command, prefix_command)]
pub async fn help(
    ctx: Context<'_>,
    #[description = "Specific command to show help for"]
    #[autocomplete = "poise::builtins::autocomplete_command"]
    command: Option<String>,
) -> Result<(), Error> {
    poise::builtins::help(
        ctx,
        command.as_deref(),
        poise::builtins::HelpConfiguration {
            extra_text_at_bottom: "Type !help <command> for more info on a specific command.",
            ..Default::default()
        },
    )
    .await?;
    Ok(())
}
