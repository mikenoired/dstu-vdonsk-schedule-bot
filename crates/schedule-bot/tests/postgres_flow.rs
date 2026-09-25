use chrono::NaiveDate;
use schedule_bot::{domain::UpdateKind, store::Store};
use schedule_parser::Lesson;
use sqlx::PgPool;

#[sqlx::test]
#[ignore = "requires a PostgreSQL superuser in DATABASE_URL; run explicitly to exercise transactions"]
async fn registration_admin_group_publish_correction_and_outbox(
    pool: PgPool,
) -> anyhow::Result<()> {
    let store = Store::from_pool(pool.clone());
    store.register_user(101, 101, Some("admin")).await?;
    store.register_user(202, 202, Some("student")).await?;
    store.register_user(303, 303, Some("next-admin")).await?;

    store.set_chat_group(-1001, "ИС11В", 101).await?;
    assert_eq!(store.chat_group(-1001).await?.as_deref(), Some("ИС11В"));
    store.set_chat_group(-1001, "КТО11В", 303).await?;
    assert_eq!(store.chat_group(-1001).await?.as_deref(), Some("КТО11В"));

    assert!(store.claim_bootstrap_admin(101).await?);
    assert!(!store.claim_bootstrap_admin(303).await?);
    assert!(store.is_admin(101).await?);
    assert!(!store.is_admin(303).await?);

    let invite = store.create_invite(101).await?;
    assert!(store.redeem_invite(303, &invite).await?);
    assert!(!store.redeem_invite(202, &invite).await?);
    assert!(store.is_admin(303).await?);

    let shared_lesson = lesson("Алгебра", &["ИС11В", "КТО11В"]);
    let preview = store
        .preview_upload(
            101,
            "week.xls",
            &"a".repeat(64),
            vec![shared_lesson.clone()],
        )
        .await?;
    assert_eq!(preview.kind, UpdateKind::NewWeek);
    assert_eq!(preview.diff.changed_groups, ["ИС11В", "КТО11В"]);
    store.confirm_upload(preview.id, 101).await?;
    assert_eq!(store.known_groups().await?, ["ИС11В", "КТО11В"]);

    store.set_pending_group(202, "ИС11В").await?;
    assert_eq!(store.confirm_group(202).await?.as_deref(), Some("ИС11В"));

    let mut second_pair = shared_lesson.clone();
    second_pair.lesson_number = 2;
    second_pair.start_time = "10:05".into();
    second_pair.end_time = "11:40".into();
    let reparsed_lessons = vec![shared_lesson.clone(), second_pair];
    let reparsed = store
        .preview_upload(101, "week.xls", &"a".repeat(64), reparsed_lessons.clone())
        .await?;
    assert_eq!(reparsed.kind, UpdateKind::Correction);
    assert_eq!(reparsed.diff.added, 1);
    store.confirm_upload(reparsed.id, 101).await?;
    let reparse_notice = store.ready_notifications(10).await?;
    assert_eq!(reparse_notice.len(), 1);
    store.mark_notification_sent(reparse_notice[0].id).await?;
    assert!(
        store
            .preview_upload(101, "week.xls", &"a".repeat(64), reparsed_lessons)
            .await
            .is_err()
    );

    let week = store
        .week_lessons("ИС11В", NaiveDate::from_ymd_opt(2026, 9, 29).unwrap())
        .await?;
    assert_eq!(week.len(), 2);
    assert_eq!(week[0].subject, "Алгебра");
    let corrected = lesson("Геометрия", &["ИС11В", "КТО11В"]);
    let correction = store
        .preview_upload(101, "week-fixed.xls", &"b".repeat(64), vec![corrected])
        .await?;
    assert_eq!(correction.kind, UpdateKind::Correction);
    assert_eq!(
        correction.diff.changed_dates,
        [NaiveDate::from_ymd_opt(2026, 9, 28).unwrap()]
    );
    store.confirm_upload(correction.id, 101).await?;
    assert_eq!(
        store
            .week_lessons("ИС11В", NaiveDate::from_ymd_opt(2026, 9, 29).unwrap())
            .await?[0]
            .subject,
        "Геометрия"
    );
    let correction_notice = store.ready_notifications(10).await?;
    assert_eq!(correction_notice.len(), 1);
    assert!(correction_notice[0].body.contains("Изменены дни: 28.09"));
    store
        .mark_notification_sent(correction_notice[0].id)
        .await?;

    let next_week = lesson_on((2026, 10, 5), "Алгебра", &["ИС11В"]);
    let next_week_preview = store
        .preview_upload(101, "next-week.xls", &"c".repeat(64), vec![next_week])
        .await?;
    assert_eq!(next_week_preview.kind, UpdateKind::NewWeek);
    store.confirm_upload(next_week_preview.id, 101).await?;
    let new_week_notice = store.ready_notifications(10).await?;
    assert_eq!(new_week_notice.len(), 1);
    assert!(new_week_notice[0].body.contains("новую неделю"));
    Ok(())
}

fn lesson(subject: &str, groups: &[&str]) -> Lesson {
    lesson_on((2026, 9, 28), subject, groups)
}

fn lesson_on(date: (i32, u32, u32), subject: &str, groups: &[&str]) -> Lesson {
    Lesson {
        groups: groups.iter().map(|group| (*group).to_owned()).collect(),
        date: NaiveDate::from_ymd_opt(date.0, date.1, date.2).unwrap(),
        weekday: "Понедельник".into(),
        lesson_number: 1,
        start_time: "08:20".into(),
        end_time: "09:55".into(),
        subject: subject.into(),
        lesson_type: Some("лек.".into()),
        teacher: Some("Преподаватель".into()),
        room: Some("101".into()),
        description: subject.into(),
    }
}
