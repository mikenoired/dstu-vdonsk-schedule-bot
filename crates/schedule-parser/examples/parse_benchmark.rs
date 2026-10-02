use schedule_parser::parse_file;
use std::{
    env,
    error::Error,
    hint::black_box,
    path::PathBuf,
    time::{Duration, Instant},
};

const WARMUP_RUNS: usize = 50;
const MEASURED_RUNS: usize = 1_000;

fn main() -> Result<(), Box<dyn Error>> {
    let paths = env::args_os()
        .skip(1)
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    if paths.is_empty() {
        return Err("usage: cargo run --release -p schedule-parser --example parse_benchmark -- <workbook>...".into());
    }

    println!("warmup={WARMUP_RUNS}, measured_runs={MEASURED_RUNS}, cache=warm");
    println!("file_bytes,lessons,median_ms,p95_ms,lessons_per_second,MiB_per_second,file");
    let mut workloads = Vec::with_capacity(paths.len());
    for path in paths {
        let file_bytes = std::fs::metadata(&path)?.len();
        let mut lesson_count = 0;
        for _ in 0..WARMUP_RUNS {
            lesson_count = black_box(parse_file(black_box(&path))?).len();
        }

        let mut durations = Vec::with_capacity(MEASURED_RUNS);
        for _ in 0..MEASURED_RUNS {
            let start = Instant::now();
            let lessons = black_box(parse_file(black_box(&path))?);
            lesson_count = lessons.len();
            durations.push(start.elapsed());
        }
        workloads.push((path, file_bytes, lesson_count, durations));
    }

    for (path, file_bytes, lesson_count, durations) in &mut workloads {
        durations.sort_unstable();
        let median = durations[MEASURED_RUNS / 2];
        let p95 = durations[(MEASURED_RUNS * 95).div_ceil(100) - 1];
        let seconds = median.as_secs_f64();
        let lesson_rate = *lesson_count as f64 / seconds;
        let mib_rate = *file_bytes as f64 / (1024.0 * 1024.0) / seconds;

        println!(
            "{file_bytes},{lesson_count},{:.3},{:.3},{lesson_rate:.0},{mib_rate:.2},{}",
            millis(median),
            millis(p95),
            path.display()
        );
    }

    let batch_lessons = workloads
        .iter()
        .map(|(_, _, lessons, _)| lessons)
        .sum::<usize>();
    let batch_bytes = workloads.iter().map(|(_, bytes, _, _)| bytes).sum::<u64>();
    let mut batch_durations = Vec::with_capacity(MEASURED_RUNS);
    for _ in 0..WARMUP_RUNS {
        for (path, _, _, _) in &workloads {
            black_box(parse_file(black_box(path))?);
        }
    }
    for _ in 0..MEASURED_RUNS {
        let start = Instant::now();
        for (path, _, _, _) in &workloads {
            black_box(parse_file(black_box(path))?);
        }
        batch_durations.push(start.elapsed());
    }
    batch_durations.sort_unstable();
    let median = batch_durations[MEASURED_RUNS / 2];
    let p95 = batch_durations[(MEASURED_RUNS * 95).div_ceil(100) - 1];
    println!(
        "batch: files={},bytes={batch_bytes},lessons={batch_lessons},median_ms={:.3},p95_ms={:.3}",
        workloads.len(),
        millis(median),
        millis(p95)
    );
    Ok(())
}

fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}
