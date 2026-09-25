use anyhow::{Context, Result, bail};
use chrono::{NaiveDate, NaiveTime, Utc};
use schedule_bot::{AppState, format::DailyKind, handlers, store::Store};
use std::{env, time::Duration};
use teloxide::{
    Bot,
    dispatching::Dispatcher,
    payloads::SetMyCommandsSetters,
    prelude::Requester,
    types::{BotCommand, BotCommandScope},
};
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let bot_token = required_env("TELEGRAM_BOT_TOKEN")?;
    let database_url = required_env("DATABASE_URL")?;
    let bootstrap_token = env::var("ADMIN_BOOTSTRAP_TOKEN").ok();
    if let Some(token) = &bootstrap_token {
        if token.len() < 24
            || token.len() > 54
            || !token
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            bail!(
                "ADMIN_BOOTSTRAP_TOKEN должен быть строкой длиной 24–54 символа из букв, цифр, `_` или `-`"
            );
        }
    } else {
        warn!(
            "ADMIN_BOOTSTRAP_TOKEN не задан; первого администратора нужно создать вручную в базе"
        );
    }
    let timezone = env::var("BOT_TIMEZONE")
        .unwrap_or_else(|_| "Europe/Moscow".to_owned())
        .parse()
        .context("BOT_TIMEZONE должен быть IANA timezone, например Europe/Moscow")?;

    let store = connect_with_retry(&database_url).await?;
    let bot = Bot::new(bot_token);
    let me = bot
        .get_me()
        .await
        .context("не удалось проверить TELEGRAM_BOT_TOKEN")?;
    info!(bot = ?me.username(), "бот запущен");
    let bot_username = me.username().to_owned();
    bot.set_my_commands([
        BotCommand::new("setgroup", "привязать группу к чату"),
        BotCommand::new("group", "показать привязанную группу"),
        BotCommand::new("today", "расписание на сегодня"),
        BotCommand::new("week", "расписание на неделю"),
        BotCommand::new("day", "расписание на дату"),
        BotCommand::new("help", "команды расписания"),
    ])
    .scope(BotCommandScope::AllGroupChats)
    .await
    .context("не удалось зарегистрировать команды для групп")?;

    let state = AppState {
        store,
        bootstrap_token,
        timezone,
        bot_username,
        rate_limiter: Default::default(),
    };
    tokio::spawn(outbox_worker(bot.clone(), state.store.clone()));
    tokio::spawn(daily_schedule_worker(state.clone()));

    Dispatcher::builder(bot, handlers::schema())
        .dependencies(dptree::deps![state])
        .enable_ctrlc_handler()
        .build()
        .dispatch()
        .await;
    Ok(())
}

fn due_daily_delivery(now: chrono::DateTime<chrono_tz::Tz>) -> Option<(NaiveDate, DailyKind)> {
    let morning = NaiveTime::from_hms_opt(7, 0, 0).expect("valid time");
    let evening = NaiveTime::from_hms_opt(21, 0, 0).expect("valid time");
    if now.time() >= evening {
        Some((now.date_naive() + chrono::Days::new(1), DailyKind::Tomorrow))
    } else if now.time() >= morning {
        Some((now.date_naive(), DailyKind::Today))
    } else {
        None
    }
}

async fn daily_schedule_worker(state: AppState) {
    let mut tick = tokio::time::interval(Duration::from_secs(15));
    loop {
        tick.tick().await;
        let now = Utc::now().with_timezone(&state.timezone);
        let Some((delivery_date, kind)) = due_daily_delivery(now) else {
            continue;
        };
        match state
            .store
            .enqueue_daily_schedules(delivery_date, kind)
            .await
        {
            Ok(queued) if queued > 0 => {
                info!(%delivery_date, delivery = kind.as_str(), queued, "поставлены ежедневные расписания в очередь");
            }
            Ok(_) => {}
            Err(error) => {
                error!(%error, %delivery_date, "не удалось поставить ежедневные расписания в очередь")
            }
        }
    }
}

fn required_env(name: &str) -> Result<String> {
    env::var(name).with_context(|| format!("не задана переменная окружения {name}"))
}

async fn connect_with_retry(database_url: &str) -> Result<Store> {
    let mut last_error = None;
    for attempt in 1..=12 {
        match Store::connect(database_url).await {
            Ok(store) => return Ok(store),
            Err(error) => {
                warn!(attempt, %error, "PostgreSQL пока недоступен");
                last_error = Some(error);
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("не удалось подключиться к PostgreSQL")))
}

async fn outbox_worker(bot: Bot, store: Store) {
    let mut tick = tokio::time::interval(Duration::from_secs(10));
    loop {
        tick.tick().await;
        let messages = match store.ready_notifications(20).await {
            Ok(messages) => messages,
            Err(error) => {
                error!(%error, "ошибка чтения очереди уведомлений");
                continue;
            }
        };
        for message in messages {
            match bot
                .send_message(teloxide::types::ChatId(message.chat_id), message.body)
                .await
            {
                Ok(_) => {
                    if let Err(error) = store.mark_notification_sent(message.id).await {
                        error!(id = message.id, %error, "не удалось отметить уведомление отправленным");
                    }
                }
                Err(error) => {
                    if let Err(mark_error) = store
                        .mark_notification_failed(message.id, &error.to_string())
                        .await
                    {
                        error!(id = message.id, %mark_error, "не удалось отложить повторную отправку");
                    }
                    warn!(id = message.id, %error, "не удалось отправить уведомление");
                }
            }
        }
    }
}

#[cfg(test)]
mod schedule_tests {
    use super::due_daily_delivery;
    use chrono::{NaiveDate, TimeZone};
    use schedule_bot::format::DailyKind;

    #[test]
    fn selects_morning_delivery_at_seven_local_time() {
        let timezone = chrono_tz::Europe::Moscow;
        let before = timezone
            .with_ymd_and_hms(2026, 9, 25, 6, 59, 59)
            .single()
            .unwrap();
        let due = timezone
            .with_ymd_and_hms(2026, 9, 25, 7, 0, 0)
            .single()
            .unwrap();
        assert_eq!(due_daily_delivery(before), None);
        assert_eq!(
            due_daily_delivery(due),
            Some((
                NaiveDate::from_ymd_opt(2026, 9, 25).unwrap(),
                DailyKind::Today
            ))
        );
    }

    #[test]
    fn selects_tomorrow_delivery_at_nine_local_time() {
        let timezone = chrono_tz::Europe::Moscow;
        let due = timezone
            .with_ymd_and_hms(2026, 9, 25, 21, 0, 0)
            .single()
            .unwrap();
        assert_eq!(
            due_daily_delivery(due),
            Some((
                NaiveDate::from_ymd_opt(2026, 9, 26).unwrap(),
                DailyKind::Tomorrow
            ))
        );
    }
}
