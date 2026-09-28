use anyhow::{Context, Result, bail};
use chrono::{DateTime, Datelike, Duration, NaiveDate, Timelike, Utc, Weekday};
use chrono_tz::Tz;
use reqwest::{
    Client,
    header::{self, HeaderMap, HeaderValue},
};
use scraper::{ElementRef, Html, Selector};
use std::time::Duration as StdDuration;

const SCHEDULE_PAGE: &str = "https://itf.donstu.ru/raspisanie/Index/";
const USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36";
const MAX_PAGE_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebsiteWeek {
    pub number: u32,
    pub term_key: String,
    pub file_name: String,
    pub file_url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WeekChange {
    InitialBaseline,
    NewSemesterBaseline,
    Unchanged,
    Advanced,
}

pub fn classify_week_change(previous: Option<(&str, u32)>, current: &WebsiteWeek) -> WeekChange {
    let Some((previous_term, previous_number)) = previous else {
        return WeekChange::InitialBaseline;
    };
    if previous_term != current.term_key {
        WeekChange::NewSemesterBaseline
    } else if current.number > previous_number {
        WeekChange::Advanced
    } else {
        WeekChange::Unchanged
    }
}

#[derive(Clone)]
pub struct ScheduleSiteClient {
    client: Client,
}

impl ScheduleSiteClient {
    pub fn new() -> Result<Self> {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::ACCEPT,
            HeaderValue::from_static("text/html,application/xhtml+xml,*/*"),
        );
        headers.insert(
            header::REFERER,
            HeaderValue::from_static("https://itf.donstu.ru/"),
        );
        let client = Client::builder()
            .user_agent(USER_AGENT)
            .default_headers(headers)
            .timeout(StdDuration::from_secs(20))
            .build()
            .context("не удалось создать HTTP-клиент сайта расписания")?;
        Ok(Self { client })
    }

    pub async fn fetch_latest_week(&self) -> Result<WebsiteWeek> {
        let response = self
            .client
            .get(SCHEDULE_PAGE)
            .send()
            .await
            .context("не удалось запросить страницу расписания")?
            .error_for_status()
            .context("страница расписания вернула ошибочный HTTP-статус")?;
        if response
            .content_length()
            .is_some_and(|length| length > MAX_PAGE_BYTES as u64)
        {
            bail!("страница расписания больше допустимого размера");
        }
        let page = response
            .bytes()
            .await
            .context("не удалось прочитать страницу расписания")?;
        if page.len() > MAX_PAGE_BYTES {
            bail!("страница расписания больше допустимого размера");
        }
        parse_latest_week(std::str::from_utf8(&page).context("страница расписания не UTF-8")?)
    }
}

pub fn parse_latest_week(source: &str) -> Result<WebsiteWeek> {
    let document = Html::parse_document(source);
    let section_selector = Selector::parse(".js-selector-tabs")
        .map_err(|error| anyhow::anyhow!("неверный CSS-селектор: {error}"))?;
    let select_selector = Selector::parse("select.js-selector-tabs-select")
        .map_err(|error| anyhow::anyhow!("неверный CSS-селектор: {error}"))?;
    let option_selector = Selector::parse("option")
        .map_err(|error| anyhow::anyhow!("неверный CSS-селектор: {error}"))?;
    let contents_selector = Selector::parse(".js-selector-tabs-content")
        .map_err(|error| anyhow::anyhow!("неверный CSS-селектор: {error}"))?;
    let document_link_selector = Selector::parse("a.document-card__link[download]")
        .map_err(|error| anyhow::anyhow!("неверный CSS-селектор: {error}"))?;

    let section = document
        .select(&section_selector)
        .next()
        .context("не найден переключатель недель на странице")?;
    let select = section
        .select(&select_selector)
        .next()
        .context("не найден исходный select учебных недель")?;
    let options = select.select(&option_selector).collect::<Vec<_>>();
    let selected_index = options
        .iter()
        .position(|option| option.value().attr("selected").is_some())
        .unwrap_or(0);
    let selected_option = options
        .get(selected_index)
        .context("список учебных недель пуст")?;
    let selected_number = parse_week_number(&selected_option.text().collect::<String>())
        .context("не удалось разобрать номер выбранной учебной недели")?;

    let tabs = section
        .select(&contents_selector)
        .next()
        .context("не найдены секции документов учебных недель")?;
    let selected_panel = tabs
        .children()
        .filter_map(ElementRef::wrap)
        .nth(selected_index)
        .context("выбранной неделе не соответствует секция документов")?;

    for link in selected_panel.select(&document_link_selector) {
        let Some(file_name) = link.value().attr("download") else {
            continue;
        };
        let Some((number, term_key)) = parse_week_pdf_name(file_name) else {
            continue;
        };
        if number != selected_number {
            continue;
        }
        let href = link
            .value()
            .attr("href")
            .context("у PDF расписания отсутствует ссылка")?;
        let file_url = reqwest::Url::parse(SCHEDULE_PAGE)?
            .join(href)
            .context("не удалось собрать ссылку на PDF расписания")?
            .to_string();
        return Ok(WebsiteWeek {
            number,
            term_key,
            file_name: file_name.to_owned(),
            file_url,
        });
    }

    bail!("в выбранной секции нет PDF для учебной недели {selected_number}")
}

fn parse_week_number(label: &str) -> Option<u32> {
    let mut words = label.split_whitespace();
    if words.next()?.to_lowercase() != "расписание" {
        return None;
    }
    words.next()?.parse().ok()
}

fn parse_week_pdf_name(file_name: &str) -> Option<(u32, String)> {
    let name = file_name.trim();
    let lower = name.to_lowercase();
    if !lower.ends_with(".pdf") || !lower.contains("офо") || !lower.contains("озфо") {
        return None;
    }
    let prefix = "расписание ";
    let rest = lower.strip_prefix(prefix)?;
    let number_end = rest.find(char::is_whitespace)?;
    let number = rest[..number_end].parse::<u32>().ok()?;
    let rest = rest[number_end..].trim_start();
    let rest = rest.strip_prefix("учебной недели")?;
    let term_key = strip_duplicate_suffix(rest.trim().trim_end_matches(".pdf").trim()).to_owned();
    if term_key.is_empty() {
        return None;
    }
    Some((number, term_key))
}

fn strip_duplicate_suffix(value: &str) -> &str {
    let Some(without_close) = value.strip_suffix(')') else {
        return value;
    };
    let Some((prefix, digits)) = without_close.rsplit_once('(') else {
        return value;
    };
    if !digits.is_empty() && digits.chars().all(|character| character.is_ascii_digit()) {
        prefix.trim_end()
    } else {
        value
    }
}

pub fn scheduled_check_slot(now: DateTime<Tz>) -> Option<DateTime<Utc>> {
    // The worker wakes every 15 seconds, but its phase depends on when the
    // process started. Keep a short window so startup jitter does not skip a
    // scheduled hour; the database claim deduplicates retries and replicas.
    if now.minute() >= 15 {
        return None;
    }
    let hour = now.hour();
    let enabled = match now.weekday() {
        Weekday::Fri => hour == 18 || hour == 21,
        Weekday::Sat | Weekday::Sun => hour % 3 == 0,
        Weekday::Mon => hour == 0,
        _ => false,
    };
    enabled.then(|| {
        now.with_minute(0)
            .and_then(|slot| slot.with_second(0))
            .and_then(|slot| slot.with_nanosecond(0))
            .expect("a valid top-of-hour time")
            .with_timezone(&Utc)
    })
}

pub fn target_week_start(now: DateTime<Tz>) -> NaiveDate {
    let monday = now
        .date_naive()
        .checked_sub_signed(Duration::days(now.weekday().num_days_from_monday() as i64))
        .expect("current date has a previous Monday");
    if now.weekday() == Weekday::Mon && now.hour() == 0 {
        monday
    } else {
        monday
            .checked_add_signed(Duration::days(7))
            .expect("next Monday is representable")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{NaiveDate, TimeZone};

    const FIXTURE: &str = r#"
      <section class="app-section js-selector-tabs">
        <select class="js-selector-tabs-select">
          <option value="0" selected>Расписание 5 неделя</option>
          <option value="1">Расписание 4 недели</option>
        </select>
        <div class="js-selector-tabs-content">
          <div><a class="document-card__link" href="/uploads/week5.pdf" download="Расписание 5 учебной недели осеннего семестра ОФО и ОЗФО.pdf"></a></div>
          <div><a class="document-card__link" href="/uploads/week4.pdf" download="Расписание 4 учебной недели осеннего семестра ОФО и ОЗФО.pdf"></a></div>
        </div>
      </section>
    "#;

    fn local(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> DateTime<Tz> {
        chrono_tz::Europe::Moscow
            .with_ymd_and_hms(year, month, day, hour, minute, 0)
            .single()
            .unwrap()
    }

    #[test]
    fn parses_only_the_pdf_for_the_selected_week() {
        let week = parse_latest_week(FIXTURE).unwrap();
        assert_eq!(week.number, 5);
        assert_eq!(week.term_key, "осеннего семестра офо и озфо");
        assert_eq!(
            week.file_name,
            "Расписание 5 учебной недели осеннего семестра ОФО и ОЗФО.pdf"
        );
        assert_eq!(week.file_url, "https://itf.donstu.ru/uploads/week5.pdf");
    }

    #[test]
    fn rejects_a_week_when_selected_panel_has_no_matching_scan() {
        let html = FIXTURE.replace("Расписание 5 учебной недели", "Расписание 4 учебной недели");
        assert!(parse_latest_week(&html).is_err());
    }

    #[test]
    fn week_counter_uses_a_silent_baseline_and_handles_semester_reset() {
        let current = parse_latest_week(FIXTURE).unwrap();
        assert_eq!(
            classify_week_change(None, &current),
            WeekChange::InitialBaseline
        );
        assert_eq!(
            classify_week_change(Some(("весеннего семестра офо и озфо", 12)), &current),
            WeekChange::NewSemesterBaseline
        );
        assert_eq!(
            classify_week_change(Some((&current.term_key, 4)), &current),
            WeekChange::Advanced
        );
        assert_eq!(
            classify_week_change(Some((&current.term_key, 5)), &current),
            WeekChange::Unchanged
        );
    }

    #[test]
    fn duplicate_download_suffix_does_not_change_the_semester_key() {
        let regular =
            parse_week_pdf_name("Расписание 5 учебной недели осеннего семестра ОФО и ОЗФО.pdf")
                .unwrap();
        let duplicate =
            parse_week_pdf_name("Расписание 5 учебной недели осеннего семестра ОФО и ОЗФО (2).pdf")
                .unwrap();
        assert_eq!(regular, duplicate);
    }

    #[test]
    fn polls_every_three_hours_from_friday_evening_through_monday_midnight() {
        assert!(scheduled_check_slot(local(2026, 9, 25, 17, 0)).is_none());
        assert!(scheduled_check_slot(local(2026, 9, 25, 18, 0)).is_some());
        assert!(scheduled_check_slot(local(2026, 9, 25, 21, 0)).is_some());
        for (day, hour) in [(26, 0), (26, 3), (26, 21), (27, 0), (27, 21)] {
            assert!(scheduled_check_slot(local(2026, 9, day, hour, 0)).is_some());
        }
        assert!(scheduled_check_slot(local(2026, 9, 28, 0, 0)).is_some());
        assert!(scheduled_check_slot(local(2026, 9, 28, 3, 0)).is_none());
        assert!(scheduled_check_slot(local(2026, 9, 25, 18, 14)).is_some());
        assert!(scheduled_check_slot(local(2026, 9, 25, 18, 15)).is_none());
    }

    #[test]
    fn target_week_stays_the_same_until_monday_midnight_rollover() {
        let target = NaiveDate::from_ymd_opt(2026, 9, 28).unwrap();
        assert_eq!(target_week_start(local(2026, 9, 25, 18, 0)), target);
        assert_eq!(target_week_start(local(2026, 9, 27, 21, 0)), target);
        assert_eq!(target_week_start(local(2026, 9, 28, 0, 0)), target);
    }
}
