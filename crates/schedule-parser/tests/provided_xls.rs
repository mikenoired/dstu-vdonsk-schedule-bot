use chrono::NaiveDate;
use schedule_parser::{for_date, for_group, for_room, parse_file};
use std::path::PathBuf;

fn sample_file() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tables/5_ОФО_ОЗФО_Расписание 5 учебной недели осеннего семестра (28.09.2026-04.10.2026).xls")
}

#[test]
fn reads_groups_and_expands_contiguous_lesson_slots() {
    let lessons = parse_file(sample_file()).expect("the supplied workbook should parse");
    assert!(!lessons.is_empty());

    let group_lessons = for_group(&lessons, "ИС11В");
    assert!(!group_lessons.is_empty());
    let shared_room_lesson = group_lessons
        .iter()
        .find(|lesson| {
            lesson.date == NaiveDate::from_ymd_opt(2026, 9, 28).unwrap()
                && lesson.subject == "Основы инклюзивной культуры и дефектологических знаний"
        })
        .expect("Monday lesson should be present");
    assert_eq!(shared_room_lesson.start_time, "15:40");
    assert_eq!(shared_room_lesson.end_time, "19:00");
    assert_eq!(shared_room_lesson.room.as_deref(), Some("303/2"));
    assert_eq!(shared_room_lesson.lesson_type.as_deref(), Some("пр.+пр."));
    assert!(
        shared_room_lesson
            .teacher
            .as_deref()
            .unwrap()
            .contains("Усова И.В.")
    );
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
