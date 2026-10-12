use super::{is_server_admin, Context, Error, HOF_ROLE_ID};
use crate::pagination::paginate;
use crate::{create_embed, db, truncate_str, user_error};
use chrono::{LocalResult, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Europe::Amsterdam;
use poise::serenity_prelude as serenity;
use serenity::builder::CreateEmbedFooter;

#[derive(Debug, PartialEq)]
pub struct ParsedReminder {
    pub message: String,
    pub datetime_utc: chrono::DateTime<Utc>,
}

pub fn parse_reminder_input(input: &str) -> Vec<ParsedReminder> {
    input
        .split([';', '\n'])
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .filter_map(|msg| {
            let actions: Vec<&str> = msg.split_whitespace().collect();
            if actions.len() < 3 {
                return None;
            }

            let datetime_str = format!("{} {}", actions[0], actions[1]);
            if let Ok(datetime) = NaiveDateTime::parse_from_str(&datetime_str, "%d-%m-%Y %H:%M") {
                let datetime_tz = match Amsterdam.from_local_datetime(&datetime) {
                    LocalResult::Single(t) => Some(t),
                    LocalResult::Ambiguous(t1, _) => Some(t1),
                    LocalResult::None => {
                        // Spring forward gap: add 1 hour and try again
                        Amsterdam
                            .from_local_datetime(&(datetime + chrono::Duration::hours(1)))
                            .earliest()
                    }
                };

                if let Some(dt_tz) = datetime_tz {
                    return Some(ParsedReminder {
                        message: actions[2..].join(" "),
                        datetime_utc: dt_tz.with_timezone(&Utc),
                    });
                }
            }
            None
        })
        .collect()
}

/// Manage reminders
#[poise::command(
    slash_command,
    prefix_command,
    rename = "remind",
    aliases("reminder", "reminders")
)]
pub async fn remind_cmd(
    ctx: Context<'_>,
    #[rest]
    #[description = "Reminder message or action"]
    input: Option<String>,
) -> Result<(), Error> {
    let pool = &ctx.data().db_pool;
    let user_id = ctx.author().id.to_string();
    db::create_user_if_not_exists(pool, &user_id).await?;
    let user = db::get_user(pool, &user_id).await.unwrap();

    let input_val = input.unwrap_or_default();
    let parts: Vec<&str> = input_val.split_whitespace().collect();
    let action = parts.first().copied().unwrap_or("");

    if action == "remove" {
        let ids_to_remove = &parts[1..];
        if ids_to_remove.is_empty() {
            return Err(user_error("No reminder IDs provided to remove."));
        }

        let mut removed_ids = Vec::new();
        let mut failed_ids = Vec::new();
        let mut unauthorized_ids = Vec::new();

        for id_str in ids_to_remove {
            let id: i64 = match id_str.parse() {
                Ok(n) => n,
                Err(_) => {
                    failed_ids.push(id_str.to_string());
                    continue;
                }
            };

            let reminder_res: Result<db::Reminder, _> =
                sqlx::query_as::<_, db::Reminder>("SELECT * FROM reminder WHERE id = ?")
                    .bind(id)
                    .fetch_one(pool)
                    .await;

            match reminder_res {
                Ok(reminder) => {
                    if reminder.owner_discord_id != user_id {
                        unauthorized_ids.push(id.to_string());
                        continue;
                    }

                    if sqlx::query("DELETE FROM reminder WHERE id = ?")
                        .bind(id)
                        .execute(pool)
                        .await
                        .is_ok()
                    {
                        removed_ids.push(id.to_string());
                    } else {
                        failed_ids.push(id.to_string());
                    }
                }
                Err(_) => {
                    failed_ids.push(id.to_string());
                }
            }
        }

        let mut response = String::new();
        if !removed_ids.is_empty() {
            response.push_str(&format!(
                "Successfully removed {} reminder(s): #{}.\n",
                removed_ids.len(),
                removed_ids.join(", #")
            ));
        }
        if !unauthorized_ids.is_empty() {
            response.push_str(&format!(
                "You do not own these reminder(s): #{}.\n",
                unauthorized_ids.join(", #")
            ));
        }
        if !failed_ids.is_empty() {
            response.push_str(&format!(
                "Could not find or parse these ID(s): {}.",
                failed_ids.join(", ")
            ));
        }

        ctx.say(response.trim()).await?;
        return Ok(());
    }

    // Check for listing actions
    let mut page: usize = 1;
    let mut filter = None;

    for part in &parts {
        if ["today", "tomorrow", "week", "month"].contains(part) {
            filter = Some(*part);
        } else if let Ok(p) = part.parse::<usize>() {
            if p > 0 {
                page = p;
            }
        }
    }

    let invoked_name = ctx.invoked_command_name();
    let is_listing = invoked_name == "reminders"
        || invoked_name == "reminder"
        || input_val.is_empty()
        || filter.is_some()
        || (parts.len() <= 2
            && parts
                .first()
                .map(|p| p.parse::<usize>().is_ok())
                .unwrap_or(false));

    if is_listing {
        let mut reminders: Vec<db::Reminder> = if invoked_name == "reminders" {
            sqlx::query_as::<_, db::Reminder>("SELECT * FROM reminder")
                .fetch_all(pool)
                .await?
        } else {
            sqlx::query_as::<_, db::Reminder>("SELECT * FROM reminder WHERE owner_discord_id = ?")
                .bind(&user_id)
                .fetch_all(pool)
                .await?
        };

        let now_ams = Utc::now().with_timezone(&Amsterdam);
        if let Some(f) = filter {
            reminders.retain(|r| {
                let r_date_ams = r.date_utc().with_timezone(&Amsterdam).date_naive();
                match f {
                    "today" => r_date_ams == now_ams.date_naive(),
                    "tomorrow" => r_date_ams == now_ams.date_naive() + chrono::Duration::days(1),
                    "week" => {
                        r_date_ams >= now_ams.date_naive()
                            && r_date_ams <= now_ams.date_naive() + chrono::Duration::days(7)
                    }
                    "month" => {
                        r_date_ams >= now_ams.date_naive()
                            && r_date_ams <= now_ams.date_naive() + chrono::Duration::days(30)
                    }
                    _ => true,
                }
            });
        }

        if reminders.is_empty() {
            let mut embed = create_embed("Reminders").footer(CreateEmbedFooter::new(format!(
                "Reminder example: !remind {} f1 time turds",
                now_ams.format("%d-%m-%Y %H:%M")
            )));
            embed = embed.description("No reminders found.");
            ctx.send(poise::CreateReply::default().embed(embed)).await?;
            return Ok(());
        }

        reminders.sort_by_key(|r| r.date);

        // Paginate: 10 reminders per page
        let page_size = 10;
        let total_pages = reminders.len().div_ceil(page_size);

        if page > total_pages {
            let mut embed = create_embed("Reminders").footer(CreateEmbedFooter::new(format!(
                "Reminder example: !remind {} f1 time turds",
                now_ams.format("%d-%m-%Y %H:%M")
            )));
            embed = embed.description(format!(
                "Page {} does not exist. Total pages: {}.",
                page, total_pages
            ));
            ctx.send(poise::CreateReply::default().embed(embed)).await?;
            return Ok(());
        }

        paginate(ctx, total_pages, page, false, |p| {
            let start_idx = (p - 1) * page_size;
            let end_idx = std::cmp::min(start_idx + page_size, reminders.len());

            let title = if total_pages > 1 {
                format!("Reminders (Page {} of {})", p, total_pages)
            } else {
                "Reminders".to_string()
            };

            let mut embed = create_embed(&title).footer(CreateEmbedFooter::new(format!(
                "Reminder example: !remind {} f1 time turds",
                now_ams.format("%d-%m-%Y %H:%M")
            )));

            for r in &reminders[start_idx..end_idx] {
                embed = embed.field(
                    truncate_str(&format!("#{}. {}", r.id, r.message.trim()), 256),
                    format!("<t:{}>", r.date_utc().timestamp()),
                    false,
                );
            }

            embed
        })
        .await?;

        return Ok(());
    }

    // Add reminder logic
    let is_authorized = {
        let has_hof = match ctx.author_member().await {
            Some(member) => member.roles.iter().any(|r| r.get() == HOF_ROLE_ID),
            None => false,
        };
        is_server_admin(ctx).await || user.role == "ADMIN" || user.authorized || has_hof
    };

    if is_authorized {
        let reminders = parse_reminder_input(&input_val);
        let mut created = 0;

        for r in reminders {
            sqlx::query("INSERT INTO reminder (message, date, owner_discord_id) VALUES (?, ?, ?)")
                .bind(r.message)
                .bind(r.datetime_utc.timestamp_millis())
                .bind(&user_id)
                .execute(pool)
                .await?;
            created += 1;
        }

        if created == 0 {
            return Err(user_error("Failed to parse reminder. Make sure the formatting is correct (eg. 30-09-2022 19:24 message)."));
        }

        ctx.say(format!("Created {} reminders.", created)).await?;
        return Ok(());
    }

    Err(user_error("You are not authorized to use this command."))
}
