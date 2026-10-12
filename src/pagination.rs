use crate::{Data, Error};
use poise::futures_util::StreamExt;
use poise::serenity_prelude as serenity;
use std::future::Future;

type Context<'a> = poise::Context<'a, Data, Error>;

/// Generate standard prev/next/cancel button row for interactive pagination.
pub fn pagination_buttons(
    page: usize,
    total_pages: usize,
) -> Vec<serenity::builder::CreateActionRow> {
    if total_pages <= 1 {
        return vec![];
    }
    vec![serenity::builder::CreateActionRow::Buttons(vec![
        serenity::builder::CreateButton::new("prev_page")
            .label("◀")
            .style(serenity::all::ButtonStyle::Secondary)
            .disabled(page <= 1),
        serenity::builder::CreateButton::new("next_page")
            .label("▶")
            .style(serenity::all::ButtonStyle::Secondary)
            .disabled(page >= total_pages),
        serenity::builder::CreateButton::new("cancel")
            .label("❌")
            .style(serenity::all::ButtonStyle::Danger),
    ])]
}

/// Run an interactive pagination session with buttons for a multi-page embed (synchronous renderer).
pub async fn paginate<F>(
    ctx: Context<'_>,
    total_pages: usize,
    page: usize,
    ephemeral: bool,
    render_page: F,
) -> Result<(), Error>
where
    F: Fn(usize) -> serenity::builder::CreateEmbed,
{
    paginate_async(ctx, total_pages, page, ephemeral, move |p| {
        let embed = render_page(p);
        async move { embed }
    })
    .await
}

/// Run an interactive pagination session with buttons for a multi-page embed (asynchronous renderer).
pub async fn paginate_async<F, Fut>(
    ctx: Context<'_>,
    total_pages: usize,
    mut page: usize,
    ephemeral: bool,
    render_page: F,
) -> Result<(), Error>
where
    F: Fn(usize) -> Fut,
    Fut: Future<Output = serenity::builder::CreateEmbed>,
{
    page = page.clamp(1, total_pages.max(1));
    let embed = render_page(page).await;
    let components = pagination_buttons(page, total_pages);

    let reply = ctx
        .send(
            poise::CreateReply::default()
                .embed(embed)
                .components(components)
                .ephemeral(ephemeral),
        )
        .await?;

    if total_pages > 1 {
        let mut collector = reply
            .message()
            .await?
            .await_component_interactions(ctx)
            .author_id(ctx.author().id)
            .timeout(std::time::Duration::from_secs(60))
            .stream();

        let mut cancelled = false;
        while let Some(interaction) = collector.next().await {
            if interaction.data.custom_id == "cancel" {
                cancelled = true;
                let current_embed = render_page(page).await;
                let _ = interaction
                    .create_response(
                        ctx,
                        serenity::builder::CreateInteractionResponse::UpdateMessage(
                            serenity::builder::CreateInteractionResponseMessage::new()
                                .embed(current_embed)
                                .components(vec![]),
                        ),
                    )
                    .await;
                break;
            }

            match interaction.data.custom_id.as_str() {
                "prev_page" => page = page.saturating_sub(1).max(1),
                "next_page" => page = std::cmp::min(page + 1, total_pages),
                _ => continue,
            }

            let new_embed = render_page(page).await;
            let new_components = pagination_buttons(page, total_pages);
            let _ = interaction
                .create_response(
                    ctx,
                    serenity::builder::CreateInteractionResponse::UpdateMessage(
                        serenity::builder::CreateInteractionResponseMessage::new()
                            .embed(new_embed)
                            .components(new_components),
                    ),
                )
                .await;
        }

        if !cancelled {
            let final_embed = render_page(page).await;
            let _ = reply
                .edit(
                    ctx,
                    poise::CreateReply::default()
                        .embed(final_embed)
                        .components(vec![]),
                )
                .await;
        }
    }

    Ok(())
}
