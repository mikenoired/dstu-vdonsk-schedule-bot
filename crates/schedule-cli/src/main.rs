use ab_glyph::FontArc;
use anyhow::{Context, Result, bail};
use chrono::{Datelike, NaiveDate};
use clap::{Parser, Subcommand};
use image::{ImageBuffer, ImageFormat, Rgb};
use imageproc::drawing::{
    draw_filled_circle_mut, draw_filled_rect_mut, draw_hollow_rect_mut, draw_text_mut, text_size,
};
use imageproc::rect::Rect;
use schedule_parser::{Lesson, display_room_name, for_date, for_group, for_room, parse_file};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

#[derive(Debug, Parser)]
#[command(name = "schedule", version, about = "Запросы к расписанию занятий")]
struct Cli {
    /// Путь к исходному файлу расписания (.xls или .xlsx).
    #[arg(short, long, global = true)]
    input: Option<PathBuf>,

    /// Сохранить тот же список как изображение JPEG.
    #[arg(long, global = true, value_name = "FILE")]
    jpeg: Option<PathBuf>,

    /// Дополнительно записать JSON в файл (по умолчанию JSON выводится в stdout).
    #[arg(long, global = true, value_name = "FILE")]
    json: Option<PathBuf>,

    /// Показать список занятий с форматированием и эмодзи вместо JSON в stdout.
    #[arg(short = 'p', long, global = true)]
    pretty: bool,

    /// Путь к TTF-шрифту, если системный шрифт не найден.
    #[arg(long, global = true, value_name = "FILE")]
    font: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Расписание группы на всю неделю.
    Week {
        #[arg(long)]
        group: String,
    },
    /// Все занятия в аудитории за неделю.
    Room {
        #[arg(long)]
        room: String,
    },
    /// Все занятия указанного дня (дата ДД.ММ.ГГГГ или ГГГГ-ММ-ДД).
    Day {
        #[arg(long)]
        date: String,
    },
}

fn main() -> Result<()> {
    // Keep the requested single-dash spelling available alongside `--pretty` and `-p`.
    let args = std::env::args_os().map(|arg| {
        if arg == "-pretty" {
            OsString::from("--pretty")
        } else {
            arg
        }
    });
    let cli = Cli::parse_from(args);
    let input = match cli.input {
        Some(path) => path,
        None => find_default_schedule()?,
    };
    let lessons =
        parse_file(&input).with_context(|| format!("ошибка при чтении {}", input.display()))?;

    let (title, pretty_heading, show_groups, show_rooms, show_day_headings, selected) =
        match cli.command {
            Command::Week { group } => {
                let selected = for_group(&lessons, &group);
                ensure_found(
                    !selected.is_empty(),
                    format!("группа `{group}` не найдена или в расписании нет занятий"),
                )?;
                (
                    format!("Расписание группы {group}"),
                    format!("📚 Расписание группы {group}"),
                    false,
                    true,
                    true,
                    selected,
                )
            }
            Command::Room { room } => {
                let selected = for_room(&lessons, &room);
                let display_room = display_room_name(&room);
                ensure_found(
                    !selected.is_empty(),
                    format!("в аудитории `{display_room}` нет занятий"),
                )?;
                (
                    format!("Расписание: {display_room}"),
                    format!("🏫 Расписание: {display_room}"),
                    true,
                    false,
                    true,
                    selected,
                )
            }
            Command::Day { date } => {
                let date = parse_cli_date(&date)?;
                let selected = for_date(&lessons, date);
                ensure_found(!selected.is_empty(), format!("на {date} нет занятий"))?;
                (
                    format!("Расписание на {date}"),
                    format!(
                        "📅 Расписание на {} · {}",
                        date.format("%d.%m.%Y"),
                        selected[0].weekday
                    ),
                    true,
                    true,
                    false,
                    selected,
                )
            }
        };

    let json = serde_json::to_string_pretty(&selected)?;
    if cli.pretty {
        println!(
            "{}",
            format_pretty(
                &selected,
                &pretty_heading,
                show_groups,
                show_rooms,
                show_day_headings,
            )
        );
    } else {
        println!("{json}");
    }
    if let Some(path) = cli.json {
        std::fs::write(&path, &json)
            .with_context(|| format!("не удалось записать JSON {}", path.display()))?;
        eprintln!("JSON сохранён: {}", path.display());
    }
    if let Some(path) = cli.jpeg {
        render_jpeg(&selected, &title, &path, cli.font.as_deref())?;
        eprintln!("JPEG сохранён: {}", path.display());
    }
    Ok(())
}

fn ensure_found(found: bool, message: String) -> Result<()> {
    if !found {
        bail!(message);
    }
    Ok(())
}

fn parse_cli_date(value: &str) -> Result<NaiveDate> {
    NaiveDate::parse_from_str(value, "%d.%m.%Y")
        .or_else(|_| NaiveDate::parse_from_str(value, "%Y-%m-%d"))
        .with_context(|| format!("неверная дата `{value}`; используйте ДД.ММ.ГГГГ или ГГГГ-ММ-ДД"))
}

fn format_pretty(
    lessons: &[&Lesson],
    heading: &str,
    show_groups: bool,
    show_rooms: bool,
    show_day_headings: bool,
) -> String {
    let mut output = heading.to_owned();
    if show_day_headings {
        if let (Some(first), Some(last)) = (lessons.first(), lessons.last()) {
            output.push_str(&format!(" · {}", format_date_range(first.date, last.date)));
        }
    }
    output.push_str("\n━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");

    let mut current_date = None;
    for lesson in lessons {
        if show_day_headings && current_date != Some(lesson.date) {
            if current_date.is_some() {
                output.push('\n');
            }
            output.push_str(&format!(
                "\n🗓️ {} · {}\n",
                lesson.weekday,
                lesson.date.format("%d.%m.%Y")
            ));
            current_date = Some(lesson.date);
        } else if !output.ends_with('\n') {
            output.push('\n');
        }

        output.push_str(&format!(
            "\n🕒 {}–{} · пара №{}\n",
            lesson.start_time, lesson.end_time, lesson.lesson_number
        ));
        if show_groups {
            output.push_str(&format!("   👥 {}\n", lesson.groups.join(", ")));
        }
        output.push_str(&format!("   📖 {}\n", lesson.subject));

        let mut details = Vec::new();
        if let Some(kind) = &lesson.lesson_type {
            details.push(format!("🧩 {kind}"));
        }
        if let Some(teacher) = &lesson.teacher {
            details.push(format!("👩‍🏫 {teacher}"));
        }
        if show_rooms {
            if let Some(room) = &lesson.room {
                details.push(format!("📍 {room}"));
            }
        }
        if !details.is_empty() {
            output.push_str(&format!("   {}\n", details.join(" · ")));
        }
    }

    output.push_str(&format!("\n✨ Всего занятий: {}", lessons.len()));
    output
}

fn format_date_range(start: NaiveDate, end: NaiveDate) -> String {
    if start == end {
        return start.format("%d.%m.%Y").to_string();
    }
    if start.year() == end.year() && start.month() == end.month() {
        format!(
            "{}–{}.{}.{}",
            start.day(),
            end.day(),
            end.month(),
            end.year()
        )
    } else {
        format!("{}–{}", start.format("%d.%m.%Y"), end.format("%d.%m.%Y"))
    }
}

fn find_default_schedule() -> Result<PathBuf> {
    let tables = Path::new("tables");
    let mut files = std::fs::read_dir(tables)
        .with_context(|| format!("не удалось открыть папку {}", tables.display()))?
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .and_then(|v| v.to_str())
                .is_some_and(|v| matches!(v.to_ascii_lowercase().as_str(), "xls" | "xlsx"))
        })
        .collect::<Vec<_>>();
    files.sort();
    match files.as_slice() {
        [file] => Ok(file.clone()),
        [] => bail!("в папке tables не найдено файлов .xls/.xlsx; передайте путь через --input"),
        _ => bail!("в папке tables несколько расписаний; укажите нужное через --input"),
    }
}

#[derive(Clone)]
struct Chip {
    label: String,
    background: Rgb<u8>,
    foreground: Rgb<u8>,
}

fn lesson_chips(lesson: &Lesson) -> Vec<Chip> {
    let mut chips = Vec::new();
    if let Some(kind) = &lesson.lesson_type {
        chips.push(Chip {
            label: format!("Вид: {kind}"),
            background: Rgb([237, 233, 254]),
            foreground: Rgb([91, 55, 145]),
        });
    }
    if let Some(teacher) = &lesson.teacher {
        chips.push(Chip {
            label: format!("Преподаватель: {teacher}"),
            background: Rgb([220, 245, 235]),
            foreground: Rgb([35, 111, 81]),
        });
    }
    if let Some(room) = &lesson.room {
        chips.push(Chip {
            label: format!("Аудитория: {room}"),
            background: Rgb([255, 237, 213]),
            foreground: Rgb([148, 78, 24]),
        });
    }
    chips
}

fn wrap_chips(chips: Vec<Chip>, font: &FontArc, scale: f32, max_width: u32) -> Vec<Vec<Chip>> {
    let mut rows = Vec::new();
    let mut current = Vec::new();
    let mut used_width = 0;
    for chip in chips {
        let chip_width = text_size(scale, font, &chip.label).0 + 28;
        let next_width = if current.is_empty() {
            chip_width
        } else {
            used_width + 10 + chip_width
        };
        if !current.is_empty() && next_width > max_width {
            rows.push(std::mem::take(&mut current));
            used_width = chip_width;
            current.push(chip);
        } else {
            used_width = next_width;
            current.push(chip);
        }
    }
    if !current.is_empty() {
        rows.push(current);
    }
    rows
}

fn draw_chip(
    image: &mut ImageBuffer<Rgb<u8>, Vec<u8>>,
    chip: &Chip,
    font: &FontArc,
    scale: f32,
    x: u32,
    y: u32,
) -> u32 {
    let text_width = text_size(scale, font, &chip.label).0;
    let width = text_width + 28;
    let height = 30;
    let radius = (height / 2) as i32;
    draw_filled_rect_mut(
        image,
        Rect::at((x + radius as u32) as i32, y as i32).of_size(width - height, height),
        chip.background,
    );
    draw_filled_circle_mut(
        image,
        ((x + radius as u32) as i32, (y + radius as u32) as i32),
        radius,
        chip.background,
    );
    draw_filled_circle_mut(
        image,
        (
            (x + width - radius as u32) as i32,
            (y + radius as u32) as i32,
        ),
        radius,
        chip.background,
    );
    draw_text_mut(
        image,
        chip.foreground,
        (x + 14) as i32,
        (y + 6) as i32,
        scale,
        font,
        &chip.label,
    );
    width
}

fn render_jpeg(
    lessons: &[&Lesson],
    title: &str,
    path: &Path,
    font_override: Option<&Path>,
) -> Result<()> {
    let font = load_font(font_override)?;
    let scale = 22.0;
    let chip_scale = 17.0;
    let title_scale = 34.0;
    let width = 1400u32;
    let padding = 48u32;
    let content_width = width - padding * 2;
    let mut cards = Vec::with_capacity(lessons.len());
    let mut total_height = 160u32;
    let mut day_ranges = Vec::new();

    for (index, lesson) in lessons.iter().enumerate() {
        if day_ranges
            .last()
            .is_none_or(|(date, _, _)| *date != lesson.date)
        {
            day_ranges.push((lesson.date, index, index + 1));
            total_height += 54;
        } else if let Some((_, _, end)) = day_ranges.last_mut() {
            *end = index + 1;
        }
        let subject_lines = wrap_text(&lesson.subject, &font, scale, content_width - 36);
        let chips = lesson_chips(lesson);
        let chip_rows = wrap_chips(chips, &font, chip_scale, content_width - 36);
        let card_height = 50 + (subject_lines.len() as u32 * 31) + (chip_rows.len() as u32 * 38);
        total_height += card_height + 16;
        cards.push((subject_lines, chip_rows, card_height));
    }
    total_height += padding;

    let mut image =
        ImageBuffer::<Rgb<u8>, Vec<u8>>::from_pixel(width, total_height, Rgb([247, 249, 252]));
    draw_filled_rect_mut(
        &mut image,
        Rect::at(0, 0).of_size(width, 114),
        Rgb([25, 54, 91]),
    );
    draw_text_mut(
        &mut image,
        Rgb([255, 255, 255]),
        padding as i32,
        35,
        title_scale,
        &font,
        title,
    );

    let mut y = 140u32;
    for (date, start, end) in day_ranges {
        let first = lessons[start];
        let day_rect = Rect::at(padding as i32, y as i32).of_size(content_width, 42);
        draw_filled_rect_mut(&mut image, day_rect, Rgb([225, 235, 247]));
        draw_text_mut(
            &mut image,
            Rgb([25, 54, 91]),
            (padding + 16) as i32,
            (y + 9) as i32,
            22.0,
            &font,
            &format!("{} · {}", first.weekday, date.format("%d.%m.%Y")),
        );
        y += 50;

        for index in start..end {
            let lesson = lessons[index];
            let (subject_lines, chip_rows, card_height) = &cards[index];
            let rect = Rect::at(padding as i32, y as i32).of_size(content_width, *card_height);
            draw_filled_rect_mut(&mut image, rect, Rgb([255, 255, 255]));
            draw_hollow_rect_mut(&mut image, rect, Rgb([218, 225, 234]));

            let heading = format!(
                "{}–{} · пара №{} · {}",
                lesson.start_time,
                lesson.end_time,
                lesson.lesson_number,
                lesson.groups.join(", ")
            );
            draw_text_mut(
                &mut image,
                Rgb([38, 92, 150]),
                (padding + 16) as i32,
                (y + 10) as i32,
                scale,
                &font,
                &heading,
            );

            let mut text_y = y + 38;
            for line in subject_lines {
                draw_text_mut(
                    &mut image,
                    Rgb([29, 37, 48]),
                    (padding + 16) as i32,
                    text_y as i32,
                    scale,
                    &font,
                    line,
                );
                text_y += 30;
            }
            text_y += 4;
            for chips in chip_rows {
                let mut x = padding + 16;
                for chip in chips {
                    let chip_width = draw_chip(&mut image, chip, &font, chip_scale, x, text_y);
                    x += chip_width + 10;
                }
                text_y += 36;
            }
            y += card_height + 14;
        }
    }

    image
        .save_with_format(path, ImageFormat::Jpeg)
        .with_context(|| format!("не удалось записать JPEG {}", path.display()))
}

fn wrap_text(text: &str, font: &FontArc, scale: f32, max_width: u32) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        let candidate = if current.is_empty() {
            word.to_owned()
        } else {
            format!("{current} {word}")
        };
        if !current.is_empty() && text_size(scale, font, &candidate).0 > max_width {
            lines.push(std::mem::take(&mut current));
            current.push_str(word);
        } else {
            current = candidate;
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

fn load_font(override_path: Option<&Path>) -> Result<FontArc> {
    let candidates = override_path.into_iter().map(Path::to_path_buf).chain([
        PathBuf::from("/System/Library/Fonts/Supplemental/Arial.ttf"),
        PathBuf::from("/Library/Fonts/Arial.ttf"),
        PathBuf::from("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf"),
        PathBuf::from("/usr/share/fonts/truetype/liberation2/LiberationSans-Regular.ttf"),
    ]);
    for path in candidates {
        if let Ok(bytes) = std::fs::read(&path) {
            if let Ok(font) = FontArc::try_from_vec(bytes) {
                return Ok(font);
            }
        }
    }
    bail!("не найден TTF-шрифт с кириллицей; укажите путь параметром --font")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lesson() -> Lesson {
        Lesson {
            groups: vec!["ИС11В".into()],
            date: NaiveDate::from_ymd_opt(2026, 9, 28).unwrap(),
            weekday: "Понедельник".into(),
            lesson_number: 1,
            start_time: "08:20".into(),
            end_time: "09:55".into(),
            subject: "Линейная алгебра".into(),
            lesson_type: Some("лек.+лек.".into()),
            teacher: Some("Петрова Э.А.".into()),
            room: Some("303/2".into()),
            description: "Линейная алгебра (лек.+лек.) Петрова Э.А.".into(),
        }
    }

    #[test]
    fn pretty_group_output_keeps_group_in_heading() {
        let lesson = lesson();
        let output = format_pretty(&[&lesson], "📚 Расписание группы ИС11В", false, true, true);
        assert_eq!(output.matches("ИС11В").count(), 1);
        assert!(output.contains("🗓️ Понедельник"));
        assert!(output.contains("📍 303/2"));
    }

    #[test]
    fn pretty_room_output_keeps_room_in_heading() {
        let lesson = lesson();
        let output = format_pretty(
            &[&lesson],
            "🏫 Расписание в аудитории 303/2",
            true,
            false,
            true,
        );
        assert_eq!(output.matches("303/2").count(), 1);
        assert!(output.contains("👥 ИС11В"));
    }

    #[test]
    fn pretty_day_output_does_not_repeat_the_date() {
        let lesson = lesson();
        let output = format_pretty(
            &[&lesson],
            "📅 Расписание на 28.09.2026 · Понедельник",
            true,
            true,
            false,
        );
        assert_eq!(output.matches("28.09.2026").count(), 1);
        assert!(output.contains("🕒 08:20–09:55"));
    }
}
