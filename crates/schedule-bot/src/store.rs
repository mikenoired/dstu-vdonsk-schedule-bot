use crate::domain::{ScheduleDiff, UpdateKind, compare_schedules, validate_schedule};
use anyhow::{Context, Result, bail};
use chrono::NaiveDate;
use schedule_parser::{Lesson, display_room_name};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Postgres, Row, Transaction, postgres::PgPoolOptions, types::Json};
use std::collections::BTreeSet;
use uuid::Uuid;

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

#[derive(Clone)]
pub struct Store {
    pool: PgPool,
}

#[derive(Debug, Clone)]
pub struct User {
    pub telegram_id: i64,
    pub chat_id: i64,
    pub role: String,
    pub group_code: Option<String>,
    pub pending_group: Option<String>,
    pub flow_state: String,
    pub search_kind: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadPreview {
    pub id: Uuid,
    pub week_start: NaiveDate,
    pub week_end: NaiveDate,
    pub kind: UpdateKind,
    pub diff: ScheduleDiff,
}

#[derive(Debug, Clone)]
pub struct Publication {
    pub week_start: NaiveDate,
    pub week_end: NaiveDate,
    pub kind: UpdateKind,
    pub diff: ScheduleDiff,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct QueuedMessage {
    pub id: i64,
    pub chat_id: i64,
    pub body: String,
}

#[derive(sqlx::FromRow)]
struct DbLesson {
    groups: Vec<String>,
    lesson_date: NaiveDate,
    weekday: String,
    lesson_number: i16,
    start_time: String,
    end_time: String,
    subject: String,
    lesson_type: Option<String>,
    teacher: Option<String>,
    room: Option<String>,
    description: String,
}

impl From<DbLesson> for Lesson {
    fn from(row: DbLesson) -> Self {
        Self {
            groups: row.groups,
            date: row.lesson_date,
            weekday: row.weekday,
            lesson_number: row.lesson_number as u8,
            start_time: row.start_time,
            end_time: row.end_time,
            subject: row.subject,
            lesson_type: row.lesson_type,
            teacher: row.teacher,
            room: row.room,
            description: row.description,
        }
    }
}

impl Store {
    pub fn from_pool(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn connect(database_url: &str) -> Result<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .acquire_timeout(std::time::Duration::from_secs(10))
            .connect(database_url)
            .await
            .context("не удалось подключиться к PostgreSQL")?;
        MIGRATOR
            .run(&pool)
            .await
            .context("ошибка миграции базы данных")?;
        Ok(Self { pool })
    }

    pub async fn register_user(
        &self,
        telegram_id: i64,
        chat_id: i64,
        username: Option<&str>,
    ) -> Result<User> {
        sqlx::query(
            "INSERT INTO users (telegram_id, chat_id, username) VALUES ($1, $2, $3) \
             ON CONFLICT (telegram_id) DO UPDATE SET chat_id = EXCLUDED.chat_id, \
             username = EXCLUDED.username, updated_at = now()",
        )
        .bind(telegram_id)
        .bind(chat_id)
        .bind(username)
        .execute(&self.pool)
        .await?;
        self.user(telegram_id)
            .await?
            .context("пользователь не сохранился")
    }

    pub async fn user(&self, telegram_id: i64) -> Result<Option<User>> {
        Ok(sqlx::query_as::<_, UserRow>(
            "SELECT telegram_id, chat_id, role, group_code, pending_group, flow_state, search_kind \
             FROM users WHERE telegram_id = $1",
        )
        .bind(telegram_id)
        .fetch_optional(&self.pool)
        .await?
        .map(Into::into))
    }

    pub async fn is_admin(&self, telegram_id: i64) -> Result<bool> {
        Ok(sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (SELECT 1 FROM users WHERE telegram_id = $1 AND role = 'admin')",
        )
        .bind(telegram_id)
        .fetch_one(&self.pool)
        .await?)
    }

    pub async fn known_groups(&self) -> Result<Vec<String>> {
        Ok(sqlx::query_scalar(
            "SELECT DISTINCT group_name FROM schedule_weeks w \
             JOIN lessons l ON l.version_id = w.current_version \
             CROSS JOIN LATERAL unnest(l.groups) AS g(group_name) \
             ORDER BY group_name",
        )
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn chat_group(&self, chat_id: i64) -> Result<Option<String>> {
        Ok(
            sqlx::query_scalar("SELECT group_code FROM chat_schedules WHERE chat_id = $1")
                .bind(chat_id)
                .fetch_optional(&self.pool)
                .await?,
        )
    }

    pub async fn set_chat_group(
        &self,
        chat_id: i64,
        group: &str,
        configured_by: i64,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO chat_schedules (chat_id, group_code, configured_by) VALUES ($1, $2, $3) \
             ON CONFLICT (chat_id) DO UPDATE SET group_code = EXCLUDED.group_code, \
             configured_by = EXCLUDED.configured_by, updated_at = now()",
        )
        .bind(chat_id)
        .bind(group)
        .bind(configured_by)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn set_flow_state(
        &self,
        telegram_id: i64,
        state: &str,
        search_kind: Option<&str>,
    ) -> Result<()> {
        sqlx::query("UPDATE users SET flow_state = $2, search_kind = $3, updated_at = now() WHERE telegram_id = $1")
            .bind(telegram_id)
            .bind(state)
            .bind(search_kind)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn set_pending_group(&self, telegram_id: i64, group: &str) -> Result<()> {
        sqlx::query("UPDATE users SET pending_group = $2, flow_state = 'confirm_group', updated_at = now() WHERE telegram_id = $1")
            .bind(telegram_id)
            .bind(group)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn confirm_group(&self, telegram_id: i64) -> Result<Option<String>> {
        let group: Option<String> = sqlx::query_scalar(
            "UPDATE users SET group_code = pending_group, pending_group = NULL, flow_state = 'menu', \
             updated_at = now() WHERE telegram_id = $1 AND pending_group IS NOT NULL RETURNING group_code",
        )
        .bind(telegram_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(group)
    }

    pub async fn reject_pending_group(&self, telegram_id: i64) -> Result<()> {
        sqlx::query("UPDATE users SET pending_group = NULL, flow_state = 'await_group', updated_at = now() WHERE telegram_id = $1")
            .bind(telegram_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn redeem_invite(&self, telegram_id: i64, token: &str) -> Result<bool> {
        let token_hash = sha256_hex(token.as_bytes());
        let mut tx = self.pool.begin().await?;
        let redeemed = sqlx::query_scalar::<_, String>(
            "UPDATE admin_invites SET consumed_at = now(), consumed_by = $2 \
             WHERE token_sha256 = $1 AND consumed_at IS NULL AND expires_at > now() \
             RETURNING token_sha256",
        )
        .bind(token_hash)
        .bind(telegram_id)
        .fetch_optional(&mut *tx)
        .await?
        .is_some();
        if redeemed {
            sqlx::query(
                "UPDATE users SET role = 'admin', updated_at = now() WHERE telegram_id = $1",
            )
            .bind(telegram_id)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(redeemed)
    }

    pub async fn claim_bootstrap_admin(&self, telegram_id: i64) -> Result<bool> {
        let mut tx = self.pool.begin().await?;
        let inserted = sqlx::query_scalar::<_, i64>(
            "INSERT INTO admin_bootstrap_claims (claim_key, telegram_id) VALUES ('primary', $1) \
             ON CONFLICT (claim_key) DO NOTHING RETURNING telegram_id",
        )
        .bind(telegram_id)
        .fetch_optional(&mut *tx)
        .await?
        .is_some();
        if inserted {
            sqlx::query(
                "UPDATE users SET role = 'admin', updated_at = now() WHERE telegram_id = $1",
            )
            .bind(telegram_id)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(inserted)
    }

    pub async fn create_invite(&self, admin_id: i64) -> Result<String> {
        let token = Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO admin_invites (token_sha256, created_by, expires_at) \
             VALUES ($1, $2, now() + interval '15 minutes')",
        )
        .bind(sha256_hex(token.as_bytes()))
        .bind(admin_id)
        .execute(&self.pool)
        .await?;
        Ok(token)
    }

    pub async fn today_lessons(&self, group: &str, date: NaiveDate) -> Result<Vec<Lesson>> {
        let rows = sqlx::query_as::<_, DbLesson>(
            "SELECT l.groups, l.lesson_date, l.weekday, l.lesson_number, l.start_time, l.end_time, \
             l.subject, l.lesson_type, l.teacher, l.room, l.description \
             FROM lessons l JOIN schedule_weeks w ON w.current_version = l.version_id \
             WHERE $1 = ANY(l.groups) AND l.lesson_date = $2 \
             ORDER BY l.start_time, l.lesson_number, l.groups",
        )
        .bind(group)
        .bind(date)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    pub async fn week_lessons(&self, group: &str, date: NaiveDate) -> Result<Vec<Lesson>> {
        let version: Option<Uuid> = sqlx::query_scalar(
            "SELECT w.current_version FROM schedule_weeks w \
             WHERE EXISTS (SELECT 1 FROM lessons l WHERE l.version_id = w.current_version AND $1 = ANY(l.groups)) \
             ORDER BY CASE WHEN $2 BETWEEN w.week_start AND w.week_end THEN 0 \
                           WHEN w.week_start > $2 THEN 1 ELSE 2 END, \
                      CASE WHEN w.week_start > $2 THEN w.week_start END ASC, \
                      CASE WHEN w.week_end < $2 THEN w.week_end END DESC \
             LIMIT 1",
        )
        .bind(group)
        .bind(date)
        .fetch_optional(&self.pool)
        .await?;
        let Some(version) = version else {
            return Ok(Vec::new());
        };
        let rows = sqlx::query_as::<_, DbLesson>(
            "SELECT groups, lesson_date, weekday, lesson_number, start_time, end_time, subject, \
             lesson_type, teacher, room, description FROM lessons \
             WHERE version_id = $1 AND $2 = ANY(groups) \
             ORDER BY lesson_date, start_time, lesson_number",
        )
        .bind(version)
        .bind(group)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    pub async fn search_lessons(
        &self,
        kind: &str,
        query: &str,
        date: NaiveDate,
    ) -> Result<Vec<Lesson>> {
        let version: Option<Uuid> = sqlx::query_scalar(
            "SELECT current_version FROM schedule_weeks \
             ORDER BY CASE WHEN $1 BETWEEN week_start AND week_end THEN 0 \
                           WHEN week_start > $1 THEN 1 ELSE 2 END, \
                      CASE WHEN week_start > $1 THEN week_start END ASC, \
                      CASE WHEN week_end < $1 THEN week_end END DESC \
             LIMIT 1",
        )
        .bind(date)
        .fetch_optional(&self.pool)
        .await?;
        let Some(version) = version else {
            return Ok(Vec::new());
        };
        let normalized = if kind == "room" {
            display_room_name(query)
        } else {
            query.to_owned()
        };
        let column = if kind == "teacher" { "teacher" } else { "room" };
        // Column is selected only from this closed allowlist; query contents remain bound.
        let sql = format!(
            "SELECT groups, lesson_date, weekday, lesson_number, start_time, end_time, subject, \
             lesson_type, teacher, room, description FROM lessons WHERE version_id = $1 \
             AND {column} ILIKE $2 ORDER BY lesson_date, start_time, lesson_number LIMIT 30"
        );
        let rows = sqlx::query_as::<_, DbLesson>(&sql)
            .bind(version)
            .bind(format!("%{normalized}%"))
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    pub async fn preview_upload(
        &self,
        admin_id: i64,
        file_name: &str,
        file_sha256: &str,
        lessons: Vec<Lesson>,
    ) -> Result<UploadPreview> {
        if !self.is_admin(admin_id).await? {
            bail!("загрузка доступна только администраторам");
        }
        let (week_start, week_end) = validate_schedule(&lessons)?;
        let (was_published, is_current_file): (bool, bool) = sqlx::query_as(
            "SELECT EXISTS (SELECT 1 FROM schedule_versions WHERE file_sha256 = $1), \
             EXISTS (SELECT 1 FROM schedule_versions v JOIN schedule_weeks w \
                     ON w.current_version = v.id \
                     WHERE v.file_sha256 = $1 AND v.week_start = $2)",
        )
        .bind(file_sha256)
        .bind(week_start)
        .fetch_one(&self.pool)
        .await?;
        if was_published && !is_current_file {
            bail!(
                "этот Excel-файл уже публиковали, но он больше не является текущей версией недели"
            );
        }

        let existing: Option<Uuid> = sqlx::query_scalar(
            "SELECT current_version FROM schedule_weeks WHERE week_start = $1 AND week_end = $2",
        )
        .bind(week_start)
        .bind(week_end)
        .fetch_optional(&self.pool)
        .await?;
        let kind = if existing.is_some() {
            UpdateKind::Correction
        } else {
            UpdateKind::NewWeek
        };
        let old_lessons = if let Some(version) = existing {
            self.lessons_by_version(version).await?
        } else {
            Vec::new()
        };
        let diff = compare_schedules(&old_lessons, &lessons);
        if kind == UpdateKind::Correction && diff.changed_groups.is_empty() {
            if is_current_file {
                bail!("эта версия Excel уже опубликована и не содержит новых изменений");
            }
            bail!("в файле нет изменений относительно опубликованной версии недели");
        }
        let parsed_lessons = serde_json::to_value(&lessons)?;
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO pending_uploads (id, uploaded_by, file_name, file_sha256, week_start, week_end, \
             update_kind, base_version, diff, parsed_lessons) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        )
        .bind(id)
        .bind(admin_id)
        .bind(file_name)
        .bind(file_sha256)
        .bind(week_start)
        .bind(week_end)
        .bind(kind.as_str())
        .bind(existing)
        .bind(Json(diff.clone()))
        .bind(Json(parsed_lessons))
        .execute(&self.pool)
        .await?;

        Ok(UploadPreview {
            id,
            week_start,
            week_end,
            kind,
            diff,
        })
    }

    async fn lessons_by_version(&self, version: Uuid) -> Result<Vec<Lesson>> {
        let rows = sqlx::query_as::<_, DbLesson>(
            "SELECT groups, lesson_date, weekday, lesson_number, start_time, end_time, subject, \
             lesson_type, teacher, room, description FROM lessons WHERE version_id = $1 \
             ORDER BY lesson_date, start_time, groups",
        )
        .bind(version)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    pub async fn cancel_upload(&self, id: Uuid, admin_id: i64) -> Result<bool> {
        let result = sqlx::query(
            "UPDATE pending_uploads SET status = 'cancelled' WHERE id = $1 AND uploaded_by = $2 AND status = 'pending'",
        )
        .bind(id)
        .bind(admin_id)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn confirm_upload(&self, id: Uuid, admin_id: i64) -> Result<Publication> {
        if !self.is_admin(admin_id).await? {
            bail!("подтверждать загрузку могут только администраторы");
        }
        let mut tx = self.pool.begin().await?;
        let row = sqlx::query(
            "SELECT week_start, week_end, update_kind, base_version, diff, parsed_lessons, file_name, file_sha256 \
             FROM pending_uploads WHERE id = $1 AND uploaded_by = $2 AND status = 'pending' FOR UPDATE",
        )
        .bind(id)
        .bind(admin_id)
        .fetch_optional(&mut *tx)
        .await?
        .context("загрузка не найдена или уже обработана")?;
        let week_start: NaiveDate = row.try_get("week_start")?;
        let week_end: NaiveDate = row.try_get("week_end")?;
        let kind = parse_update_kind(row.try_get::<String, _>("update_kind")?.as_str())?;
        let base_version: Option<Uuid> = row.try_get("base_version")?;
        let diff: Json<ScheduleDiff> = row.try_get("diff")?;
        let parsed_lessons: Json<Vec<Lesson>> = row.try_get("parsed_lessons")?;
        let current: Option<Uuid> = sqlx::query_scalar(
            "SELECT current_version FROM schedule_weeks WHERE week_start = $1 FOR UPDATE",
        )
        .bind(week_start)
        .fetch_optional(&mut *tx)
        .await?;
        if current != base_version {
            bail!("расписание изменилось после предпросмотра; загрузите файл ещё раз");
        }
        let revision: i32 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(revision), 0) + 1 FROM schedule_versions WHERE week_start = $1",
        )
        .bind(week_start)
        .fetch_one(&mut *tx)
        .await?;
        let version_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO schedule_versions (id, week_start, week_end, revision, update_kind, file_name, file_sha256, created_by) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(version_id)
        .bind(week_start)
        .bind(week_end)
        .bind(revision)
        .bind(kind.as_str())
        .bind(row.try_get::<String, _>("file_name")?)
        .bind(row.try_get::<String, _>("file_sha256")?)
        .bind(admin_id)
        .execute(&mut *tx)
        .await?;
        insert_lessons(&mut tx, version_id, &parsed_lessons.0).await?;
        sqlx::query(
            "INSERT INTO schedule_weeks (week_start, week_end, current_version) VALUES ($1, $2, $3) \
             ON CONFLICT (week_start) DO UPDATE SET week_end = EXCLUDED.week_end, \
             current_version = EXCLUDED.current_version, updated_at = now()",
        )
        .bind(week_start)
        .bind(week_end)
        .bind(version_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE pending_uploads SET status = 'confirmed', confirmed_at = now() WHERE id = $1",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?;

        let notify_groups = if kind == UpdateKind::NewWeek {
            parsed_lessons
                .0
                .iter()
                .flat_map(|lesson| lesson.groups.iter().cloned())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>()
        } else {
            diff.0.changed_groups.clone()
        };
        let recipients = sqlx::query_scalar::<_, i64>(
            "SELECT telegram_id FROM users WHERE group_code = ANY($1)",
        )
        .bind(&notify_groups)
        .fetch_all(&mut *tx)
        .await?;
        let body = notification_text(kind, week_start, week_end, &diff.0);
        for recipient in recipients {
            sqlx::query("INSERT INTO notification_outbox (telegram_id, body) VALUES ($1, $2)")
                .bind(recipient)
                .bind(&body)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(Publication {
            week_start,
            week_end,
            kind,
            diff: diff.0,
        })
    }

    pub async fn ready_notifications(&self, limit: i64) -> Result<Vec<QueuedMessage>> {
        Ok(sqlx::query_as(
            "WITH ready AS ( \
                 SELECT id FROM notification_outbox WHERE sent_at IS NULL AND available_at <= now() \
                 AND attempts < 8 AND (claimed_at IS NULL OR claimed_at < now() - interval '5 minutes') \
                 ORDER BY id LIMIT $1 FOR UPDATE SKIP LOCKED \
             ) \
             UPDATE notification_outbox o SET claimed_at = now() FROM ready r, users u \
             WHERE o.id = r.id AND u.telegram_id = o.telegram_id \
             RETURNING o.id, u.chat_id, o.body",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn mark_notification_sent(&self, id: i64) -> Result<()> {
        sqlx::query(
            "UPDATE notification_outbox SET sent_at = now(), claimed_at = NULL, attempts = attempts + 1 WHERE id = $1",
        )
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn mark_notification_failed(&self, id: i64, error: &str) -> Result<()> {
        sqlx::query(
            "UPDATE notification_outbox SET attempts = attempts + 1, claimed_at = NULL, last_error = $2, \
             available_at = now() + make_interval(secs => LEAST(3600, 30 * power(2, LEAST(attempts, 6))::int)) \
             WHERE id = $1",
        )
        .bind(id)
        .bind(error.chars().take(500).collect::<String>())
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

#[derive(sqlx::FromRow)]
struct UserRow {
    telegram_id: i64,
    chat_id: i64,
    role: String,
    group_code: Option<String>,
    pending_group: Option<String>,
    flow_state: String,
    search_kind: Option<String>,
}

impl From<UserRow> for User {
    fn from(row: UserRow) -> Self {
        Self {
            telegram_id: row.telegram_id,
            chat_id: row.chat_id,
            role: row.role,
            group_code: row.group_code,
            pending_group: row.pending_group,
            flow_state: row.flow_state,
            search_kind: row.search_kind,
        }
    }
}

fn parse_update_kind(value: &str) -> Result<UpdateKind> {
    match value {
        "new_week" => Ok(UpdateKind::NewWeek),
        "correction" => Ok(UpdateKind::Correction),
        _ => bail!("неизвестный тип обновления `{value}`"),
    }
}

fn notification_text(
    kind: UpdateKind,
    week_start: NaiveDate,
    week_end: NaiveDate,
    diff: &ScheduleDiff,
) -> String {
    match kind {
        UpdateKind::NewWeek => format!(
            "🆕 Опубликовано расписание на новую неделю: {}–{}. Нажмите «На неделю», чтобы посмотреть пары.",
            week_start.format("%d.%m"),
            week_end.format("%d.%m.%Y")
        ),
        UpdateKind::Correction => {
            let dates = diff
                .changed_dates
                .iter()
                .map(|date| date.format("%d.%m").to_string())
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "🔄 Обновлено расписание на неделю {}–{}. Изменены дни: {}. Добавлено пар: {}, удалено пар: {}.",
                week_start.format("%d.%m"),
                week_end.format("%d.%m.%Y"),
                dates,
                diff.added,
                diff.removed
            )
        }
    }
}

async fn insert_lessons(
    tx: &mut Transaction<'_, Postgres>,
    version_id: Uuid,
    lessons: &[Lesson],
) -> Result<()> {
    for lesson in lessons {
        sqlx::query(
            "INSERT INTO lessons (version_id, groups, lesson_date, weekday, lesson_number, start_time, end_time, \
             subject, lesson_type, teacher, room, description) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
        )
        .bind(version_id)
        .bind(&lesson.groups)
        .bind(lesson.date)
        .bind(&lesson.weekday)
        .bind(lesson.lesson_number as i16)
        .bind(&lesson.start_time)
        .bind(&lesson.end_time)
        .bind(&lesson.subject)
        .bind(&lesson.lesson_type)
        .bind(&lesson.teacher)
        .bind(&lesson.room)
        .bind(&lesson.description)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

fn sha256_hex(value: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(value))
}
