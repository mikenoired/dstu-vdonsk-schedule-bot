use crate::AppState;
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
        CallbackQuery, ChatId, ChatKind, InlineKeyboardButton, InlineKeyboardMarkup,
        KeyboardButton, KeyboardMarkup, Message, Update,
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
    if !matches!(message.chat.kind, ChatKind::Private(_)) {
        return Ok(());
    }
    let Some(from) = message.from.as_ref() else {
        return Ok(());
    };
    let user_id = from.id.0 as i64;
    let chat_id = message.chat.id.0;
    state
        .store
        .register_user(user_id, chat_id, from.username.as_deref())
        .await?;

    if let Some(text) = message.text() {
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
        if state.store.redeem_invite(user_id, token).await? {
            send_text(bot, chat_id, "✅ Права администратора выданы. Отправь Excel-файл расписания или используй /admin_link для приглашения администратора.").await?;
            return Ok(());
        }
    } else if let Some(token) = parameter.strip_prefix("bootstrap_") {
        if let Some(secret) = state.bootstrap_token.as_deref() {
            if bool::from(token.as_bytes().ct_eq(secret.as_bytes()))
                && state.store.claim_bootstrap_admin(user_id).await?
            {
                send_text(bot, chat_id, "✅ Создан первый администратор. Отправь Excel-файл или создай одноразовую ссылку командой /admin_link.").await?;
                return Ok(());
            }
        }
    }

    let user = state
        .store
        .user(user_id)
        .await?
        .ok_or_else(|| anyhow!("пользователь не найден"))?;
    if user.role == "admin" {
        send_text(
            bot,
            chat_id,
            "Ты вошёл как администратор. Пришли Excel с расписанием или используй /admin_link.",
        )
        .await?;
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
    let data = query.data.as_deref().unwrap_or("").to_owned();
    bot.answer_callback_query(query.id.clone()).await?;
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
            if state.store.cancel_upload(id, user_id).await? {
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
    let mut lines = vec![title.to_owned(), "━━━━━━━━━━━━━━━━".to_owned()];
    let mut current_date: Option<NaiveDate> = None;
    for lesson in lessons {
        if show_dates && current_date != Some(lesson.date) {
            lines.push(String::new());
            lines.push(format!(
                "🗓️ {} · {}",
                lesson.weekday,
                lesson.date.format("%d.%m.%Y")
            ));
            current_date = Some(lesson.date);
        }
        lines.push(format!(
            "🕒 {}–{} · пара №{}",
            lesson.start_time, lesson.end_time, lesson.lesson_number
        ));
        lines.push(format!("👥 {}", lesson.groups.join(", ")));
        lines.push(format!("📖 {}", lesson.subject));
        let mut details = Vec::new();
        if let Some(kind) = &lesson.lesson_type {
            details.push(format!("🧩 {kind}"));
        }
        if let Some(teacher) = &lesson.teacher {
            details.push(format!("👩‍🏫 {teacher}"));
        }
        if let Some(room) = &lesson.room {
            details.push(format!("📍 {room}"));
        }
        if !details.is_empty() {
            lines.push(details.join(" · "));
        }
        lines.push(String::new());
    }
    lines.push(format!("✨ Всего пар: {}", lessons.len()));
    send_long_text(bot, chat_id, &lines.join("\n")).await
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
    "Команды и меню:\n/start — начать или открыть меню\n/admin_link — создать одноразовую ссылку администратора\n\nДля студентов доступны расписание на сегодня, на неделю и поиск по преподавателю или аудитории."
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(bytes))
}
