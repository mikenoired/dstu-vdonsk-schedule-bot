use crate::AppState;
use crate::format::format_schedule;
use crate::stats::{self, Period};
use anyhow::{Context, Result, anyhow};
use chrono::NaiveDate;
use schedule_parser::Lesson;
use std::{error::Error, path::PathBuf};
use subtle::ConstantTimeEq;
use teloxide::{
    dispatching::UpdateHandler,
    net::Download,
    payloads::SendMessageSetters,
    prelude::*,
    requests::Requester,
    types::{
        CallbackQuery, ChatId, ChatKind, ChatMemberKind, InlineKeyboardButton,
        InlineKeyboardMarkup, InputFile, InputMedia, InputMediaPhoto, KeyboardButton,
        KeyboardMarkup, Message, PublicChatKind, Update,
    },
};
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

pub type HandlerResult = Result<(), Box<dyn Error + Send + Sync>>;

pub fn schema() -> UpdateHandler<Box<dyn Error + Send + Sync>> {
    dptree::entry()
        .branch(Update::filter_message().endpoint(handle_message))
        .branch(Update::filter_callback_query().endpoint(handle_callback))
}

async fn handle_message(bot: Bot, message: Message, state: AppState) -> HandlerResult {
    let Some(from) = message.from.as_ref() else {
        return Ok(());
    };
    let user_id = from.id.0 as i64;
    let username = from.username.clone();
    let chat_id = message.chat.id.0;
    match &message.chat.kind {
        ChatKind::Private(_) => {
            if !state.rate_limiter.allow(user_id, chat_id).await? {
                record_metric(&state, stats::Metric::RateLimited, 1).await;
                if state.rate_limiter.should_warn(user_id).await? {
                    send_text(
                        &bot,
                        message.chat.id,
                        "Слишком много запросов подряд. Подожди немного и попробуй ещё раз.",
                    )
                    .await?;
                }
                return Ok(());
            }
            record_metric(&state, stats::Metric::Commands, 1).await;
            handle_private_message(bot, message, state, user_id, username.as_deref()).await
        }
        ChatKind::Public(chat)
            if matches!(
                chat.kind,
                PublicChatKind::Group | PublicChatKind::Supergroup(_)
            ) =>
        {
            handle_group_message(bot, message, state, user_id).await
        }
        ChatKind::Public(_) => Ok(()),
    }
}

async fn handle_private_message(
    bot: Bot,
    message: Message,
    state: AppState,
    user_id: i64,
    username: Option<&str>,
) -> HandlerResult {
    let chat_id = message.chat.id.0;
    state
        .store
        .register_user(user_id, chat_id, username)
        .await?;

    if let Some(text) = message.text() {
        if text
            .split_whitespace()
            .next()
            .is_some_and(|command| command == "/stats" || command.starts_with("/stats@"))
        {
            if !state.store.is_admin(user_id).await? {
                send_text(
                    &bot,
                    message.chat.id,
                    "Эта команда доступна только администратору.",
                )
                .await?;
                return Ok(());
            }
            send_stats_dashboard(&bot, message.chat.id, &state, Period::Day).await?;
            return Ok(());
        }
        if text.starts_with("/start") {
            handle_start(&bot, &message, &state, user_id, text).await?;
            return Ok(());
        }
        if text.starts_with("/help") {
            send_text(&bot, message.chat.id, help_text()).await?;
            return Ok(());
        }
        if text.starts_with("/admin_link") {
            handle_admin_link(&bot, message.chat.id, &state, user_id).await?;
            return Ok(());
        }
        if text.starts_with("/notifications") || text == "🔔 Уведомления" {
            let user = state
                .store
                .user(user_id)
                .await?
                .ok_or_else(|| anyhow!("пользователь не найден"))?;
            let enabled = !user.daily_notifications_enabled;
            state
                .store
                .set_daily_notifications(user_id, enabled)
                .await?;
            let status = if enabled {
                "включены"
            } else {
                "выключены"
            };
            send_menu(
                &bot,
                message.chat.id,
                &format!("Ежедневные уведомления {status}."),
            )
            .await?;
            return Ok(());
        }
    }

    let user = state
        .store
        .user(user_id)
        .await?
        .ok_or_else(|| anyhow!("пользователь не найден"))?;
    if message.document().is_some() {
        handle_document(&bot, &message, &state, user_id, &user.role).await?;
        return Ok(());
    }
    let Some(text) = message.text().map(str::trim) else {
        send_text(
            &bot,
            message.chat.id,
            "Отправь название группы или выбери кнопку меню.",
        )
        .await?;
        return Ok(());
    };

    match user.flow_state.as_str() {
        "await_group" => handle_group_input(&bot, message.chat.id, &state, user_id, text).await?,
        "confirm_group" => {
            send_group_confirmation(
                &bot,
                message.chat.id,
                user.pending_group.as_deref().unwrap_or(""),
            )
            .await?
        }
        _ if is_today_button(text) => show_today(&bot, message.chat.id, &state, &user).await?,
        _ if is_week_button(text) => show_week(&bot, message.chat.id, &state, &user).await?,
        _ if is_search_button(text) => {
            state
                .store
                .set_flow_state(user_id, "await_search_kind", None)
                .await?;
            show_search_keyboard(&bot, message.chat.id).await?;
        }
        "await_search_kind" => {
            send_text(
                &bot,
                message.chat.id,
                "Выбери, что искать: преподавателя или аудиторию.",
            )
            .await?;
            show_search_keyboard(&bot, message.chat.id).await?;
        }
        "await_search_text" => handle_search(&bot, message.chat.id, &state, user_id, text).await?,
        _ => {
            if user.group_code.is_none() {
                state
                    .store
                    .set_flow_state(user_id, "await_group", None)
                    .await?;
                send_text(
                    &bot,
                    message.chat.id,
                    "Сначала укажи свою группу, например ИС11В.",
                )
                .await?;
            } else {
                send_menu(&bot, message.chat.id, "Выбери действие в меню.").await?;
            }
        }
    }
    Ok(())
}

async fn handle_group_message(
    bot: Bot,
    message: Message,
    state: AppState,
    user_id: i64,
) -> HandlerResult {
    let Some(text) = message.text().map(str::trim) else {
        return Ok(());
    };
    let Some((command, args)) = parse_group_command(text, &state.bot_username) else {
        return Ok(());
    };
    let chat_id = message.chat.id;
    if !state.rate_limiter.allow(user_id, chat_id.0).await? {
        record_metric(&state, stats::Metric::RateLimited, 1).await;
        return Ok(());
    }
    record_metric(&state, stats::Metric::Commands, 1).await;

    match command.as_str() {
        "disable" => {
            let member = match bot.get_chat_member(chat_id, UserId(user_id as u64)).await {
                Ok(member) => member,
                Err(_) => {
                    send_text(&bot, chat_id, "Не удалось проверить права администратора чата.").await?;
                    return Ok(());
                }
            };
            if !matches!(member.kind, ChatMemberKind::Owner(_) | ChatMemberKind::Administrator(_)) {
                send_text(&bot, chat_id, "Отключить расписание могут только администраторы чата.").await?;
                return Ok(());
            }
            if state.store.remove_chat_group(chat_id.0).await? {
                send_text(&bot, chat_id, "✅ Команды расписания отключены для этого чата. Чтобы включить снова, администратор может вызвать /setgroup ИС11В.").await?;
            } else {
                send_text(&bot, chat_id, "Для этого чата расписание уже не настроено.").await?;
            }
        }
        "setgroup" => {
            let member = match bot.get_chat_member(chat_id, UserId(user_id as u64)).await {
                Ok(member) => member,
                Err(_) => {
                    send_text(
                        &bot,
                        chat_id,
                        "Не удалось проверить права в чате. Убедись, что бот добавлен в группу и назначен администратором с минимальными разрешениями, затем повтори команду.",
                    )
                    .await?;
                    return Ok(());
                }
            };
            if !matches!(member.kind, ChatMemberKind::Owner(_) | ChatMemberKind::Administrator(_)) {
                send_text(&bot, chat_id, "Привязать учебную группу могут только администраторы чата.").await?;
                return Ok(());
            }
            let Some(requested) = single_argument(&args) else {
                send_text(&bot, chat_id, "Использование: /setgroup ИС11В").await?;
                return Ok(());
            };
            let group = state
                .store
                .known_groups()
                .await?
                .into_iter()
                .find(|group| group.eq_ignore_ascii_case(requested));
            let Some(group) = group else {
                send_text(&bot, chat_id, "Такой группы нет в опубликованном расписании. Проверь написание.").await?;
                return Ok(());
            };
            state.store.set_chat_group(chat_id.0, &group, user_id).await?;
            send_text(
                &bot,
                chat_id,
                &format!("✅ Чат привязан к группе {group}. Доступны /today, /week и /day ДД.ММ.ГГГГ."),
            )
            .await?;
        }
        "group" => match state.store.chat_group(chat_id.0).await? {
            Some(group) => send_text(&bot, chat_id, &format!("Для этого чата выбрана группа {group}."))
                .await?,
            None => send_text(&bot, chat_id, "Группа ещё не выбрана. Администратор чата может задать её командой /setgroup ИС11В.")
                .await?,
        },
        "today" | "week" | "day" => {
            let Some(group) = state.store.chat_group(chat_id.0).await? else {
                send_text(&bot, chat_id, "Группа ещё не выбрана. Администратор чата может задать её командой /setgroup ИС11В.")
                    .await?;
                return Ok(());
            };
            let date = if command == "day" {
                let Some(date) = single_argument(&args)
                    .and_then(|value| NaiveDate::parse_from_str(value, "%d.%m.%Y").ok())
                else {
                    send_text(&bot, chat_id, "Использование: /day 28.09.2026").await?;
                    return Ok(());
                };
                date
            } else {
                state.today()
            };
            let lessons = if command == "week" {
                state.store.week_lessons(&group, date).await?
            } else {
                state.store.today_lessons(&group, date).await?
            };
            if lessons.is_empty() {
                let label = if command == "week" {
                    format!("На неделю для группы {group} расписания не нашёл.")
                } else {
                    format!("На {} для группы {group} пар не нашёл.", date.format("%d.%m.%Y"))
                };
                send_text(&bot, chat_id, &label).await?;
            } else {
                let title = match command.as_str() {
                    "week" => format!("На неделю · группа {group}"),
                    "today" => format!("Сегодня · группа {group}"),
                    _ => format!("{} · группа {group}", date.format("%d.%m.%Y")),
                };
                send_schedule(&bot, chat_id, &title, &lessons, command != "today").await?;
            }
        }
        "help" | "start" => send_text(&bot, chat_id, group_help_text()).await?,
        _ => send_text(&bot, chat_id, group_help_text()).await?,
    }
    Ok(())
}

fn parse_group_command(text: &str, bot_username: &str) -> Option<(String, Vec<String>)> {
    let mut parts = text.split_whitespace();
    let command = parts.next()?;
    let command = command.strip_prefix('/')?;
    let (name, mention) = command
        .split_once('@')
        .map_or((command, None), |(name, mention)| (name, Some(mention)));
    if mention.is_some_and(|mention| !mention.eq_ignore_ascii_case(bot_username)) {
        return None;
    }
    Some((
        name.to_ascii_lowercase(),
        parts.map(str::to_owned).collect(),
    ))
}

fn single_argument(args: &[String]) -> Option<&str> {
    (args.len() == 1).then(|| args[0].as_str())
}

fn group_help_text() -> &'static str {
    "Команды расписания в группе:\n/setgroup ИС11В — привязать группу (только администратор чата)\n/disable — отключить расписание (только администратор чата)\n/group — показать выбранную группу\n/today — расписание на сегодня\n/week — расписание на неделю\n/day 28.09.2026 — расписание на дату"
}

async fn handle_start(
    bot: &Bot,
    message: &Message,
    state: &AppState,
    user_id: i64,
    text: &str,
) -> Result<()> {
    let chat_id = message.chat.id;
    let parameter = text.split_whitespace().nth(1).unwrap_or("");
    if let Some(token) = parameter.strip_prefix("adm_") {
        state.store.redeem_invite(user_id, token).await?;
    } else if let Some(token) = parameter.strip_prefix("bootstrap_") {
        if let Some(secret) = state.bootstrap_token.as_deref() {
            if bool::from(token.as_bytes().ct_eq(secret.as_bytes()))
                && state.store.claim_bootstrap_admin(user_id).await?
            {
                // Continue into the regular start flow so the new admin can also
                // choose a group and use the schedule menu.
            }
        }
    }

    let user = state
        .store
        .user(user_id)
        .await?
        .ok_or_else(|| anyhow!("пользователь не найден"))?;
    if user.role == "admin" {
        if let Some(group) = user.group_code {
            send_menu(
                bot,
                chat_id,
                &format!("Ты вошёл как администратор. Группа: {group}.\nВыбери расписание в меню, отправь Excel для публикации или используй /admin_link."),
            )
            .await?;
        } else {
            state
                .store
                .set_flow_state(user_id, "await_group", None)
                .await?;
            send_text(
                bot,
                chat_id,
                "Права администратора активны. Чтобы открыть расписание, напиши свою группу. Ты также можешь отправить Excel или создать ссылку командой /admin_link.",
            )
            .await?;
        }
    } else if let Some(group) = user.group_code {
        send_menu(
            bot,
            chat_id,
            &format!("С возвращением! Твоя группа: {group}."),
        )
        .await?;
    } else {
        state
            .store
            .set_flow_state(user_id, "await_group", None)
            .await?;
        send_text(bot, chat_id, "Привет! Напиши свою учебную группу.").await?;
    }
    Ok(())
}

async fn handle_group_input(
    bot: &Bot,
    chat_id: ChatId,
    state: &AppState,
    user_id: i64,
    input: &str,
) -> Result<()> {
    let groups = state.store.known_groups().await?;
    let input = input.trim();
    let found = groups
        .into_iter()
        .find(|group| group.to_lowercase() == input.to_lowercase());
    if let Some(group) = found {
        state.store.set_pending_group(user_id, &group).await?;
        send_group_confirmation(bot, chat_id, &group).await?;
    } else {
        send_text(bot, chat_id, "Не нашёл такую группу в опубликованных расписаниях. Проверь написание и попробуй ещё раз.").await?;
    }
    Ok(())
}

async fn handle_callback(bot: Bot, query: CallbackQuery, state: AppState) -> HandlerResult {
    let user_id = query.from.id.0 as i64;
    let chat_id = ChatId(user_id);
    if !state.rate_limiter.allow(user_id, chat_id.0).await? {
        record_metric(&state, stats::Metric::RateLimited, 1).await;
        bot.answer_callback_query(query.id.clone()).await?;
        if state.rate_limiter.should_warn(user_id).await? {
            send_text(
                &bot,
                chat_id,
                "Слишком много запросов подряд. Подожди немного и попробуй ещё раз.",
            )
            .await?;
        }
        return Ok(());
    }
    let data = query.data.as_deref().unwrap_or("").to_owned();
    if let Some(period_code) = data.strip_prefix("stats:") {
        let Some(period) = Period::from_callback(period_code) else {
            bot.answer_callback_query(query.id.clone()).await?;
            return Ok(());
        };
        let message = query
            .message
            .as_ref()
            .and_then(|message| message.regular_message());
        let private_message_id = message
            .filter(|message| message.chat.id.0 == user_id)
            .map(|message| (message.chat.id, message.id));
        let is_admin = state.store.is_admin(user_id).await?;
        if !stats_callback_allowed(
            is_admin,
            user_id,
            private_message_id.map(|(chat_id, _)| chat_id.0),
        ) {
            bot.answer_callback_query(query.id.clone())
                .text("Дашборд доступен администратору в личном чате.")
                .show_alert(true)
                .await?;
            return Ok(());
        }
        bot.answer_callback_query(query.id.clone()).await?;
        record_metric(&state, stats::Metric::Commands, 1).await;
        let (chat_id, message_id) = private_message_id.expect("checked above");
        edit_stats_dashboard(&bot, chat_id, message_id, &state, period).await?;
        return Ok(());
    }
    bot.answer_callback_query(query.id.clone()).await?;
    record_metric(&state, stats::Metric::Commands, 1).await;
    state
        .store
        .register_user(user_id, user_id, query.from.username.as_deref())
        .await?;

    match data.as_str() {
        "group:yes" => {
            if let Some(group) = state.store.confirm_group(user_id).await? {
                send_menu(&bot, chat_id, &format!("Группа {group} сохранена.")).await?;
            } else {
                send_text(
                    &bot,
                    chat_id,
                    "Не нашёл ожидающее подтверждения. Напиши группу ещё раз.",
                )
                .await?;
            }
        }
        "group:no" => {
            state.store.reject_pending_group(user_id).await?;
            send_text(&bot, chat_id, "Хорошо, напиши правильную группу.").await?;
        }
        "search:teacher" | "search:room" => {
            let kind = if data == "search:teacher" {
                "teacher"
            } else {
                "room"
            };
            state
                .store
                .set_flow_state(user_id, "await_search_text", Some(kind))
                .await?;
            let label = if kind == "teacher" {
                "имя преподавателя"
            } else {
                "номер или часть номера аудитории"
            };
            send_text(&bot, chat_id, &format!("Напиши {label} для поиска.")).await?;
        }
        "menu:today" => {
            let user = state
                .store
                .user(user_id)
                .await?
                .ok_or_else(|| anyhow!("пользователь не найден"))?;
            show_today(&bot, chat_id, &state, &user).await?;
        }
        "menu:week" => {
            let user = state
                .store
                .user(user_id)
                .await?
                .ok_or_else(|| anyhow!("пользователь не найден"))?;
            show_week(&bot, chat_id, &state, &user).await?;
        }
        "menu:search" => {
            state
                .store
                .set_flow_state(user_id, "await_search_kind", None)
                .await?;
            show_search_keyboard(&bot, chat_id).await?;
        }
        _ if data.starts_with("upload:confirm:") => {
            let id = Uuid::parse_str(data.trim_start_matches("upload:confirm:"))?;
            let publication = state.store.confirm_upload(id, user_id).await?;
            send_text(&bot, chat_id, &publication_message(&publication)).await?;
        }
        _ if data.starts_with("upload:cancel:") => {
            let id = Uuid::parse_str(data.trim_start_matches("upload:cancel:"))?;
            let (cancelled, source_key) = state.store.cancel_upload(id, user_id).await?;
            if cancelled {
                if let (Some(key), Some(archive)) = (source_key, state.archive.as_ref()) {
                    if let Err(error) = archive.delete(&key).await {
                        tracing::warn!(%error, %key, "не удалось удалить отменённую исходную таблицу из S3");
                    }
                }
                send_text(
                    &bot,
                    chat_id,
                    "Загрузка отменена, опубликованное расписание не менялось.",
                )
                .await?;
            } else {
                send_text(
                    &bot,
                    chat_id,
                    "Эту загрузку уже обработали или она принадлежит другому администратору.",
                )
                .await?;
            }
        }
        _ => {
            send_text(
                &bot,
                chat_id,
                "Эта кнопка уже устарела. Используй меню или загрузи файл снова.",
            )
            .await?
        }
    }
    Ok(())
}

async fn record_metric(state: &AppState, metric: stats::Metric, amount: u64) {
    match metric {
        stats::Metric::RateLimited => {
            crate::metrics::RATE_LIMITED.fetch_add(amount, std::sync::atomic::Ordering::Relaxed)
        }
        stats::Metric::OutboxSent => {
            crate::metrics::OUTBOX_SENT.fetch_add(amount, std::sync::atomic::Ordering::Relaxed)
        }
        stats::Metric::OutboxFailed => {
            crate::metrics::OUTBOX_FAILED.fetch_add(amount, std::sync::atomic::Ordering::Relaxed)
        }
        stats::Metric::DailyEnqueued => {
            crate::metrics::DAILY_ENQUEUED.fetch_add(amount, std::sync::atomic::Ordering::Relaxed)
        }
        stats::Metric::Commands => 0,
    };
    if let Err(error) = state.stats.record(metric, amount).await {
        tracing::warn!(%error, metric = metric.as_str(), "не удалось записать метрику в Redis");
    }
}

async fn send_stats_dashboard(
    bot: &Bot,
    chat_id: ChatId,
    state: &AppState,
    period: Period,
) -> Result<()> {
    let data = dashboard_data(state, period).await?;
    let jpeg = stats::render_jpeg(&data)?;
    bot.send_photo(chat_id, InputFile::memory(jpeg))
        .caption(format!(
            "📊 Статистика за {}\nНажми на период, чтобы обновить график.",
            period.label()
        ))
        .reply_markup(stats_keyboard(period))
        .await?;
    Ok(())
}

async fn edit_stats_dashboard(
    bot: &Bot,
    chat_id: ChatId,
    message_id: teloxide::types::MessageId,
    state: &AppState,
    period: Period,
) -> Result<()> {
    let data = dashboard_data(state, period).await?;
    let jpeg = stats::render_jpeg(&data)?;
    let media = InputMedia::Photo(
        InputMediaPhoto::new(InputFile::memory(jpeg)).caption(format!(
            "📊 Статистика за {}\nНажми на период, чтобы обновить график.",
            period.label()
        )),
    );
    bot.edit_message_media(chat_id, message_id, media)
        .reply_markup(stats_keyboard(period))
        .await?;
    Ok(())
}

async fn dashboard_data(state: &AppState, period: Period) -> Result<stats::DashboardData> {
    let queued = state.store.queued_notifications().await.unwrap_or(-1);
    let last_success = match state.stats.scheduler_last_success().await {
        Ok(Some(timestamp)) => timestamp,
        Ok(None) => crate::metrics::read(&crate::metrics::SCHEDULER_LAST_SUCCESS),
        Err(error) => {
            tracing::warn!(%error, "не удалось прочитать сохранённое время планировщика");
            crate::metrics::read(&crate::metrics::SCHEDULER_LAST_SUCCESS)
        }
    };
    Ok(state.stats.dashboard(period, queued, last_success).await?)
}

fn stats_keyboard(selected: Period) -> InlineKeyboardMarkup {
    let button = |period: Period, label: &str| {
        let label = if period == selected {
            format!("✅ {label}")
        } else {
            label.to_owned()
        };
        InlineKeyboardButton::callback(label, format!("stats:{}", period.callback()))
    };
    InlineKeyboardMarkup::new(vec![
        vec![
            button(Period::ThirtyMinutes, "30 мин"),
            button(Period::Hour, "1 час"),
        ],
        vec![
            button(Period::Day, "24 часа"),
            button(Period::Week, "7 дней"),
        ],
    ])
}

fn stats_callback_allowed(is_admin: bool, user_id: i64, callback_chat_id: Option<i64>) -> bool {
    is_admin && callback_chat_id == Some(user_id)
}

async fn handle_search(
    bot: &Bot,
    chat_id: ChatId,
    state: &AppState,
    user_id: i64,
    query: &str,
) -> Result<()> {
    let user = state
        .store
        .user(user_id)
        .await?
        .ok_or_else(|| anyhow!("пользователь не найден"))?;
    let kind = user.search_kind.as_deref().unwrap_or("teacher");
    let today = state.today();
    let lessons = state.store.search_lessons(kind, query, today).await?;
    state.store.set_flow_state(user_id, "menu", None).await?;
    if lessons.is_empty() {
        send_menu(bot, chat_id, "Совпадений не нашёл. Попробуй другой запрос.").await?;
    } else {
        let title = if kind == "teacher" {
            format!("Поиск преподавателя: {query}")
        } else {
            format!(
                "Поиск аудитории: {}",
                schedule_parser::display_room_name(query)
            )
        };
        send_schedule(bot, chat_id, &title, &lessons, true).await?;
    }
    Ok(())
}

async fn show_today(
    bot: &Bot,
    chat_id: ChatId,
    state: &AppState,
    user: &crate::store::User,
) -> Result<()> {
    let Some(group) = user.group_code.as_deref() else {
        state
            .store
            .set_flow_state(user.telegram_id, "await_group", None)
            .await?;
        send_text(
            bot,
            chat_id,
            "Напиши свою группу, чтобы я нашёл расписание.",
        )
        .await?;
        return Ok(());
    };
    let today = state.today();
    let lessons = state.store.today_lessons(group, today).await?;
    if lessons.is_empty() {
        send_menu(
            bot,
            chat_id,
            &format!(
                "На сегодня ({}) занятий не нашёл.",
                today.format("%d.%m.%Y")
            ),
        )
        .await?;
    } else {
        send_schedule(
            bot,
            chat_id,
            &format!("Сегодня · группа {group}"),
            &lessons,
            false,
        )
        .await?;
    }
    Ok(())
}

async fn show_week(
    bot: &Bot,
    chat_id: ChatId,
    state: &AppState,
    user: &crate::store::User,
) -> Result<()> {
    let Some(group) = user.group_code.as_deref() else {
        state
            .store
            .set_flow_state(user.telegram_id, "await_group", None)
            .await?;
        send_text(
            bot,
            chat_id,
            "Напиши свою группу, чтобы я нашёл расписание.",
        )
        .await?;
        return Ok(());
    };
    let lessons = state.store.week_lessons(group, state.today()).await?;
    if lessons.is_empty() {
        send_menu(
            bot,
            chat_id,
            "Для этой группы пока нет опубликованного расписания.",
        )
        .await?;
    } else {
        send_schedule(
            bot,
            chat_id,
            &format!("На неделю · группа {group}"),
            &lessons,
            true,
        )
        .await?;
    }
    Ok(())
}

async fn handle_admin_link(
    bot: &Bot,
    chat_id: ChatId,
    state: &AppState,
    user_id: i64,
) -> Result<()> {
    if !state.store.is_admin(user_id).await? {
        send_text(bot, chat_id, "Команда доступна только администратору.").await?;
        return Ok(());
    }
    let token = state.store.create_invite(user_id).await?;
    let username = bot.get_me().await?.username().to_owned();
    send_text(
        bot,
        chat_id,
        &format!(
            "Одноразовая ссылка действует 15 минут:\nhttps://t.me/{username}?start=adm_{token}"
        ),
    )
    .await?;
    Ok(())
}

async fn handle_document(
    bot: &Bot,
    message: &Message,
    state: &AppState,
    user_id: i64,
    role: &str,
) -> Result<()> {
    if role != "admin" {
        send_text(
            bot,
            message.chat.id,
            "Загружать расписание может только администратор.",
        )
        .await?;
        return Ok(());
    }
    let document = message
        .document()
        .ok_or_else(|| anyhow!("не найден документ"))?;
    let file_name = document.file_name.as_deref().unwrap_or("schedule.xls");
    let extension = PathBuf::from(file_name)
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_lowercase();
    if !matches!(extension.as_str(), "xls" | "xlsx" | "xlsb") {
        send_text(
            bot,
            message.chat.id,
            "Поддерживаются файлы Excel .xls, .xlsx и .xlsb.",
        )
        .await?;
        return Ok(());
    }
    let tg_file = bot.get_file(document.file.id.clone()).await?;
    let path = std::env::temp_dir().join(format!("schedule-{}.{}", Uuid::new_v4(), extension));
    let download_result = async {
        let mut file = tokio::fs::File::create(&path).await?;
        bot.download_file(&tg_file.path, &mut file).await?;
        file.flush().await?;
        Ok::<(), anyhow::Error>(())
    }
    .await;
    if let Err(error) = download_result {
        let _ = tokio::fs::remove_file(&path).await;
        return Err(error.into());
    }
    let bytes = match tokio::fs::read(&path).await {
        Ok(bytes) => bytes,
        Err(error) => {
            let _ = tokio::fs::remove_file(&path).await;
            return Err(error.into());
        }
    };
    if bytes.len() > 15 * 1024 * 1024 {
        let _ = tokio::fs::remove_file(&path).await;
        send_text(bot, message.chat.id, "Файл больше лимита 15 МБ.").await?;
        return Ok(());
    }
    let hash = sha256_hex(&bytes);
    let parse_path = path.clone();
    let parsed = tokio::task::spawn_blocking(move || schedule_parser::parse_file(parse_path)).await;
    let _ = tokio::fs::remove_file(&path).await;
    let parsed = parsed.context("задача парсинга завершилась аварийно")?;
    let lessons = match parsed {
        Ok(lessons) => lessons,
        Err(error) => {
            send_text(
                bot,
                message.chat.id,
                &format!("Не распознал расписание в файле: {error}"),
            )
            .await?;
            return Ok(());
        }
    };
    match state
        .store
        .preview_upload(user_id, file_name, &hash, lessons)
        .await
    {
        Ok(preview) => {
            let Some(archive) = state.archive.as_ref() else {
                state.store.cancel_upload(preview.id, user_id).await?;
                send_text(
                    bot,
                    message.chat.id,
                    "Архив Excel ещё не настроен. Бот продолжает работать, но публикация расписания временно недоступна.",
                )
                .await?;
                return Ok(());
            };
            let key = match archive.save(preview.id, &extension, &bytes).await {
                Ok(key) => key,
                Err(error) => {
                    state.store.cancel_upload(preview.id, user_id).await?;
                    send_text(
                        bot,
                        message.chat.id,
                        &format!("Не удалось сохранить исходную таблицу в архиве: {error}"),
                    )
                    .await?;
                    return Ok(());
                }
            };
            if let Err(error) = state
                .store
                .attach_source_key(preview.id, user_id, &key)
                .await
            {
                state.store.cancel_upload(preview.id, user_id).await?;
                if let Err(delete_error) = archive.delete(&key).await {
                    tracing::warn!(%delete_error, "не удалось удалить неиспользуемый Excel из S3");
                }
                send_text(
                    bot,
                    message.chat.id,
                    &format!("Не удалось подготовить исходную таблицу к публикации: {error}"),
                )
                .await?;
                return Ok(());
            }
            let summary = preview_message(&preview);
            let keyboard = InlineKeyboardMarkup::new(vec![vec![
                InlineKeyboardButton::callback(
                    "✅ Подтвердить публикацию",
                    format!("upload:confirm:{}", preview.id),
                ),
                InlineKeyboardButton::callback("Отмена", format!("upload:cancel:{}", preview.id)),
            ]]);
            bot.send_message(message.chat.id, summary)
                .reply_markup(keyboard)
                .await?;
        }
        Err(error) => {
            send_text(
                bot,
                message.chat.id,
                &format!("Файл не прошёл проверку: {error}"),
            )
            .await?;
        }
    }
    Ok(())
}

async fn send_group_confirmation(bot: &Bot, chat_id: ChatId, group: &str) -> Result<()> {
    let keyboard = InlineKeyboardMarkup::new(vec![vec![
        InlineKeyboardButton::callback("✅ Да", "group:yes"),
        InlineKeyboardButton::callback("❌ Нет", "group:no"),
    ]]);
    bot.send_message(chat_id, format!("Твоя группа — {group}?"))
        .reply_markup(keyboard)
        .await?;
    Ok(())
}

async fn show_search_keyboard(bot: &Bot, chat_id: ChatId) -> Result<()> {
    let keyboard = InlineKeyboardMarkup::new(vec![
        vec![InlineKeyboardButton::callback(
            "👩‍🏫 Преподаватель",
            "search:teacher",
        )],
        vec![InlineKeyboardButton::callback(
            "🏫 Аудитория",
            "search:room",
        )],
    ]);
    bot.send_message(chat_id, "Что ищем?")
        .reply_markup(keyboard)
        .await?;
    Ok(())
}

async fn send_schedule(
    bot: &Bot,
    chat_id: ChatId,
    title: &str,
    lessons: &[Lesson],
    show_dates: bool,
) -> Result<()> {
    send_long_text(bot, chat_id, &format_schedule(title, lessons, show_dates)).await
}

async fn send_long_text(bot: &Bot, chat_id: ChatId, text: &str) -> Result<()> {
    let mut chunk = String::new();
    for line in text.lines() {
        if !chunk.is_empty() && chunk.len() + line.len() + 1 > 3800 {
            send_text(bot, chat_id, &chunk).await?;
            chunk.clear();
        }
        if line.len() > 3800 {
            let mut piece = String::new();
            for character in line.chars() {
                if piece.len() + character.len_utf8() > 3500 {
                    send_text(bot, chat_id, &piece).await?;
                    piece.clear();
                }
                piece.push(character);
            }
            if !piece.is_empty() {
                send_text(bot, chat_id, &piece).await?;
            }
        } else {
            if !chunk.is_empty() {
                chunk.push('\n');
            }
            chunk.push_str(line);
        }
    }
    if !chunk.is_empty() {
        send_text(bot, chat_id, &chunk).await?;
    }
    Ok(())
}

async fn send_menu(bot: &Bot, chat_id: ChatId, text: &str) -> Result<()> {
    let keyboard = KeyboardMarkup::new(vec![
        vec![
            KeyboardButton::new("📅 Сегодня"),
            KeyboardButton::new("🗓️ На неделю"),
        ],
        vec![KeyboardButton::new("🔎 Расширенный поиск")],
        vec![KeyboardButton::new("🔔 Уведомления")],
    ])
    .resize_keyboard();
    bot.send_message(chat_id, text)
        .reply_markup(keyboard)
        .await?;
    Ok(())
}

async fn send_text(bot: &Bot, chat_id: ChatId, text: &str) -> Result<()> {
    bot.send_message(chat_id, text).await?;
    Ok(())
}

fn is_today_button(text: &str) -> bool {
    text == "Сегодня" || text == "📅 Сегодня"
}
fn is_week_button(text: &str) -> bool {
    text == "На неделю" || text == "🗓️ На неделю"
}
fn is_search_button(text: &str) -> bool {
    text == "Расширенный поиск" || text == "🔎 Расширенный поиск"
}

fn preview_message(preview: &crate::store::UploadPreview) -> String {
    let kind = match preview.kind {
        crate::domain::UpdateKind::NewWeek => "🆕 Новая неделя",
        crate::domain::UpdateKind::Correction => "🔄 Актуализация существующей недели",
    };
    let dates = preview
        .diff
        .changed_dates
        .iter()
        .map(|date| date.format("%d.%m").to_string())
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "{kind}\nПериод: {}–{}\nЗатронутые группы: {}\nИзменённые дни: {}\nДобавлено пар: {}, удалено пар: {}\n\nОпубликовать это расписание?",
        preview.week_start.format("%d.%m.%Y"),
        preview.week_end.format("%d.%m.%Y"),
        if preview.diff.changed_groups.is_empty() {
            "—".to_owned()
        } else {
            preview.diff.changed_groups.join(", ")
        },
        if dates.is_empty() {
            "—".to_owned()
        } else {
            dates
        },
        preview.diff.added,
        preview.diff.removed,
    )
}

fn publication_message(publication: &crate::store::Publication) -> String {
    match publication.kind {
        crate::domain::UpdateKind::NewWeek => format!(
            "✅ Новая неделя опубликована: {}–{}. Уведомления студентам поставлены в очередь.",
            publication.week_start.format("%d.%m"),
            publication.week_end.format("%d.%m.%Y")
        ),
        crate::domain::UpdateKind::Correction => format!(
            "✅ Расписание актуализировано на {}–{}. Изменены дни: {}. Уведомления поставлены в очередь.",
            publication.week_start.format("%d.%m"),
            publication.week_end.format("%d.%m.%Y"),
            publication
                .diff
                .changed_dates
                .iter()
                .map(|date| date.format("%d.%m").to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn help_text() -> &'static str {
    "Команды и меню:\n/start — начать или открыть меню\n/notifications — переключить ежедневные уведомления\n/admin_link — создать одноразовую ссылку администратора\n\nДля студентов доступны расписание на сегодня, на неделю и поиск по преподавателю или аудитории. В группах: /setgroup, /disable, /today, /week, /day."
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::{parse_group_command, stats_callback_allowed};

    #[test]
    fn parses_group_command_and_arguments() {
        assert_eq!(
            parse_group_command("/day@ScheduleBot 28.09.2026", "schedulebot"),
            Some(("day".to_owned(), vec!["28.09.2026".to_owned()]))
        );
    }

    #[test]
    fn ignores_commands_mentioned_to_another_bot() {
        assert_eq!(parse_group_command("/today@OtherBot", "schedulebot"), None);
    }

    #[test]
    fn parses_commands_without_explicit_mention() {
        assert_eq!(
            parse_group_command("/week", "schedulebot"),
            Some(("week".to_owned(), vec![]))
        );
    }

    #[test]
    fn dashboard_callbacks_are_limited_to_admin_private_chat() {
        assert!(stats_callback_allowed(true, 42, Some(42)));
        assert!(!stats_callback_allowed(false, 42, Some(42)));
        assert!(!stats_callback_allowed(true, 42, Some(-10042)));
        assert!(!stats_callback_allowed(true, 42, None));
    }
}
