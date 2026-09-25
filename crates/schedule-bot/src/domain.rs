use anyhow::{Context, Result, bail};
use chrono::{Datelike, Duration, NaiveDate};
use schedule_parser::Lesson;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UpdateKind {
    NewWeek,
    Correction,
}

impl UpdateKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NewWeek => "new_week",
            Self::Correction => "correction",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScheduleDiff {
    pub changed_groups: Vec<String>,
    pub changed_dates: Vec<NaiveDate>,
    pub added: usize,
    pub removed: usize,
}

pub fn validate_schedule(lessons: &[Lesson]) -> Result<(NaiveDate, NaiveDate)> {
    if lessons.is_empty() {
        bail!("в файле не найдено ни одной пары");
    }
    if lessons.iter().any(|lesson| {
        lesson.groups.is_empty()
            || lesson.groups.iter().any(|group| group.trim().is_empty())
            || lesson.subject.trim().is_empty()
            || lesson.lesson_number == 0
    }) {
        bail!("в расписании есть пара без группы, дисциплины или номера");
    }

    let first_date = lessons.iter().map(|lesson| lesson.date).min().unwrap();
    let last_date = lessons.iter().map(|lesson| lesson.date).max().unwrap();
    let week_start = first_date
        .checked_sub_signed(Duration::days(
            first_date.weekday().num_days_from_monday() as i64
        ))
        .context("не удалось вычислить начало недели")?;
    let week_end = week_start
        .checked_add_signed(Duration::days(6))
        .context("не удалось вычислить конец недели")?;
    if last_date > week_end {
        bail!("таблица содержит пары из нескольких календарных недель");
    }
    Ok((week_start, week_end))
}

pub fn compare_schedules(old: &[Lesson], new: &[Lesson]) -> ScheduleDiff {
    type Key = (NaiveDate, String);
    let mut old_by_group_day: BTreeMap<Key, Vec<&Lesson>> = BTreeMap::new();
    let mut new_by_group_day: BTreeMap<Key, Vec<&Lesson>> = BTreeMap::new();

    for lesson in old {
        for group in &lesson.groups {
            old_by_group_day
                .entry((lesson.date, group.clone()))
                .or_default()
                .push(lesson);
        }
    }
    for lesson in new {
        for group in &lesson.groups {
            new_by_group_day
                .entry((lesson.date, group.clone()))
                .or_default()
                .push(lesson);
        }
    }

    let keys: BTreeSet<_> = old_by_group_day
        .keys()
        .chain(new_by_group_day.keys())
        .cloned()
        .collect();
    let mut changed_groups = BTreeSet::new();
    let mut changed_dates = BTreeSet::new();
    let mut added = 0;
    let mut removed = 0;

    for (date, group) in keys {
        let old_entries = old_by_group_day
            .get(&(date, group.clone()))
            .cloned()
            .unwrap_or_default();
        let new_entries = new_by_group_day
            .get(&(date, group.clone()))
            .cloned()
            .unwrap_or_default();
        if same_entries(&old_entries, &new_entries) {
            continue;
        }
        changed_groups.insert(group);
        changed_dates.insert(date);
        let old_keys = entry_keys(&old_entries);
        let new_keys = entry_keys(&new_entries);
        added += new_keys.difference(&old_keys).count();
        removed += old_keys.difference(&new_keys).count();
    }

    ScheduleDiff {
        changed_groups: changed_groups.into_iter().collect(),
        changed_dates: changed_dates.into_iter().collect(),
        added,
        removed,
    }
}

fn same_entries(left: &[&Lesson], right: &[&Lesson]) -> bool {
    entry_keys(left) == entry_keys(right)
}

fn entry_keys(lessons: &[&Lesson]) -> BTreeSet<String> {
    lessons
        .iter()
        .filter_map(|lesson| serde_json::to_string(lesson).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lesson(date: (i32, u32, u32), subject: &str, groups: &[&str]) -> Lesson {
        Lesson {
            groups: groups.iter().map(|group| (*group).to_owned()).collect(),
            date: NaiveDate::from_ymd_opt(date.0, date.1, date.2).unwrap(),
            weekday: "Понедельник".to_owned(),
            lesson_number: 1,
            start_time: "08:20".to_owned(),
            end_time: "09:55".to_owned(),
            subject: subject.to_owned(),
            lesson_type: Some("лек.".to_owned()),
            teacher: Some("Преподаватель".to_owned()),
            room: Some("101".to_owned()),
            description: subject.to_owned(),
        }
    }

    #[test]
    fn identifies_monday_to_sunday_schedule_range() {
        let (start, end) = validate_schedule(&[
            lesson((2026, 9, 29), "Алгебра", &["ИС11В"]),
            lesson((2026, 10, 3), "Физика", &["ИС11В"]),
        ])
        .unwrap();
        assert_eq!(start, NaiveDate::from_ymd_opt(2026, 9, 28).unwrap());
        assert_eq!(end, NaiveDate::from_ymd_opt(2026, 10, 4).unwrap());
    }

    #[test]
    fn rejects_data_spanning_two_calendar_weeks() {
        let result = validate_schedule(&[
            lesson((2026, 9, 28), "Алгебра", &["ИС11В"]),
            lesson((2026, 10, 5), "Физика", &["ИС11В"]),
        ]);
        assert!(result.is_err());
    }

    #[test]
    fn finds_changed_groups_and_days() {
        let old = [lesson((2026, 9, 28), "Алгебра", &["ИС11В", "КТО11В"])];
        let new = [lesson((2026, 9, 28), "Геометрия", &["ИС11В", "КТО11В"])];
        let diff = compare_schedules(&old, &new);
        assert_eq!(diff.changed_groups, ["ИС11В", "КТО11В"]);
        assert_eq!(
            diff.changed_dates,
            [NaiveDate::from_ymd_opt(2026, 9, 28).unwrap()]
        );
        assert_eq!((diff.added, diff.removed), (2, 2));
    }
}
