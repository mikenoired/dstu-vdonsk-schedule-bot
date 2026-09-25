use chrono::NaiveDate;
use schedule_parser::{for_date, for_group, for_room, parse_file};
use std::path::PathBuf;

fn sample_file() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tables/5_ОФО_ОЗФО_Расписание 5 учебной недели осеннего семестра (28.09.2026-04.10.2026).xls")
}

#[test]
fn reads_groups_and_keeps_consecutive_pairs_separate() {
    let lessons = parse_file(sample_file()).expect("the supplied workbook should parse");
    assert!(!lessons.is_empty());

    let group_lessons = for_group(&lessons, "ИС11В");
    assert!(!group_lessons.is_empty());
    let first_period = group_lessons
        .iter()
        .find(|lesson| {
            lesson.date == NaiveDate::from_ymd_opt(2026, 9, 28).unwrap()
                && lesson.subject == "Основы инклюзивной культуры и дефектологических знаний"
                && lesson.lesson_number == 5
        })
        .expect("first Monday pair should be present");
    assert_eq!(first_period.start_time, "15:40");
    assert_eq!(first_period.end_time, "17:15");
    assert_eq!(first_period.room.as_deref(), Some("303/2"));
    assert_eq!(first_period.lesson_type.as_deref(), Some("пр.+пр."));
    assert!(
        first_period
            .teacher
            .as_deref()
            .unwrap()
            .contains("Усова И.В.")
    );
    let second_period = group_lessons
        .iter()
        .find(|lesson| {
            lesson.date == NaiveDate::from_ymd_opt(2026, 9, 28).unwrap()
                && lesson.subject == first_period.subject
                && lesson.lesson_number == 6
        })
        .expect("second Monday pair should not be merged with the first");
    assert_eq!(second_period.start_time, "17:25");
    assert_eq!(second_period.end_time, "19:00");

    let same_subject_periods: Vec<_> = for_group(&lessons, "ИС31В")
        .into_iter()
        .filter(|lesson| {
            lesson.date == NaiveDate::from_ymd_opt(2026, 10, 1).unwrap()
                && lesson.subject == "Исследование операций"
        })
        .collect();
    assert_eq!(same_subject_periods.len(), 2);
    assert_eq!(same_subject_periods[0].lesson_number, 2);
    assert_eq!(same_subject_periods[0].end_time, "11:40");
    assert_eq!(same_subject_periods[1].lesson_number, 3);
    assert_eq!(same_subject_periods[1].start_time, "12:10");
    assert_eq!(same_subject_periods[1].end_time, "13:45");
}

#[test]
fn supports_room_and_day_queries() {
    let lessons = parse_file(sample_file()).expect("the supplied workbook should parse");
    assert!(
        for_room(&lessons, "303/2")
            .iter()
            .any(|lesson| lesson.subject.contains("Иностранный язык"))
    );
    assert!(
        for_date(&lessons, NaiveDate::from_ymd_opt(2026, 9, 29).unwrap())
            .iter()
            .any(|lesson| lesson.subject.contains("Физическая культура"))
    );
}

#[test]
fn preserves_joint_group_headers_as_individual_groups() {
    let lessons = parse_file(sample_file()).expect("the supplied workbook should parse");
    assert!(
        lessons
            .iter()
            .any(|lesson| lesson.groups.iter().any(|group| group == "ОЗЭ21В"))
    );
}
