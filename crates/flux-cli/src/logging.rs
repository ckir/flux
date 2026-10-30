// Copyright (c) 2026 Costas Kirgoussios
//
// This software is licensed under the PolyForm Noncommercial License 1.0.0.
// You may obtain a copy of the License at:
// https://polyformproject.org/licenses/noncommercial/1.0.0/

//! Rendering the engine's `tracing` events as terminal lines (logging design 5.1, 5.2, 5.4-5.6).
//!
//! One subscriber, installed once by `main`: it keeps only targets starting with `flux`, at the level
//! the verbosity asks for, formats each event as ONE line (no timestamp, no colour), escapes every
//! field value and message, and writes the line through [`LineWriter`], which hands it to the live
//! display (above the bar) when one is registered and to stderr otherwise.
//!
//! The shipped binary aborts on a panic, so nothing here may panic: no `unwrap`, `expect`, indexing
//! or `panic!`, and I/O errors are discarded.

use crate::verbosity::Verbosity;
use std::fmt::{self, Write as _};
use std::io::{self, Write};
use std::sync::{Arc, Mutex};
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::filter::{LevelFilter, filter_fn};
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::{FmtContext, FormatEvent, FormatFields, MakeWriter};
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::{Layer as _, Registry};

type Sink = Arc<dyn Fn(&str) + Send + Sync>;

/// Where whole lines go while a live display runs; `None` means stderr.
static SINK: Mutex<Option<Sink>> = Mutex::new(None);

/// Route whole log lines to `sink` (the live display), or back to stderr with `None`.
pub fn set_display_sink(sink: Option<Sink>) {
    if let Ok(mut slot) = SINK.lock() {
        *slot = sink;
    }
}

/// Whether a display sink is registered (a poisoned lock counts as none).
#[must_use]
pub fn has_display_sink() -> bool {
    SINK.lock().map(|g| g.is_some()).unwrap_or(false)
}

/// `s` with every control character (ESC, CR, LF, DEL, ...) written as `\u{..}`; printable
/// Unicode is kept.
#[must_use]
pub fn escape_control(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_control() {
            let _ = write!(out, "\\u{{{:x}}}", u32::from(c));
        } else {
            out.push(c);
        }
    }
    out
}

/// One `flux::op` line: `{Verb:>10} {path}`, then ` -> {to}` and ` ({size})` when present.
#[must_use]
pub fn render_op(action: &str, path: &str, bytes: Option<u64>, to: Option<&str>) -> String {
    let mut chars = action.chars();
    let verb: String = match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    };
    let mut line = format!("{verb:>10} {path}");
    if let Some(to) = to {
        let _ = write!(line, " -> {to}");
    }
    if let Some(bytes) = bytes {
        let _ = write!(line, " ({})", crate::report::human_bytes(bytes));
    }
    line
}

/// Write `line` and a newline to `w`, discarding any error.
fn write_line_to(w: &mut impl Write, line: &str) {
    let _ = w.write_all(line.as_bytes());
    let _ = w.write_all(b"\n");
}

fn stderr_line(line: &str) {
    write_line_to(&mut io::stderr().lock(), line);
}

/// Deliver one whole line to the sink, or to `fallback` when none is registered. The lock is held
/// only for the clone of the `Arc`; the sink runs after it is released.
fn deliver(line: &str, fallback: fn(&str)) {
    let sink = SINK.lock().ok().and_then(|g| g.clone());
    match sink {
        Some(sink) => sink(line),
        None => fallback(line),
    }
}

/// The `MakeWriter` of the log subscriber.
#[derive(Clone, Copy)]
pub struct LineWriter {
    fallback: fn(&str),
}

impl LineWriter {
    /// Lines go to the display sink, else to stderr.
    #[must_use]
    pub fn new() -> LineWriter {
        LineWriter { fallback: stderr_line }
    }

    #[cfg(test)]
    fn with_fallback(fallback: fn(&str)) -> LineWriter {
        LineWriter { fallback }
    }
}

impl Default for LineWriter {
    fn default() -> Self {
        LineWriter::new()
    }
}

impl<'a> MakeWriter<'a> for LineWriter {
    type Writer = LineBuffer;

    fn make_writer(&'a self) -> LineBuffer {
        LineBuffer { buf: Vec::new(), fallback: self.fallback }
    }
}

/// Buffers bytes until a newline and delivers each whole line; a trailing partial line goes out on
/// flush or drop.
pub struct LineBuffer {
    buf: Vec<u8>,
    fallback: fn(&str),
}

impl LineBuffer {
    fn emit(&self, bytes: &[u8]) {
        deliver(&String::from_utf8_lossy(bytes), self.fallback);
    }
}

impl Write for LineBuffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(bytes);
        while let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
            let mut line: Vec<u8> = self.buf.drain(..=pos).collect();
            line.pop();
            self.emit(&line);
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if !self.buf.is_empty() {
            let rest = std::mem::take(&mut self.buf);
            self.emit(&rest);
        }
        Ok(())
    }
}

impl Drop for LineBuffer {
    fn drop(&mut self) {
        let _ = self.flush();
    }
}

/// Collects an event's fields in declaration order, as text.
#[derive(Default)]
struct Fields {
    message: Option<String>,
    fields: Vec<(&'static str, String)>,
}

impl Fields {
    fn put(&mut self, field: &Field, text: String) {
        if field.name() == "message" {
            self.message = Some(text);
        } else {
            self.fields.push((field.name(), text));
        }
    }

    fn get(&self, name: &str) -> Option<&str> {
        self.fields.iter().find(|(n, _)| *n == name).map(|(_, v)| v.as_str())
    }
}

impl Visit for Fields {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.put(field, value.to_owned());
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.put(field, value.to_string());
    }
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.put(field, value.to_string());
    }
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.put(field, value.to_string());
    }
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.put(field, format!("{value:?}"));
    }
}

/// The event formatter: `flux::op` events as [`render_op`] lines, everything else as
/// `{level:>5} {module}: {message} key=value ...`. Every value is escaped.
struct LineFormat;

impl<S, N> FormatEvent<S, N> for LineFormat
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        _ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result {
        let meta = event.metadata();
        let mut f = Fields::default();
        event.record(&mut f);
        let line = if meta.target() == "flux::op" {
            let bytes = f.get("bytes").and_then(|b| b.parse::<u64>().ok());
            let path = escape_control(f.get("path").unwrap_or_default());
            let to = f.get("to").map(escape_control);
            render_op(
                &escape_control(f.get("action").unwrap_or_default()),
                &path,
                bytes,
                to.as_deref(),
            )
        } else {
            let module = meta.target().rsplit("::").next().unwrap_or_else(|| meta.target());
            let level = meta.level().as_str().to_lowercase();
            let mut line = format!("{level:>5} {module}:");
            if let Some(message) = &f.message {
                let _ = write!(line, " {}", escape_control(message));
            }
            for (name, value) in &f.fields {
                let _ = write!(line, " {name}={}", escape_control(value));
            }
            line
        };
        writer.write_str(&line)?;
        writer.write_char('\n')
    }
}

fn max_level(v: Verbosity) -> Level {
    if v.quiet {
        return Level::WARN;
    }
    match v.level {
        0 => Level::WARN,
        1 => Level::INFO,
        2 => Level::DEBUG,
        _ => Level::TRACE,
    }
}

/// The subscriber `install` sets, over any writer (tests use a scoped dispatcher over it).
pub fn build<W>(v: Verbosity, writer: W) -> impl Subscriber + Send + Sync
where
    W: for<'w> MakeWriter<'w> + Send + Sync + 'static,
{
    let max = max_level(v);
    let filter = filter_fn(move |m| m.target().starts_with("flux") && *m.level() <= max)
        .with_max_level_hint(LevelFilter::from_level(max));
    let layer = tracing_subscriber::fmt::layer()
        .event_format(LineFormat)
        .with_writer(writer)
        .with_filter(filter);
    Registry::default().with(layer)
}

/// Install the global subscriber; a second call (or another global subscriber) is ignored.
pub fn install(v: Verbosity) {
    let _ = tracing::subscriber::set_global_default(build(v, LineWriter::new()));
}

#[cfg(test)]
mod tests {
    // The tests that touch the process-global display sink (and `install`) assume `cargo nextest`'s
    // one-process-per-test isolation and would race under plain `cargo test` (the justfile and CI run nextest).
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    static FALLBACK: Mutex<Vec<String>> = Mutex::new(Vec::new());

    fn capture_fallback(line: &str) {
        if let Ok(mut g) = FALLBACK.lock() {
            g.push(line.to_owned());
        }
    }

    fn collecting_sink() -> (Arc<Mutex<Vec<String>>>, Sink) {
        let got = Arc::new(Mutex::new(Vec::new()));
        let g2 = Arc::clone(&got);
        let sink: Sink = Arc::new(move |l: &str| {
            if let Ok(mut g) = g2.lock() {
                g.push(l.to_owned());
            }
        });
        (got, sink)
    }

    /// A `MakeWriter` collecting whole bytes, for formatter tests that do not touch the global sink.
    #[derive(Clone, Default)]
    struct Mem(Arc<Mutex<Vec<u8>>>);

    struct MemW(Arc<Mutex<Vec<u8>>>);

    impl Write for MemW {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            if let Ok(mut g) = self.0.lock() {
                g.extend_from_slice(b);
            }
            Ok(b.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for Mem {
        type Writer = MemW;
        fn make_writer(&'a self) -> MemW {
            MemW(Arc::clone(&self.0))
        }
    }

    impl Mem {
        fn text(&self) -> String {
            self.0.lock().map(|g| String::from_utf8_lossy(&g).into_owned()).unwrap_or_default()
        }
    }

    fn v(level: u8, quiet: bool) -> Verbosity {
        Verbosity { level, quiet }
    }

    #[test]
    fn render_op_matches_the_design_table() {
        assert_eq!(render_op("created", "sub", None, None), "   Created sub");
        assert_eq!(render_op("copied", "a", Some(2), None), "    Copied a (2 B)");
        assert_eq!(render_op("linked", "lnk", None, Some("a")), "    Linked lnk -> a");
        assert_eq!(render_op("hardlinked", "b", None, Some("a")), "Hardlinked b -> a");
        assert_eq!(render_op("replaced", "a", Some(2), None), "  Replaced a (2 B)");
        assert_eq!(render_op("resumed", "a", Some(2), None), "   Resumed a (2 B)");
        assert_eq!(render_op("skipped", "a", None, None), "   Skipped a");
        assert_eq!(render_op("removed", ".flux-x", None, None), "   Removed .flux-x");
    }

    #[test]
    fn escape_control_keeps_printable_and_escapes_the_rest() {
        assert_eq!(escape_control("a\x1b[2Kb"), "a\\u{1b}[2Kb");
        assert_eq!(escape_control("a\rb\nc\x7fd"), "a\\u{d}b\\u{a}c\\u{7f}d");
        assert_eq!(escape_control("héllo ✓ 日本"), "héllo ✓ 日本");
        assert_eq!(escape_control(""), "");
    }

    #[test]
    fn line_writer_hands_whole_lines_to_the_sink() {
        let (got, sink) = collecting_sink();
        set_display_sink(Some(sink));
        let mut w = LineWriter::new().make_writer();
        assert!(w.write_all(b"    Copied a").is_ok());
        assert!(got.lock().map(|g| g.is_empty()).unwrap_or(false));
        assert!(w.write_all(b" (2 B)\nnext").is_ok());
        assert_eq!(got.lock().map(|g| g.clone()).unwrap_or_default(), ["    Copied a (2 B)"]);
        drop(w);
        set_display_sink(None);
        assert_eq!(
            got.lock().map(|g| g.clone()).unwrap_or_default(),
            ["    Copied a (2 B)", "next"]
        );
    }

    struct Failing;

    impl Write for Failing {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }
        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }
    }

    #[test]
    fn line_writer_to_a_closed_stderr_does_not_panic() {
        write_line_to(&mut Failing, "a line");
        write_line_to(&mut Failing, "x\u{1b}y");
        // No sink: the fallback path runs with a writer that always fails.
        set_display_sink(None);
        let mut w = LineWriter::with_fallback(|l| write_line_to(&mut Failing, l)).make_writer();
        assert!(w.write_all(b"one\ntwo\npartial").is_ok());
        drop(w);
    }

    #[test]
    fn sink_swap_during_finish_loses_no_line() {
        if let Ok(mut g) = FALLBACK.lock() {
            g.clear();
        }
        let (got, sink) = collecting_sink();
        let done = Arc::new(AtomicUsize::new(0));
        let d2 = Arc::clone(&done);
        let writer = std::thread::spawn(move || {
            let make = LineWriter::with_fallback(capture_fallback);
            for i in 0..1000 {
                let mut w = make.make_writer();
                let _ = w.write_all(format!("line {i}\n").as_bytes());
            }
            d2.store(1, Ordering::Release);
        });
        while done.load(Ordering::Acquire) == 0 {
            set_display_sink(Some(Arc::clone(&sink)));
            set_display_sink(None);
        }
        let _ = writer.join();
        set_display_sink(None);
        let mut all: Vec<String> = got.lock().map(|g| g.clone()).unwrap_or_default();
        all.extend(FALLBACK.lock().map(|g| g.clone()).unwrap_or_default());
        all.sort();
        let mut want: Vec<String> = (0..1000).map(|i| format!("line {i}")).collect();
        want.sort();
        assert_eq!(all, want);
    }

    #[test]
    fn debug_and_trace_lines_carry_the_level_and_the_path() {
        let mem = Mem::default();
        tracing::subscriber::with_default(build(v(2, false), mem.clone()), || {
            tracing::debug!(target: "flux_core::tree", path = "a/b", plan = "new", "decided");
            tracing::trace!(target: "flux_core::tree", path = "c", "chunk");
            tracing::warn!(target: "flux_core::copy", "slow\x1b[2K");
            tracing::debug!(target: "other_crate", path = "z", "not ours");
        });
        assert_eq!(
            mem.text(),
            "debug tree: decided path=a/b plan=new\n warn copy: slow\\u{1b}[2K\n"
        );
        let mem = Mem::default();
        tracing::subscriber::with_default(build(v(3, false), mem.clone()), || {
            tracing::trace!(target: "flux_core::tree", path = "c\n", status = "created");
        });
        assert_eq!(mem.text(), "trace tree: path=c\\u{a} status=created\n");
        let mem = Mem::default();
        tracing::subscriber::with_default(build(v(1, false), mem.clone()), || {
            tracing::debug!(target: "flux_core::tree", path = "a");
        });
        assert_eq!(mem.text(), "");
    }

    #[test]
    fn a_hostile_name_reaches_the_sink_escaped() {
        let (got, sink) = collecting_sink();
        set_display_sink(Some(sink));
        tracing::subscriber::with_default(build(v(1, false), LineWriter::new()), || {
            flux_core::oplog::emit(
                flux_core::oplog::Verb::Copied,
                std::path::Path::new("x\x1b[2Ky"),
                Some(2),
                None,
            );
        });
        set_display_sink(None);
        let lines = got.lock().map(|g| g.clone()).unwrap_or_default();
        assert_eq!(lines, ["    Copied x\\u{1b}[2Ky (2 B)"]);
        assert!(!lines.iter().any(|l| l.contains('\x1b')));
    }

    #[test]
    fn at_default_and_quiet_levels_op_events_are_filtered() {
        for verbosity in [v(0, false), v(0, true)] {
            let mem = Mem::default();
            tracing::subscriber::with_default(build(verbosity, mem.clone()), || {
                flux_core::oplog::emit(
                    flux_core::oplog::Verb::Copied,
                    std::path::Path::new("a"),
                    Some(1),
                    None,
                );
                tracing::warn!(target: "flux_core::tree", "heads up");
            });
            assert_eq!(mem.text(), " warn tree: heads up\n", "{verbosity:?}");
        }
        let mem = Mem::default();
        tracing::subscriber::with_default(build(v(1, false), mem.clone()), || {
            flux_core::oplog::emit(
                flux_core::oplog::Verb::Linked,
                std::path::Path::new("l"),
                None,
                Some(std::path::Path::new("a")),
            );
        });
        assert_eq!(mem.text(), "    Linked l -> a\n");
    }

    #[test]
    fn the_formatter_escapes_a_raw_control_character_in_an_op_path() {
        // A hand-built event: its `path` is a plain string, not an `EscapedPath`, so only the formatter can escape it.
        let mem = Mem::default();
        tracing::subscriber::with_default(build(v(1, false), mem.clone()), || {
            tracing::info!(target: "flux::op", action = "copied", path = "x\u{1b}[2Ky");
        });
        assert_eq!(mem.text(), "    Copied x\\u{1b}[2Ky\n");
        assert!(!mem.text().contains('\x1b'));
    }

    #[test]
    fn the_formatter_escapes_a_raw_control_character_in_an_op_link_target() {
        let mem = Mem::default();
        tracing::subscriber::with_default(build(v(1, false), mem.clone()), || {
            tracing::info!(target: "flux::op", action = "linked", path = "l", to = "t\u{1b}x");
        });
        assert_eq!(mem.text(), "    Linked l -> t\\u{1b}x\n");
        assert!(!mem.text().contains('\x1b'));
    }

    #[test]
    fn a_write_with_several_lines_delivers_each_whole_line_once_in_order() {
        let (got, sink) = collecting_sink();
        set_display_sink(Some(sink));
        let mut w = LineWriter::new().make_writer();
        assert!(w.write_all(b"one\ntwo\nthree\n").is_ok());
        assert_eq!(got.lock().map(|g| g.clone()).unwrap_or_default(), ["one", "two", "three"]);
        // A line split across two writes goes out once, when its newline arrives.
        assert!(w.write_all(b"par").is_ok());
        assert_eq!(got.lock().map(|g| g.len()).unwrap_or_default(), 3);
        assert!(w.write_all(b"tial\n").is_ok());
        drop(w);
        set_display_sink(None);
        assert_eq!(
            got.lock().map(|g| g.clone()).unwrap_or_default(),
            ["one", "two", "three", "partial"]
        );
    }

    #[test]
    fn install_twice_does_not_panic() {
        install(v(0, false));
        install(v(3, false));
    }
}
