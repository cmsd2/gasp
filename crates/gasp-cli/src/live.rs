//! Live multi-task display used by parallel commands (sync, fetch).
//!
//! Pre-allocates one spinner per job (in submission order) so each
//! worker has its own fixed position. The cargo / pip pattern:
//!
//!   clone  alpha ... ok          <- finished in its slot
//!     ⠴   beta                   <- running in its slot
//!     ·   delta                  <- still queued
//!     ·   zebra
//!   [1/4] syncing
//!
//! As each worker takes its slot it animates, then on completion the
//! slot is replaced with the final result line. Output order is
//! therefore the submission order — alphabetical in our case.
//!
//! Outside a TTY the bars are hidden; finalize_with_result is a
//! no-op visually, and the caller is expected to print the buffered
//! result lines after all workers join.

use std::time::Duration;

use console::Term;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};

pub struct LiveDisplay {
    mp: MultiProgress,
    slots: Vec<ProgressBar>,
    overall: ProgressBar,
    in_tty: bool,
}

impl LiveDisplay {
    /// `names` are listed in submission order — typically alphabetic.
    /// One spinner is pre-allocated per name, in that order, so the
    /// vertical layout matches the order results will land in.
    /// `verb` is a short label for the overall bar (e.g. "syncing").
    pub fn new(names: &[&str], verb: &str) -> Self {
        let in_tty = Term::stderr().is_term();
        let mp = MultiProgress::new();
        let slots = names
            .iter()
            .map(|name| {
                let pb = mp.add(ProgressBar::new_spinner());
                pb.set_style(
                    ProgressStyle::with_template("  {spinner:.dim} {msg:.dim}")
                        .expect("static template"),
                );
                pb.set_message((*name).to_string());
                pb
            })
            .collect();
        let overall = mp.add(ProgressBar::new(names.len() as u64));
        overall.set_style(
            ProgressStyle::with_template(&format!("[{{pos}}/{{len}}] {verb} {{wide_msg}}"))
                .expect("static template")
                .progress_chars("=> "),
        );
        Self {
            mp,
            slots,
            overall,
            in_tty,
        }
    }

    /// Mark slot `i` as actively being processed. Replaces the dim
    /// "queued" appearance with an animated cyan spinner.
    pub fn mark_running(&self, i: usize, name: &str) {
        if !self.in_tty {
            return;
        }
        let pb = &self.slots[i];
        pb.set_style(
            ProgressStyle::with_template("  {spinner:.cyan} {msg}").expect("static template"),
        );
        pb.set_message(name.to_string());
        pb.enable_steady_tick(Duration::from_millis(80));
        self.overall.set_message(name.to_string());
    }

    /// Finalize slot `i` with the worker's result line. The spinner
    /// is replaced in place with the line; the slot remains visible
    /// as part of the scrollback, in its submitted position.
    pub fn finish_slot(&self, i: usize, result_line: &str) {
        if self.in_tty {
            let pb = &self.slots[i];
            pb.disable_steady_tick();
            pb.set_style(ProgressStyle::with_template("{msg}").expect("static template"));
            pb.finish_with_message(result_line.to_string());
        }
        self.overall.inc(1);
    }

    /// Tear down the overall bar. Slot bars stay in place — they're
    /// the visible record of the run.
    pub fn finish(self) {
        self.overall.finish_and_clear();
        // Keep `mp` alive until here so its draw thread isn't dropped
        // before the finished slot bars get their final render.
        drop(self.mp);
    }

    /// True if the display is hidden (non-TTY). Callers use this to
    /// decide whether to print result lines on stdout after the run.
    pub fn is_hidden(&self) -> bool {
        !self.in_tty
    }
}
