use anyhow::{Context, Result, bail};
use schedule_bot::{AppState, handlers, store::Store};
use std::{env, time::Duration};
use teloxide::{Bot, dispatching::Dispatcher, prelude::Requester};
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

    let state = AppState {
        store,
        bootstrap_token,
        timezone,
    };
    tokio::spawn(outbox_worker(bot.clone(), state.store.clone()));

    Dispatcher::builder(bot, handlers::schema())
        .dependencies(dptree::deps![state])
        .enable_ctrlc_handler()
        .build()
        .dispatch()
        .await;
    Ok(())
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
