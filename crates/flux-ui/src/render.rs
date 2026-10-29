// Copyright (c) 2026 Costas Kirgoussios
//
// This software is licensed under the PolyForm Noncommercial License 1.0.0.
// You may obtain a copy of the License at:
// https://polyformproject.org/licenses/noncommercial/1.0.0/

//! Pure rendering of a progress [`Snapshot`] into display lines.

use flux_core::progress::Snapshot;
use indicatif::{HumanBytes, HumanCount};
use std::time::Duration;

const BAR_WIDTH: u128 = 20;

fn line(label: &str, value: &str) -> String {
    format!("{label:<14}{value}")
}

/// Render one frame: the folder frame, or the single-file frame when the
/// snapshot carries an expected size.
#[must_use]
pub fn render(s: &Snapshot, elapsed: Duration) -> Vec<String> {
    let bytes_per_second = if elapsed > Duration::ZERO && s.bytes > 0 {
        Some((s.bytes as f64 / elapsed.as_secs_f64()) as u64)
    } else {
        None
    };
    let speed = bytes_per_second.map_or_else(|| "-".to_owned(), |b| format!("{}/s", HumanBytes(b)));
    match s.expected_bytes {
        None => {
            let processed = (s.copied + s.resumed).min(s.discovered);
            let mut lines = vec![
                line("Discovered", &HumanCount(s.discovered).to_string()),
                line("Processed", &HumanCount(processed).to_string()),
                line("Transferred", &HumanBytes(s.bytes).to_string()),
                line("Speed", &speed),
            ];
            if s.resumed > 0 {
                lines.push(line("Resumed", &HumanCount(s.resumed).to_string()));
            }
            if s.errors > 0 {
                lines.push(line("Errors", &HumanCount(s.errors).to_string()));
            }
            lines
        }
        Some(total) => {
            let done = s.bytes.min(total);
            let pct = u128::from(done) * 100 / u128::from(total);
            let filled = (pct * BAR_WIDTH / 100) as usize;
            let bar = format!("{}{}", "#".repeat(filled), ".".repeat(BAR_WIDTH as usize - filled));
            let eta = bytes_per_second
                .map_or_else(|| "-".to_owned(), |b| eta_text((total - done) / b.max(1)));
            vec![
                line("Transferred", &format!("{} / {}", HumanBytes(s.bytes), HumanBytes(total))),
                line("Progress", &format!("[{bar}] {pct}%")),
                line("Speed", &speed),
                line("ETA", &eta),
            ]
        }
    }
}

pub(crate) fn eta_text(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    if h > 0 {
        format!("{h}h {m}m {s}s")
    } else if m > 0 {
        format!("{m}m {s}s")
    } else {
        format!("{s}s")
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use flux_core::progress::Snapshot;
    use std::time::Duration;

    fn s(d: u64, c: u64, r: u64, e: u64, b: u64) -> Snapshot {
        Snapshot { discovered: d, copied: c, resumed: r, errors: e, bytes: b, expected_bytes: None }
    }

    fn single(d: u64, c: u64, b: u64) -> Snapshot {
        Snapshot {
            discovered: d,
            copied: c,
            bytes: b,
            expected_bytes: Some(4096),
            ..Snapshot::default()
        }
    }

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn render_folder_frames() {
        let cases: Vec<(Snapshot, Duration, Vec<&str>)> = vec![
            (
                Snapshot::default(),
                secs(0),
                vec!["Discovered    0", "Processed     0", "Transferred   0 B", "Speed         -"],
            ),
            (
                s(123_481, 90_000, 8_412, 0, 4_294_967_296),
                secs(14),
                vec![
                    "Discovered    123,481",
                    "Processed     98,412",
                    "Transferred   4.00 GiB",
                    "Speed         292.57 MiB/s",
                    "Resumed       8,412",
                ],
            ),
            (
                s(10, 4, 0, 3, 2048),
                secs(2),
                vec![
                    "Discovered    10",
                    "Processed     4",
                    "Transferred   2.00 KiB",
                    "Speed         1.00 KiB/s",
                    "Errors        3",
                ],
            ),
            (
                s(5, 4, 3, 0, 0),
                secs(10),
                vec![
                    "Discovered    5",
                    "Processed     5",
                    "Transferred   0 B",
                    "Speed         -",
                    "Resumed       3",
                ],
            ),
            (
                s(1, 1, 0, 0, 1024),
                secs(0),
                vec![
                    "Discovered    1",
                    "Processed     1",
                    "Transferred   1.00 KiB",
                    "Speed         -",
                ],
            ),
            (
                s(2, 0, 1, 1, 0),
                secs(1),
                vec![
                    "Discovered    2",
                    "Processed     1",
                    "Transferred   0 B",
                    "Speed         -",
                    "Resumed       1",
                    "Errors        1",
                ],
            ),
        ];
        for (i, (snap, elapsed, want)) in cases.into_iter().enumerate() {
            assert_eq!(render(&snap, elapsed), want, "case {}", i + 1);
        }
    }

    #[test]
    fn render_single_file_frames() {
        let case2 = vec![
            "Transferred   1.00 KiB / 4.00 KiB",
            "Progress      [#####...............] 25%",
            "Speed         512 B/s",
            "ETA           6s",
        ];
        let cases: Vec<(Snapshot, Duration, Vec<&str>)> = vec![
            (
                single(0, 0, 0),
                secs(5),
                vec![
                    "Transferred   0 B / 4.00 KiB",
                    "Progress      [....................] 0%",
                    "Speed         -",
                    "ETA           -",
                ],
            ),
            (single(0, 0, 1024), secs(2), case2.clone()),
            (
                single(0, 0, 4096),
                secs(4),
                vec![
                    "Transferred   4.00 KiB / 4.00 KiB",
                    "Progress      [####################] 100%",
                    "Speed         1.00 KiB/s",
                    "ETA           0s",
                ],
            ),
            (
                single(0, 0, 8192),
                secs(4),
                vec![
                    "Transferred   8.00 KiB / 4.00 KiB",
                    "Progress      [####################] 100%",
                    "Speed         2.00 KiB/s",
                    "ETA           0s",
                ],
            ),
            (single(9, 9, 1024), secs(2), case2),
        ];
        for (i, (snap, elapsed, want)) in cases.into_iter().enumerate() {
            assert_eq!(render(&snap, elapsed), want, "case {}", i + 1);
        }
    }

    #[test]
    fn eta_text_formats() {
        let cases = [
            (0, "0s"),
            (3, "3s"),
            (59, "59s"),
            (60, "1m 0s"),
            (123, "2m 3s"),
            (3600, "1h 0m 0s"),
            (3723, "1h 2m 3s"),
        ];
        for (secs, want) in cases {
            assert_eq!(eta_text(secs), want, "{secs}");
        }
    }
}
