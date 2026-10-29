// Copyright (c) 2026 Costas Kirgoussios
//
// This software is licensed under the PolyForm Noncommercial License 1.0.0.
// You may obtain a copy of the License at:
// https://polyformproject.org/licenses/noncommercial/1.0.0/

//! The live display: an `indicatif` bar redrawn from a sampler thread.

use crate::render::render;
use flux_core::progress::Progress;
use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const SAMPLE_INTERVAL: Duration = Duration::from_millis(250);

/// A live progress display drawn to a terminal-like target.
pub struct Display {
    bar: ProgressBar,
    progress: Arc<Progress>,
    started: Instant,
    stop: Arc<AtomicBool>,
    sampler: Option<JoinHandle<()>>,
}

fn draw(bar: &ProgressBar, progress: &Progress, started: Instant) {
    bar.set_message(render(&progress.snapshot(), started.elapsed()).join("\n"));
    bar.force_draw();
}

impl Display {
    /// Start drawing; `None` when the target is hidden or the sampler thread
    /// cannot be spawned.
    #[must_use]
    pub fn start(progress: Arc<Progress>, target: ProgressDrawTarget) -> Option<Display> {
        if target.is_hidden() {
            return None;
        }
        let bar = ProgressBar::with_draw_target(None, target);
        bar.set_style(ProgressStyle::with_template("{msg}").ok()?);
        let started = Instant::now();
        draw(&bar, &progress, started);
        let stop = Arc::new(AtomicBool::new(false));
        let sampler = std::thread::Builder::new()
            .name("flux-progress".to_owned())
            .spawn({
                let (bar, progress, stop) = (bar.clone(), Arc::clone(&progress), Arc::clone(&stop));
                move || loop {
                    std::thread::park_timeout(SAMPLE_INTERVAL);
                    if stop.load(Ordering::Acquire) {
                        break;
                    }
                    draw(&bar, &progress, started);
                }
            })
            .ok()?;
        Some(Display { bar, progress, started, stop, sampler: Some(sampler) })
    }

    /// Render now and force a draw.
    pub fn refresh(&self) {
        draw(&self.bar, &self.progress, self.started);
    }

    /// Print one record line above the bar.
    pub fn println(&self, line: &str) {
        self.bar.println(line);
    }

    /// Stop the sampler and clear the bar.
    pub fn finish(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.sampler.take() {
            handle.thread().unpark();
            let _ = handle.join();
            self.bar.finish_and_clear();
        }
    }
}

impl Drop for Display {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_core::progress::Progress;
    use indicatif::InMemoryTerm;

    fn target(term: &InMemoryTerm) -> ProgressDrawTarget {
        ProgressDrawTarget::term_like_with_hz(Box::new(term.clone()), 255)
    }

    #[test]
    fn start_draws_the_folder_frame() {
        let term = InMemoryTerm::new(20, 80);
        let _d = Display::start(Progress::new(), target(&term)).unwrap();
        assert!(term.contents().contains("Discovered"), "{}", term.contents());
    }

    #[test]
    fn refresh_shows_new_counts() {
        let term = InMemoryTerm::new(20, 80);
        let p = Progress::new();
        let d = Display::start(p.clone(), target(&term)).unwrap();
        p.add_discovered();
        p.add_discovered();
        p.add_discovered();
        d.refresh();
        assert!(term.contents().contains("Discovered    3"), "{}", term.contents());
    }

    #[test]
    fn println_puts_the_line_above_the_bar() {
        let term = InMemoryTerm::new(20, 80);
        let d = Display::start(Progress::new(), target(&term)).unwrap();
        d.println("a record line");
        let c = term.contents();
        let line = c.find("a record line").expect("line present");
        let bar = c.find("Discovered").expect("bar present");
        assert!(line < bar, "{c}");
    }

    #[test]
    fn finish_clears_the_bar() {
        let term = InMemoryTerm::new(20, 80);
        let d = Display::start(Progress::new(), target(&term)).unwrap();
        d.finish();
        assert!(!term.contents().contains("Discovered"), "{}", term.contents());
    }

    #[test]
    fn a_hidden_target_starts_no_display() {
        assert!(Display::start(Progress::new(), ProgressDrawTarget::hidden()).is_none());
    }
}
