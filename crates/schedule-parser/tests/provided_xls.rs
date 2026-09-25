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

#[test]
fn parses_the_four_attached_consecutive_weekly_workbooks() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tables");
    let workbooks = [
        (
            "2_",
            NaiveDate::from_ymd_opt(2026, 9, 7).unwrap(),
            NaiveDate::from_ymd_opt(2026, 9, 13).unwrap(),
            178,
            25,
        ),
        (
            "3_",
            NaiveDate::from_ymd_opt(2026, 9, 14).unwrap(),
            NaiveDate::from_ymd_opt(2026, 9, 20).unwrap(),
            161,
            22,
        ),
        (
            "4_",
            NaiveDate::from_ymd_opt(2026, 9, 21).unwrap(),
            NaiveDate::from_ymd_opt(2026, 9, 27).unwrap(),
            179,
            25,
        ),
        (
            "5_",
            NaiveDate::from_ymd_opt(2026, 9, 28).unwrap(),
            NaiveDate::from_ymd_opt(2026, 10, 4).unwrap(),
            186,
            25,
        ),
    ];

    for (prefix, start, end, expected_lessons, expected_groups) in workbooks {
        let path = std::fs::read_dir(&root)
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .find(|path| {
                path.file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with(prefix))
            })
            .unwrap_or_else(|| panic!("не найдена таблица недели с префиксом {prefix}"));
        let file = path.file_name().unwrap().to_string_lossy();
        let lessons = parse_file(&path).unwrap_or_else(|error| panic!("{file}: {error}"));
        assert!(!lessons.is_empty(), "{file}: расписание пустое");
        assert!(
            lessons
                .iter()
                .all(|lesson| (start..=end).contains(&lesson.date)),
            "{file}: есть занятие вне заявленной недели"
        );
        assert_eq!(
            lessons.len(),
            expected_lessons,
            "{file}: количество занятий изменилось"
        );
        assert!(
            lessons
                .iter()
                .all(|lesson| !lesson.groups.is_empty() && !lesson.subject.is_empty()),
            "{file}: найдена пара без группы или дисциплины"
        );
        let groups = lessons
            .iter()
            .flat_map(|lesson| lesson.groups.iter())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            groups.len(),
            expected_groups,
            "{file}: количество групп изменилось"
        );
        let dates = lessons
            .iter()
            .map(|lesson| lesson.date)
            .collect::<std::collections::BTreeSet<_>>();
        let weekdays = lessons
            .iter()
            .map(|lesson| lesson.weekday.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(dates.len(), 6, "{file}: ожидалось 6 дней с парами");
        assert_eq!(weekdays.len(), 6, "{file}: не распознан день недели");
        assert!(
            lessons
                .iter()
                .filter(|lesson| lesson
                    .description
                    .to_lowercase()
                    .starts_with("кураторский час, ст. преп."))
                .all(|lesson| {
                    lesson.subject == "Кураторский час"
                        && lesson
                            .teacher
                            .as_deref()
                            .is_some_and(|teacher| teacher.starts_with("ст. преп."))
                }),
            "{file}: некорректно разобран преподаватель кураторского часа"
        );
    }
}
