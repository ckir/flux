// Copyright (c) 2026 Costas Kirgoussios
//
// This software is licensed under the PolyForm Noncommercial License 1.0.0.
// You may obtain a copy of the License at:
// https://polyformproject.org/licenses/noncommercial/1.0.0/

//! Per-operation tracing events (logging design 5.2, 5.6).
//!
//! The engine announces each file, directory, link and removal as ONE `INFO` event with target
//! [`TARGET`]. Nothing here renders anything: a subscriber (the CLI's, Task 5) decides whether
//! and how to print. With no subscriber installed, or one that disables the target, an event
//! costs a level check - the escaped path is built only when an enabled subscriber formats it.

use std::fmt;
use std::path::Path;

/// The `tracing` target every operation event carries.
pub const TARGET: &str = "flux::op";

/// What happened to one entry. `Copied` is the default so a fresh `TreeOutcome` carries the
/// plain-copy hint; a path that is not a plain copy sets another verb just before the shared
/// completion code runs.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    Created,
    #[default]
    Copied,
    Linked,
    Hardlinked,
    Replaced,
    Resumed,
    Skipped,
    Removed,
}

impl Verb {
    /// The lowercase action word an event carries in its `action` field.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Verb::Created => "created",
            Verb::Copied => "copied",
            Verb::Linked => "linked",
            Verb::Hardlinked => "hardlinked",
            Verb::Replaced => "replaced",
            Verb::Resumed => "resumed",
            Verb::Skipped => "skipped",
            Verb::Removed => "removed",
        }
    }
}

/// `Display` for a path with control characters and (on Unix) non-UTF-8 bytes escaped, so a
/// hostile file name cannot drive the terminal. Lazy: nothing is built until it is formatted.
pub struct EscapedPath<'a>(pub &'a Path);

impl fmt::Display for EscapedPath<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_escaped(self.0, f)
    }
}

fn write_char(c: char, out: &mut impl fmt::Write) -> fmt::Result {
    if c.is_control() { write!(out, "\\u{{{:x}}}", u32::from(c)) } else { out.write_char(c) }
}

#[cfg(unix)]
fn write_escaped(p: &Path, out: &mut impl fmt::Write) -> fmt::Result {
    use std::os::unix::ffi::OsStrExt;
    for chunk in p.as_os_str().as_bytes().utf8_chunks() {
        for c in chunk.valid().chars() {
            write_char(c, out)?;
        }
        for b in chunk.invalid() {
            write!(out, "\\x{b:02x}")?;
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn write_escaped(p: &Path, out: &mut impl fmt::Write) -> fmt::Result {
    for c in p.to_string_lossy().chars() {
        write_char(if c == '\\' { '/' } else { c }, out)?;
    }
    Ok(())
}

/// `p` with control characters (`\x1b`, `\r`, `\n`, ...) written as `\u{..}` and, on Unix,
/// non-UTF-8 bytes as `\xNN`. Printable Unicode is kept; separators are `/` on every platform.
#[must_use]
pub fn escape_path(p: &Path) -> String {
    let mut s = String::new();
    // Writing to a String cannot fail.
    let _ = write_escaped(p, &mut s);
    s
}

/// Announce one operation. `path` is relative to the destination root; `to` is a link target
/// or hardlink anchor.
pub fn emit(verb: Verb, path: &Path, bytes: Option<u64>, to: Option<&Path>) {
    tracing::info!(
        target: TARGET,
        action = verb.as_str(),
        path = %EscapedPath(path),
        bytes = bytes,
        to = to.map(|t| tracing::field::display(EscapedPath(t))),
    );
}

/// Whether any subscriber wants `flux::op` events. Every emission site builds its arguments only behind this, so
/// with logging off nothing is allocated for an event (design anti-goal A4).
#[inline]
#[must_use]
pub fn enabled() -> bool {
    tracing::enabled!(target: TARGET, tracing::Level::INFO)
}

#[cfg(test)]
thread_local! {
    /// How many times an argument-building closure ran on this thread (test-only proof of the guard).
    static BUILT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Test-only: the number of argument builds on this thread so far.
#[cfg(test)]
pub(crate) fn built() -> usize {
    BUILT.with(std::cell::Cell::get)
}

/// `emit` for a site whose arguments cost something to build: `build` runs only when [`enabled`].
#[inline]
pub fn emit_lazy(
    verb: Verb,
    build: impl FnOnce() -> (std::path::PathBuf, Option<u64>, Option<std::path::PathBuf>),
) {
    if enabled() {
        #[cfg(test)]
        BUILT.with(|c| c.set(c.get() + 1));
        let (path, bytes, to) = build();
        emit(verb, &path, bytes, to.as_deref());
    }
}

/// Test support: capture the events a closure emits.
#[cfg(test)]
pub mod testing {
    use std::fmt;
    use std::sync::{Arc, Mutex};
    use tracing::field::{Field, Visit};
    use tracing::span::{Attributes, Id, Record};
    use tracing::{Event, Metadata, Subscriber};

    /// One captured operation event.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct OpEvent {
        pub action: String,
        pub path: String,
        pub bytes: Option<u64>,
        pub to: Option<String>,
    }

    #[derive(Default)]
    struct Fields {
        action: String,
        path: String,
        bytes: Option<u64>,
        to: Option<String>,
    }

    impl Visit for Fields {
        fn record_u64(&mut self, field: &Field, value: u64) {
            if field.name() == "bytes" {
                self.bytes = Some(value);
            }
        }
        fn record_str(&mut self, field: &Field, value: &str) {
            match field.name() {
                "action" => self.action = value.to_string(),
                "path" => self.path = value.to_string(),
                "to" => self.to = Some(value.to_string()),
                _ => {}
            }
        }
        fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
            // A `%` display field arrives as a Debug of the Display wrapper, which prints the Display text.
            let text = format!("{value:?}");
            match field.name() {
                "action" => self.action = text,
                "path" => self.path = text,
                "to" => self.to = Some(text),
                _ => {}
            }
        }
    }

    struct Collector {
        events: Arc<Mutex<Vec<(String, OpEvent)>>>,
    }

    impl Subscriber for Collector {
        fn enabled(&self, _: &Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, _: &Attributes<'_>) -> Id {
            Id::from_u64(1)
        }
        fn record(&self, _: &Id, _: &Record<'_>) {}
        fn record_follows_from(&self, _: &Id, _: &Id) {}
        fn enter(&self, _: &Id) {}
        fn exit(&self, _: &Id) {}
        fn event(&self, event: &Event<'_>) {
            let mut f = Fields::default();
            event.record(&mut f);
            let level = event.metadata().level().to_string();
            let tagged = format!("{level} {}", event.metadata().target());
            self.events.lock().unwrap().push((
                tagged,
                OpEvent { action: f.action, path: f.path, bytes: f.bytes, to: f.to },
            ));
        }
    }

    fn run<R>(f: impl FnOnce() -> R) -> (R, Vec<(String, OpEvent)>) {
        let events = Arc::new(Mutex::new(Vec::new()));
        let sub = Collector { events: Arc::clone(&events) };
        let r = tracing::subscriber::with_default(sub, f);
        let got = std::mem::take(&mut *events.lock().unwrap());
        (r, got)
    }

    /// Run `f` with a capturing subscriber installed on this thread; return its result and the
    /// `flux::op` events it emitted, in order. Other targets are ignored.
    pub fn capture<R>(f: impl FnOnce() -> R) -> (R, Vec<OpEvent>) {
        let (r, all) = run(f);
        let ops = all
            .into_iter()
            .filter(|(tag, _)| tag.ends_with(super::TARGET))
            .map(|(_, e)| e)
            .collect();
        (r, ops)
    }

    /// As [`capture`], but also returns each event's `"LEVEL target"` tag (for the one-INFO-event test).
    pub fn capture_tagged<R>(f: impl FnOnce() -> R) -> (R, Vec<(String, OpEvent)>) {
        run(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn escape_path_escapes_controls_and_non_utf8() {
        assert_eq!(escape_path(Path::new("a\x1b[2K")), "a\\u{1b}[2K");
        assert_eq!(escape_path(Path::new("a\nb")), "a\\u{a}b");
        assert_eq!(escape_path(Path::new("a\rb")), "a\\u{d}b");
        assert_eq!(escape_path(Path::new("é/x")), "é/x");
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            let p = PathBuf::from(std::ffi::OsStr::from_bytes(b"a\xffb"));
            assert_eq!(escape_path(&p), "a\\xffb");
        }
    }

    #[test]
    fn emit_lazy_builds_nothing_without_a_subscriber() {
        let before = built();
        emit_lazy(Verb::Copied, || panic!("built with logging off"));
        assert!(!enabled());
        assert_eq!(built(), before);
        let (_, evs) = testing::capture(|| {
            emit_lazy(Verb::Copied, || (PathBuf::from("f"), Some(1), None));
        });
        assert_eq!(evs.len(), 1);
        assert_eq!(built(), before + 1);
    }

    #[test]
    fn emit_is_one_info_event_with_target_flux_op() {
        let (_, evs) = testing::capture_tagged(|| {
            emit(Verb::Linked, Path::new("d/l\n"), Some(7), Some(Path::new("t")));
        });
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].0, "INFO flux::op");
        assert_eq!(
            evs[0].1,
            testing::OpEvent {
                action: "linked".into(),
                path: "d/l\\u{a}".into(),
                bytes: Some(7),
                to: Some("t".into()),
            }
        );
        let (_, evs) = testing::capture(|| emit(Verb::Copied, Path::new("f"), None, None));
        assert_eq!(evs[0].bytes, None);
        assert_eq!(evs[0].to, None);
    }
}
