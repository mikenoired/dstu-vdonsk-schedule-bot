use chrono::{Datelike, NaiveDate, Weekday};
use schedule_parser::Lesson;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DailyKind {
    Today,
    Tomorrow,
}

impl DailyKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Today => "today",
            Self::Tomorrow => "tomorrow",
        }
    }
}

pub fn format_schedule(title: &str, lessons: &[Lesson], show_dates: bool) -> String {
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
    lines.join("\n")
}

pub fn format_daily_delivery(
    group: &str,
    date: NaiveDate,
    kind: DailyKind,
    lessons: &[Lesson],
) -> String {
    let heading = match kind {
        DailyKind::Today => format!(
            "☀️ Доброе утро! Расписание на сегодня · {} · {}",
            weekday_ru(date.weekday()),
            date.format("%d.%m.%Y")
        ),
        DailyKind::Tomorrow => format!(
            "🌙 План на завтра · {} · {}",
            weekday_ru(date.weekday()),
            date.format("%d.%m.%Y")
        ),
    };
    let title = format!("{heading}\n📚 Группа {group}");
    if lessons.is_empty() {
        format!("{title}\n\n📭 По опубликованному расписанию занятий нет.")
    } else {
        format_schedule(&title, lessons, false)
    }
}

fn weekday_ru(day: Weekday) -> &'static str {
    match day {
        Weekday::Mon => "понедельник",
        Weekday::Tue => "вторник",
        Weekday::Wed => "среда",
        Weekday::Thu => "четверг",
        Weekday::Fri => "пятница",
        Weekday::Sat => "суббота",
        Weekday::Sun => "воскресенье",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gives_tomorrow_delivery_a_clear_heading() {
        let message = format_daily_delivery(
            "ИС31В",
            NaiveDate::from_ymd_opt(2026, 9, 26).unwrap(),
            DailyKind::Tomorrow,
            &[],
        );
        assert!(message.starts_with("🌙 План на завтра · суббота · 26.09.2026"));
        assert!(message.contains("📚 Группа ИС31В"));
        assert!(message.contains("📭 По опубликованному расписанию занятий нет."));
    }

    #[test]
    fn gives_today_delivery_a_clear_heading() {
        let message = format_daily_delivery(
            "ИС31В",
            NaiveDate::from_ymd_opt(2026, 9, 25).unwrap(),
            DailyKind::Today,
            &[],
        );
        assert!(
            message.starts_with("☀️ Доброе утро! Расписание на сегодня · пятница · 25.09.2026")
        );
    }
}
