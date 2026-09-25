use chrono::NaiveDate;
use schedule_bot::domain::validate_schedule;
use schedule_parser::parse_file;
use std::path::PathBuf;

#[test]
fn parser_output_passes_bot_publication_validation() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(
        "../../tables/5_ОФО_ОЗФО_Расписание 5 учебной недели осеннего семестра (28.09.2026-04.10.2026).xls",
    );
    let lessons = parse_file(path).unwrap();
    let (start, end) = validate_schedule(&lessons).unwrap();
    assert_eq!(start, NaiveDate::from_ymd_opt(2026, 9, 28).unwrap());
    assert_eq!(end, NaiveDate::from_ymd_opt(2026, 10, 4).unwrap());
}
