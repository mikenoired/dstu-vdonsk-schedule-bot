use anyhow::{Context, Result, bail};
use axum::{Router, extract::State, http::StatusCode, response::IntoResponse, routing::get};
use chrono::{NaiveDate, NaiveTime, Utc};
use schedule_bot::{
    AppState, archive::SourceArchive, format::DailyKind, handlers, metrics,
    rate_limit::RateLimiter, store::Store,
};
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

const SEND_MISSED_DELIVERIES: bool = false;
const OUTBOX_BATCH_SIZE: i64 = 100;

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
    let redis_url = required_env("REDIS_URL")?;
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
    let rate_limiter = connect_redis_with_retry(&redis_url).await?;
    let archive = match SourceArchive::from_env() {
        Ok(Some(archive)) => Some(archive),
        Ok(None) => {
            warn!("S3 не настроен; бот запустится, но публикация Excel будет недоступна");
            None
        }
        Err(error) => {
            warn!(%error, "ошибка конфигурации S3; бот запустится без архивации Excel");
            None
        }
    };
    let bot = Bot::new(bot_token);
    let me = bot
        .get_me()
        .await
        .context("не удалось проверить TELEGRAM_BOT_TOKEN")?;
    info!(bot = ?me.username(), "бот запущен");
    let bot_username = me.username().to_owned();
    bot.set_my_commands([
        BotCommand::new("setgroup", "привязать группу к чату"),
        BotCommand::new("disable", "отключить расписание в чате"),
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
        rate_limiter,
        archive,
    };
    spawn_http_endpoints(state.clone())?;
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

fn due_daily_delivery(
    now: chrono::DateTime<chrono_tz::Tz>,
    send_missed: bool,
) -> Option<(NaiveDate, DailyKind)> {
    let morning = NaiveTime::from_hms_opt(7, 0, 0).expect("valid time");
    let evening = NaiveTime::from_hms_opt(21, 0, 0).expect("valid time");
    let time = now.time();
    let due =
        |target| send_missed || (time >= target && time < target + chrono::Duration::minutes(1));
    if time >= evening && due(evening) {
        Some((now.date_naive() + chrono::Days::new(1), DailyKind::Tomorrow))
    } else if time >= morning && due(morning) {
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
        let Some((delivery_date, kind)) = due_daily_delivery(now, SEND_MISSED_DELIVERIES) else {
            continue;
        };
        match state
            .store
            .enqueue_daily_schedules(delivery_date, kind)
            .await
        {
            Ok(queued) if queued > 0 => {
                metrics::DAILY_ENQUEUED.fetch_add(queued, std::sync::atomic::Ordering::Relaxed);
                metrics::SCHEDULER_LAST_SUCCESS.store(
                    Utc::now().timestamp() as u64,
                    std::sync::atomic::Ordering::Relaxed,
                );
                info!(%delivery_date, delivery = kind.as_str(), queued, "поставлены ежедневные расписания в очередь");
            }
            Ok(_) => {
                metrics::SCHEDULER_LAST_SUCCESS.store(
                    Utc::now().timestamp() as u64,
                    std::sync::atomic::Ordering::Relaxed,
                );
            }
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

async fn connect_redis_with_retry(redis_url: &str) -> Result<RateLimiter> {
    let mut last_error = None;
    for attempt in 1..=12 {
        match RateLimiter::connect(redis_url).await {
            Ok(limiter) => return Ok(limiter),
            Err(error) => {
                warn!(attempt, %error, "Redis пока недоступен");
                last_error = Some(error);
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("не удалось подключиться к Redis")))
}

async fn outbox_worker(bot: Bot, store: Store) {
    let mut tick = tokio::time::interval(Duration::from_secs(10));
    loop {
        tick.tick().await;
        let messages = match store.ready_notifications(OUTBOX_BATCH_SIZE).await {
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
                    metrics::OUTBOX_SENT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if let Err(error) = store.mark_notification_sent(message.id).await {
                        error!(id = message.id, %error, "не удалось отметить уведомление отправленным");
                    }
                }
                Err(error) => {
                    metrics::OUTBOX_FAILED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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

fn spawn_http_endpoints(state: AppState) -> Result<()> {
    let port = env::var("PORT")
        .unwrap_or_else(|_| "3000".to_owned())
        .parse::<u16>()
        .context("PORT должен быть числом от 1 до 65535")?;
    tokio::spawn(async move {
        let app = Router::new()
            .route("/healthz", get(health_endpoint))
            .route("/metrics", get(metrics_endpoint))
            .with_state(state);
        let listener = match tokio::net::TcpListener::bind(("0.0.0.0", port)).await {
            Ok(listener) => listener,
            Err(error) => {
                error!(%error, port, "не удалось запустить HTTP endpoint здоровья и метрик");
                return;
            }
        };
        info!(port, "HTTP endpoints доступны на /healthz и /metrics");
        if let Err(error) = axum::serve(listener, app).await {
            error!(%error, "HTTP endpoint завершился с ошибкой");
        }
    });
    Ok(())
}

async fn health_endpoint(State(state): State<AppState>) -> StatusCode {
    if state.store.health().await.is_ok() && state.rate_limiter.health().await.is_ok() {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}

async fn metrics_endpoint(State(state): State<AppState>) -> impl IntoResponse {
    let queued = state.store.queued_notifications().await.unwrap_or(-1);
    let output = format!(
        "# TYPE schedule_rate_limited_total counter\nschedule_rate_limited_total {}\n\
         # TYPE schedule_outbox_sent_total counter\nschedule_outbox_sent_total {}\n\
         # TYPE schedule_outbox_failed_total counter\nschedule_outbox_failed_total {}\n\
         # TYPE schedule_daily_enqueued_total counter\nschedule_daily_enqueued_total {}\n\
         # TYPE schedule_outbox_queued gauge\nschedule_outbox_queued {}\n\
         # TYPE schedule_worker_last_success_timestamp_seconds gauge\nschedule_worker_last_success_timestamp_seconds {}\n",
        metrics::read(&metrics::RATE_LIMITED),
        metrics::read(&metrics::OUTBOX_SENT),
        metrics::read(&metrics::OUTBOX_FAILED),
        metrics::read(&metrics::DAILY_ENQUEUED),
        queued,
        metrics::read(&metrics::SCHEDULER_LAST_SUCCESS),
    );
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4",
        )],
        output,
    )
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
        assert_eq!(due_daily_delivery(before, false), None);
        assert_eq!(
            due_daily_delivery(due, false),
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
            due_daily_delivery(due, false),
            Some((
                NaiveDate::from_ymd_opt(2026, 9, 26).unwrap(),
                DailyKind::Tomorrow
            ))
        );
    }

    #[test]
    fn skips_missed_delivery_after_service_restart_by_default() {
        let late = chrono_tz::Europe::Moscow
            .with_ymd_and_hms(2026, 9, 25, 8, 0, 0)
            .single()
            .unwrap();
        assert_eq!(due_daily_delivery(late, false), None);
        assert!(due_daily_delivery(late, true).is_some());
    }
}
