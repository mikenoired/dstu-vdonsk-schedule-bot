use calamine::{Data, Reader, open_workbook_auto};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Lesson {
    /// Groups that share this lesson (for example, `ОЗМ21В/ОЗЭ21В`).
    pub groups: Vec<String>,
    pub date: NaiveDate,
    pub weekday: String,
    pub lesson_number: u8,
    pub start_time: String,
    pub end_time: String,
    pub subject: String,
    pub lesson_type: Option<String>,
    pub teacher: Option<String>,
    pub room: Option<String>,
    /// Original cell contents, retained in case the source contains extra details.
    pub description: String,
}

#[derive(Debug, Error)]
pub enum ParseError {
    #[error("не удалось прочитать расписание: {0}")]
    Workbook(#[from] calamine::Error),
    #[error("в файле не найдено листов с таблицами расписания")]
    NoScheduleSheets,
}

/// Reads a weekly timetable from an Excel workbook (`.xls`, `.xlsx`, or `.xlsb`).
pub fn parse_file(path: impl AsRef<Path>) -> Result<Vec<Lesson>, ParseError> {
    let mut workbook = open_workbook_auto(path)?;
    let names = workbook.sheet_names().to_owned();
    let mut lessons = Vec::new();
    let mut found_schedule = false;

    for name in names {
        let range = workbook.worksheet_range(&name)?;
        let rows: Vec<Vec<String>> = range
            .rows()
            .map(|row| row.iter().map(cell_text).collect())
            .collect();
        let Some(header_row) = rows.iter().position(|row| row.iter().any(|v| v == "День"))
        else {
            continue;
        };

        let Some(group_row) = rows.get(header_row) else {
            continue;
        };
        let mut groups = Vec::new();
        let mut col = 4;
        while col < group_row.len() {
            let group_names = split_groups(group_row.get(col).map(String::as_str).unwrap_or(""));
            if !group_names.is_empty() {
                groups.push((col, group_names));
            }
            col += 2;
        }
        if groups.is_empty() {
            continue;
        }
        found_schedule = true;

        let mut day_name = String::new();
        let mut date = None;
        for row_index in (header_row + 2)..rows.len() {
            let row = &rows[row_index];
            if let Some(day) = row
                .first()
                .filter(|v| !v.is_empty())
                .filter(|v| is_weekday(v))
            {
                day_name = day.clone();
                date = row.get(1).and_then(|v| parse_date(v));
            } else if let Some(date_text) = row.get(1).filter(|v| !v.is_empty()) {
                date = parse_date(date_text).or(date);
            }
            let (Some(date), Some(time)) = (date, row.get(3).and_then(|v| parse_time(v))) else {
                continue;
            };
            let lesson_number = row.get(2).and_then(|v| v.parse::<u8>().ok()).unwrap_or(0);
            if lesson_number == 0 {
                continue;
            }

            for (subject_col, group_names) in &groups {
                let description = row.get(*subject_col).cloned().unwrap_or_default();
                if description.trim().is_empty() {
                    continue;
                }
                let room = row
                    .get(subject_col + 1)
                    .and_then(|v| non_empty(v))
                    .map(|room| display_room_name(&room));
                let (subject, lesson_type, teacher) = parse_description(&description);

                // Room cells repeat down merged multi-period lessons. The subject is
                // only present in the first row, so extend that entry through them.
                let mut end_time = time.1.clone();
                let mut next_index = row_index + 1;
                while next_index < rows.len() {
                    let next = &rows[next_index];
                    if next.first().is_some_and(|v| is_weekday(v))
                        || next.get(1).is_some_and(|v| parse_date(v).is_some())
                    {
                        break;
                    }
                    let next_room = next
                        .get(subject_col + 1)
                        .and_then(|v| non_empty(v))
                        .map(|room| display_room_name(&room));
                    let next_subject = next.get(*subject_col).is_some_and(|v| !v.trim().is_empty());
                    if next_subject
                        || room.as_deref() != next_room.as_deref()
                        || next_room.is_none()
                    {
                        break;
                    }
                    if let Some((_, next_end)) = next.get(3).and_then(|v| parse_time(v)) {
                        end_time = next_end;
                    }
                    next_index += 1;
                }

                lessons.push(Lesson {
                    groups: group_names.clone(),
                    date,
                    weekday: day_name.clone(),
                    lesson_number,
                    start_time: time.0.clone(),
                    end_time,
                    subject,
                    lesson_type,
                    teacher,
                    room,
                    description,
                });
            }
        }
    }

    if !found_schedule {
        return Err(ParseError::NoScheduleSheets);
    }
    lessons.sort_by(|a, b| {
        (a.date, &a.start_time, &a.groups, &a.subject).cmp(&(
            b.date,
            &b.start_time,
            &b.groups,
            &b.subject,
        ))
    });
    Ok(lessons)
}

pub fn for_group<'a>(lessons: &'a [Lesson], group: &str) -> Vec<&'a Lesson> {
    lessons
        .iter()
        .filter(|lesson| lesson.groups.iter().any(|g| g.eq_ignore_ascii_case(group)))
        .collect()
}

pub fn for_room<'a>(lessons: &'a [Lesson], room: &str) -> Vec<&'a Lesson> {
    let room = display_room_name(room);
    lessons
        .iter()
        .filter(|lesson| {
            lesson
                .room
                .as_deref()
                .is_some_and(|r| r.eq_ignore_ascii_case(&room))
        })
        .collect()
}

/// Gives the remote teaching mode a readable label in normalized data and output.
pub fn display_room_name(room: &str) -> String {
    if room.trim().to_lowercase() == "дот" {
        "Удалённо".to_owned()
    } else {
        room.trim().to_owned()
    }
}

pub fn for_date<'a>(lessons: &'a [Lesson], date: NaiveDate) -> Vec<&'a Lesson> {
    lessons
        .iter()
        .filter(|lesson| lesson.date == date)
        .collect()
}

fn cell_text(cell: &Data) -> String {
    match cell {
        Data::Empty => String::new(),
        Data::String(value) => value.trim().to_owned(),
        _ => cell.to_string().trim().to_owned(),
    }
}

fn split_groups(value: &str) -> Vec<String> {
    value
        .split('/')
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn is_weekday(value: &str) -> bool {
    matches!(
        value,
        "Понедельник"
            | "Вторник"
            | "Среда"
            | "Четверг"
            | "Пятница"
            | "Суббота"
            | "Воскресенье"
    )
}

fn parse_date(value: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(value.trim(), "%d.%m.%Y")
        .ok()
        .or_else(|| NaiveDate::parse_from_str(value.trim(), "%Y-%m-%d").ok())
}

fn parse_time(value: &str) -> Option<(String, String)> {
    let (start, end) = value.trim().split_once('-')?;
    let normalize = |time: &str| {
        let (hour, minute) = time.trim().split_once('.')?;
        Some(format!("{hour:0>2}:{minute:0>2}"))
    };
    Some((normalize(start)?, normalize(end)?))
}

fn non_empty(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn parse_description(description: &str) -> (String, Option<String>, Option<String>) {
    let type_start = description.match_indices('(').find_map(|(start, _)| {
        let tail = &description[start..];
        let end = tail.find(')')?;
        let inside = &tail[1..end];
        let lower = inside.to_lowercase();
        ["лек", "пр", "лаб", "сем", "конс", "экз", "зач"]
            .iter()
            .any(|word| lower.contains(word))
            .then_some((start, start + end + 1, inside.trim().to_owned()))
    });
    let Some((start, end, kind)) = type_start else {
        return (description.trim().to_owned(), None, None);
    };
    let subject = description[..start]
        .split('(')
        .next()
        .unwrap_or(&description[..start])
        .trim()
        .to_owned();
    let teacher = non_empty(&description[end..]);
    (subject, Some(kind), teacher)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_lesson_description() {
        let (subject, kind, teacher) = parse_description(
            "Основы инклюзивной культуры (ИС11В, КТО11В) (пр.+пр.) доц. Усова И.В.",
        );
        assert_eq!(subject, "Основы инклюзивной культуры");
        assert_eq!(kind.as_deref(), Some("пр.+пр."));
        assert_eq!(teacher.as_deref(), Some("доц. Усова И.В."));
    }

    #[test]
    fn normalizes_time() {
        assert_eq!(
            parse_time("08.20-09.55 "),
            Some(("08:20".into(), "09:55".into()))
        );
    }

    #[test]
    fn recognizes_joint_group_headers() {
        assert_eq!(split_groups("ОЗМ21В/ОЗЭ21В"), vec!["ОЗМ21В", "ОЗЭ21В"]);
    }

    #[test]
    fn labels_dot_as_remote() {
        assert_eq!(display_room_name("ДОТ"), "Удалённо");
        assert_eq!(display_room_name(" дот "), "Удалённо");
        assert_eq!(display_room_name("303/2"), "303/2");
    }

    #[test]
    fn remote_room_can_be_queried_by_either_label() {
        let lesson = Lesson {
            groups: vec!["ИС11В".into()],
            date: NaiveDate::from_ymd_opt(2026, 9, 28).unwrap(),
            weekday: "Понедельник".into(),
            lesson_number: 1,
            start_time: "08:20".into(),
            end_time: "09:55".into(),
            subject: "Тест".into(),
            lesson_type: None,
            teacher: None,
            room: Some(display_room_name("ДОТ")),
            description: "Тест".into(),
        };
        assert_eq!(for_room(std::slice::from_ref(&lesson), "ДОТ").len(), 1);
        assert_eq!(for_room(std::slice::from_ref(&lesson), "Удалённо").len(), 1);
    }
}
