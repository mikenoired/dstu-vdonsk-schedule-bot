use ab_glyph::{FontArc, PxScale};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use image::{ImageBuffer, ImageFormat, Rgb, RgbImage};
use imageproc::{
    drawing::{draw_filled_rect_mut, draw_hollow_rect_mut, draw_line_segment_mut, draw_text_mut},
    rect::Rect,
};
use redis::{AsyncCommands, aio::ConnectionManager};
use std::io::Cursor;

const MINUTE: i64 = 60;
const HOUR: i64 = 60 * MINUTE;
const MINUTE_TTL: i64 = 2 * 24 * HOUR;
const HOUR_TTL: i64 = 9 * 24 * HOUR;
const METRICS: [Metric; 5] = [
    Metric::Commands,
    Metric::RateLimited,
    Metric::OutboxSent,
    Metric::OutboxFailed,
    Metric::DailyEnqueued,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Metric {
    Commands,
    RateLimited,
    OutboxSent,
    OutboxFailed,
    DailyEnqueued,
}

impl Metric {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Commands => "commands",
            Self::RateLimited => "rate_limited",
            Self::OutboxSent => "outbox_sent",
            Self::OutboxFailed => "outbox_failed",
            Self::DailyEnqueued => "daily_enqueued",
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Commands => "Действия",
            Self::RateLimited => "Антиспам",
            Self::OutboxSent => "Отправлено",
            Self::OutboxFailed => "Ошибки отправки",
            Self::DailyEnqueued => "Поставлено в очередь",
        }
    }

    const fn color(self) -> Rgb<u8> {
        match self {
            Self::Commands => Rgb([61, 154, 255]),
            Self::RateLimited => Rgb([255, 180, 72]),
            Self::OutboxSent => Rgb([67, 204, 150]),
            Self::OutboxFailed => Rgb([255, 93, 108]),
            Self::DailyEnqueued => Rgb([170, 133, 255]),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Period {
    ThirtyMinutes,
    Hour,
    Day,
    Week,
}

impl Period {
    pub const fn callback(self) -> &'static str {
        match self {
            Self::ThirtyMinutes => "30m",
            Self::Hour => "1h",
            Self::Day => "24h",
            Self::Week => "7d",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::ThirtyMinutes => "30 минут",
            Self::Hour => "1 час",
            Self::Day => "24 часа",
            Self::Week => "7 дней",
        }
    }

    pub const fn seconds(self) -> i64 {
        match self {
            Self::ThirtyMinutes => 30 * MINUTE,
            Self::Hour => HOUR,
            Self::Day => 24 * HOUR,
            Self::Week => 7 * 24 * HOUR,
        }
    }

    pub fn from_callback(value: &str) -> Option<Self> {
        match value {
            "30m" => Some(Self::ThirtyMinutes),
            "1h" => Some(Self::Hour),
            "24h" => Some(Self::Day),
            "7d" => Some(Self::Week),
            _ => None,
        }
    }

    const fn bucket_seconds(self) -> i64 {
        match self {
            Self::Week => HOUR,
            _ => MINUTE,
        }
    }
}

#[derive(Debug, Clone)]
pub struct DashboardData {
    pub period: Period,
    pub totals: [u64; 5],
    pub points: [Vec<u64>; 5],
    pub queued: i64,
    pub scheduler_last_success: u64,
    pub generated_at: DateTime<Utc>,
}

impl DashboardData {
    pub fn from_bucket_values(
        period: Period,
        bucket_values: [Vec<u64>; 5],
        queued: i64,
        scheduler_last_success: u64,
        generated_at: DateTime<Utc>,
    ) -> Self {
        let totals = std::array::from_fn(|index| bucket_values[index].iter().sum());
        let point_count = bucket_values[0].len().min(120);
        let points =
            std::array::from_fn(|metric| aggregate_points(&bucket_values[metric], point_count));
        Self {
            period,
            totals,
            points,
            queued,
            scheduler_last_success,
            generated_at,
        }
    }
}

fn aggregate_points(values: &[u64], point_count: usize) -> Vec<u64> {
    if point_count == 0 || values.is_empty() {
        return Vec::new();
    }
    (0..point_count)
        .map(|point| {
            let start = point * values.len() / point_count;
            let end = ((point + 1) * values.len() / point_count).max(start + 1);
            values[start..end.min(values.len())].iter().sum()
        })
        .collect()
}

#[derive(Clone)]
pub struct StatsStore {
    redis: ConnectionManager,
}

impl StatsStore {
    pub async fn connect(redis_url: &str) -> Result<Self> {
        let client = redis::Client::open(redis_url).context("неверный REDIS_URL для метрик")?;
        let redis = client
            .get_connection_manager()
            .await
            .context("не удалось подключиться к Redis для метрик")?;
        Ok(Self { redis })
    }

    pub async fn record(&self, metric: Metric, amount: u64) -> Result<()> {
        if amount == 0 {
            return Ok(());
        }
        let now = Utc::now().timestamp();
        let minute = now.div_euclid(MINUTE);
        let hour = now.div_euclid(HOUR);
        let script = redis::Script::new(
            "redis.call('INCRBY', KEYS[1], ARGV[1]); \
             redis.call('EXPIRE', KEYS[1], ARGV[2]); \
             redis.call('INCRBY', KEYS[2], ARGV[1]); \
             redis.call('EXPIRE', KEYS[2], ARGV[3]); return 1",
        );
        let mut redis = self.redis.clone();
        let _: i64 = script
            .key(bucket_key("minute", minute, metric))
            .key(bucket_key("hour", hour, metric))
            .arg(amount)
            .arg(MINUTE_TTL)
            .arg(HOUR_TTL)
            .invoke_async(&mut redis)
            .await
            .context("ошибка записи временной метрики в Redis")?;
        Ok(())
    }

    pub async fn set_scheduler_last_success(&self, timestamp: u64) -> Result<()> {
        let mut redis = self.redis.clone();
        redis
            .set::<_, _, ()>("schedule:stats:v1:scheduler_last_success", timestamp)
            .await
            .context("ошибка записи времени планировщика в Redis")?;
        Ok(())
    }

    pub async fn scheduler_last_success(&self) -> Result<Option<u64>> {
        let mut redis = self.redis.clone();
        redis
            .get("schedule:stats:v1:scheduler_last_success")
            .await
            .context("ошибка чтения времени планировщика из Redis")
    }

    pub async fn dashboard(
        &self,
        period: Period,
        queued: i64,
        scheduler_last_success: u64,
    ) -> Result<DashboardData> {
        let now = Utc::now();
        let step = period.bucket_seconds();
        let last_bucket = now.timestamp().div_euclid(step);
        let first_bucket = (now.timestamp() - period.seconds()).div_euclid(step);
        let bucket_count = (last_bucket - first_bucket + 1) as usize;
        let keys = (first_bucket..=last_bucket)
            .flat_map(|bucket| METRICS.map(|metric| bucket_key(bucket_kind(step), bucket, metric)))
            .collect::<Vec<_>>();

        let mut redis = self.redis.clone();
        let values: Vec<Option<u64>> = redis::cmd("MGET")
            .arg(keys)
            .query_async(&mut redis)
            .await
            .context("ошибка чтения временных метрик из Redis")?;
        let mut bucket_values: [Vec<u64>; 5] =
            std::array::from_fn(|_| Vec::with_capacity(bucket_count));
        for bucket in 0..bucket_count {
            for (metric_index, series) in bucket_values.iter_mut().enumerate() {
                series.push(
                    values
                        .get(bucket * METRICS.len() + metric_index)
                        .copied()
                        .flatten()
                        .unwrap_or(0),
                );
            }
        }
        Ok(DashboardData::from_bucket_values(
            period,
            bucket_values,
            queued,
            scheduler_last_success,
            now,
        ))
    }
}

fn bucket_kind(step: i64) -> &'static str {
    if step == HOUR { "hour" } else { "minute" }
}

fn bucket_key(kind: &str, bucket: i64, metric: Metric) -> String {
    format!("schedule:stats:v1:{kind}:{bucket}:{}", metric.as_str())
}

pub fn render_jpeg(data: &DashboardData) -> Result<Vec<u8>> {
    let font = load_font()?;
    let mut image = ImageBuffer::from_pixel(1080, 1180, Rgb([15, 22, 35]));
    let white = Rgb([239, 244, 252]);
    let muted = Rgb([147, 162, 183]);

    draw_text_mut(
        &mut image,
        muted,
        56,
        42,
        PxScale::from(24.0),
        &font,
        "SCHEDULE BOT  /  ОПЕРАЦИОННАЯ СВОДКА",
    );
    draw_text_mut(
        &mut image,
        white,
        56,
        84,
        PxScale::from(48.0),
        &font,
        "Статистика бота",
    );
    draw_text_mut(
        &mut image,
        Rgb([100, 184, 255]),
        56,
        146,
        PxScale::from(28.0),
        &font,
        &format!("Период: {}", data.period.label()),
    );

    let cards = [
        (Metric::Commands, data.totals[0], 56, 208),
        (Metric::OutboxSent, data.totals[2], 548, 208),
        (Metric::OutboxFailed, data.totals[3], 56, 340),
        (Metric::RateLimited, data.totals[1], 548, 340),
    ];
    for (metric, count, x, y) in cards {
        let rect = Rect::at(x, y).of_size(476, 108);
        draw_filled_rect_mut(&mut image, rect, Rgb([25, 36, 54]));
        draw_hollow_rect_mut(&mut image, rect, Rgb([47, 64, 86]));
        draw_filled_rect_mut(
            &mut image,
            Rect::at(x + 20, y + 24).of_size(8, 60),
            metric.color(),
        );
        draw_text_mut(
            &mut image,
            muted,
            x + 44,
            y + 18,
            PxScale::from(23.0),
            &font,
            metric.label(),
        );
        draw_text_mut(
            &mut image,
            white,
            x + 44,
            y + 49,
            PxScale::from(38.0),
            &font,
            &count.to_string(),
        );
    }

    let queue_y = 486;
    draw_text_mut(
        &mut image,
        white,
        56,
        queue_y,
        PxScale::from(28.0),
        &font,
        "Состояние очереди",
    );
    draw_text_mut(
        &mut image,
        muted,
        56,
        queue_y + 44,
        PxScale::from(22.0),
        &font,
        "Уведомлений ожидают отправки",
    );
    let queued_label = if data.queued < 0 {
        "—".to_owned()
    } else {
        data.queued.to_string()
    };
    draw_text_mut(
        &mut image,
        Rgb([67, 204, 150]),
        760,
        queue_y + 32,
        PxScale::from(36.0),
        &font,
        &queued_label,
    );

    let chart = Rect::at(56, 584).of_size(968, 482);
    draw_filled_rect_mut(&mut image, chart, Rgb([20, 30, 47]));
    draw_hollow_rect_mut(&mut image, chart, Rgb([47, 64, 86]));
    draw_text_mut(
        &mut image,
        white,
        82,
        606,
        PxScale::from(25.0),
        &font,
        "Динамика событий",
    );
    for metric_index in 0..METRICS.len() {
        let row_y = 658 + metric_index as i32 * 76;
        draw_text_mut(
            &mut image,
            muted,
            82,
            row_y + 14,
            PxScale::from(19.0),
            &font,
            METRICS[metric_index].label(),
        );
        draw_text_mut(
            &mut image,
            white,
            82,
            row_y + 39,
            PxScale::from(22.0),
            &font,
            &data.totals[metric_index].to_string(),
        );
        let sparkline = Rect::at(400, row_y).of_size(584, 62);
        draw_filled_rect_mut(&mut image, sparkline, Rgb([25, 36, 54]));
        draw_series(
            &mut image,
            Rect::at(sparkline.left() + 5, sparkline.top() + 5).of_size(574, 52),
            &data.points[metric_index],
            METRICS[metric_index].color(),
        );
    }

    let scheduler_label = if data.scheduler_last_success == 0 {
        "последний запуск не записан".to_owned()
    } else {
        DateTime::from_timestamp(data.scheduler_last_success as i64, 0)
            .map(|date| format!("успешный запуск: {} UTC", date.format("%d.%m %H:%M")))
            .unwrap_or_else(|| "время запуска неизвестно".to_owned())
    };
    draw_text_mut(
        &mut image,
        muted,
        56,
        1114,
        PxScale::from(18.0),
        &font,
        &format!(
            "Обновлено {} UTC  ·  Планировщик: {scheduler_label}",
            data.generated_at.format("%d.%m.%Y %H:%M")
        ),
    );

    let mut cursor = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(image)
        .write_to(&mut cursor, ImageFormat::Jpeg)
        .context("не удалось закодировать dashboard в JPEG")?;
    Ok(cursor.into_inner())
}

fn draw_series(image: &mut RgbImage, area: Rect, values: &[u64], color: Rgb<u8>) {
    if values.is_empty() {
        return;
    }
    let maximum = values.iter().copied().max().unwrap_or(0).max(1) as f32;
    let last = values.len().saturating_sub(1).max(1) as f32;
    let mut previous = None;
    for (index, value) in values.iter().enumerate() {
        let x = area.left() as f32 + index as f32 / last * area.width().saturating_sub(1) as f32;
        let y = area.bottom() as f32 - (*value as f32 / maximum) * area.height() as f32;
        if let Some((previous_x, previous_y)) = previous {
            draw_line_segment_mut(image, (previous_x, previous_y), (x, y), color);
        }
        previous = Some((x, y));
    }
}

fn load_font() -> Result<FontArc> {
    let candidates = [
        std::env::var("DASHBOARD_FONT").ok(),
        Some("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf".to_owned()),
        Some("/Library/Fonts/Arial Unicode.ttf".to_owned()),
        Some("/System/Library/Fonts/Supplemental/Arial.ttf".to_owned()),
        Some("/Library/Fonts/Arial.ttf".to_owned()),
    ];
    for path in candidates.into_iter().flatten() {
        if let Ok(bytes) = std::fs::read(path) {
            if let Ok(font) = FontArc::try_from_vec(bytes) {
                return Ok(font);
            }
        }
    }
    anyhow::bail!("шрифт для JPEG-дашборда не найден; укажи путь через DASHBOARD_FONT")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn period_callback_values_round_trip() {
        for period in [
            Period::ThirtyMinutes,
            Period::Hour,
            Period::Day,
            Period::Week,
        ] {
            assert_eq!(Period::from_callback(period.callback()), Some(period));
        }
        assert_eq!(Period::from_callback("month"), None);
    }

    #[test]
    fn dashboard_sums_series_and_aggregates_points() {
        let data = DashboardData::from_bucket_values(
            Period::Hour,
            [
                vec![1, 2, 3, 4],
                vec![0; 4],
                vec![2, 0, 1, 0],
                vec![0; 4],
                vec![1; 4],
            ],
            7,
            0,
            DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
        );
        assert_eq!(data.totals, [10, 0, 3, 0, 4]);
        assert_eq!(data.points[0], vec![1, 2, 3, 4]);
        assert_eq!(data.queued, 7);
    }

    #[test]
    fn renderer_produces_jpeg_bytes() {
        let data = DashboardData::from_bucket_values(
            Period::Day,
            std::array::from_fn(|_| vec![0; 24]),
            0,
            0,
            Utc::now(),
        );
        let jpeg = render_jpeg(&data).unwrap();
        assert!(jpeg.starts_with(&[0xff, 0xd8, 0xff]));
        assert!(jpeg.len() > 10_000);
    }
}
