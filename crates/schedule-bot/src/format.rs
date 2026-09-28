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
    format_schedule_inner(title, lessons, show_dates, None, true)
}

pub fn format_teacher_schedule(title: &str, lessons: &[Lesson], show_dates: bool) -> String {
    format_schedule_inner(title, lessons, show_dates, None, false)
}

pub fn format_week_schedule(
    title: &str,
    lessons: &[Lesson],
    week_start: NaiveDate,
    week_end: NaiveDate,
) -> String {
    format_schedule_inner(title, lessons, true, Some((week_start, week_end)), true)
}

pub fn format_teacher_week_schedule(
    title: &str,
    lessons: &[Lesson],
    week_start: NaiveDate,
    week_end: NaiveDate,
) -> String {
    format_schedule_inner(title, lessons, true, Some((week_start, week_end)), false)
}

fn format_schedule_inner(
    title: &str,
    lessons: &[Lesson],
    show_dates: bool,
    date_range: Option<(NaiveDate, NaiveDate)>,
    show_teacher: bool,
) -> String {
    let mut lines = vec![title.to_owned(), "━━━━━━━━━━━━━━━━".to_owned()];
    let mut current_date: Option<NaiveDate> = None;
    let visible: Vec<_> = lessons
        .iter()
        .filter(|lesson| !is_self_study(lesson))
        .collect();
    let dates: Vec<_> = if let Some((start, end)) = date_range {
        std::iter::successors(Some(start), |date| date.succ_opt())
            .take_while(|date| *date <= end)
            .collect()
    } else {
        visible.iter().map(|lesson| lesson.date).collect()
    };
    let mut day_index = 0;
    for lesson in &visible {
        if show_dates {
            while dates.get(day_index).is_some_and(|date| *date < lesson.date) {
                append_day_off(&mut lines, &dates[day_index]);
                day_index += 1;
            }
        }
        if show_dates && current_date != Some(lesson.date) {
            lines.push(String::new());
            lines.push(format!(
                "🗓️ {} · {}",
                lesson.weekday,
                lesson.date.format("%d.%m.%Y")
            ));
            current_date = Some(lesson.date);
            day_index = dates
                .iter()
                .position(|date| *date == lesson.date)
                .map_or(day_index, |i| i + 1);
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
        if show_teacher {
            if let Some(teacher) = &lesson.teacher {
                details.push(format!("👩‍🏫 {teacher}"));
            }
        }
        if let Some(room) = &lesson.room {
            details.push(format!("📍 {room}"));
        }
        if !details.is_empty() {
            lines.push(details.join(" · "));
        }
        lines.push(String::new());
    }
    if show_dates {
        while let Some(date) = dates.get(day_index) {
            append_day_off(&mut lines, date);
            day_index += 1;
        }
    }
    if visible.is_empty() {
        lines.push("📭 Пар нет".to_owned());
    } else {
        lines.push(format!("✨ Всего пар: {}", visible.len()));
    }
    lines.join("\n")
}

fn append_day_off(lines: &mut Vec<String>, date: &NaiveDate) {
    lines.push(String::new());
    lines.push(format!(
        "🗓️ {} · {}",
        weekday_ru(date.weekday()),
        date.format("%d.%m.%Y")
    ));
    lines.push("🌴 Выходной · пар нет".to_owned());
}

fn is_self_study(lesson: &Lesson) -> bool {
    let text = format!("{} {}", lesson.subject, lesson.description).to_lowercase();
    text.contains("самостоятель") && (text.contains("работ") || text.contains("занят"))
}

pub fn format_daily_delivery(
    group: &str,
    date: NaiveDate,
    kind: DailyKind,
    lessons: &[Lesson],
) -> String {
    format_daily_delivery_for_identity("Группа", group, date, kind, lessons, true)
}

pub fn format_teacher_daily_delivery(
    teacher: &str,
    date: NaiveDate,
    kind: DailyKind,
    lessons: &[Lesson],
) -> String {
    format_daily_delivery_for_identity("Преподаватель", teacher, date, kind, lessons, false)
}

fn format_daily_delivery_for_identity(
    identity_label: &str,
    identity: &str,
    date: NaiveDate,
    kind: DailyKind,
    lessons: &[Lesson],
    show_teacher: bool,
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
    let title = format!("{heading}\n📚 {identity_label} {identity}");
    if lessons.is_empty() {
        format!("{title}\n\n📭 По опубликованному расписанию занятий нет.")
    } else {
        format_schedule_inner(&title, lessons, false, None, show_teacher)
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
