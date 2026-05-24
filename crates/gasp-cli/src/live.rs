//! Live multi-task display used by parallel commands (sync, fetch).
//!
//! In a TTY: shows a per-worker spinner while each task runs; on
//! completion the spinner is replaced by a result line printed above
//! the still-live overall progress bar. Behaves like the per-package
//! display in `cargo` / `pip` / `npm`.
//!
//! Outside a TTY (pipes, CI, tests): all of that auto-disables;
//! result lines stream to stdout as plain text and the overall bar
//! is hidden.

use std::time::Duration;

use console::Term;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};

pub struct LiveDisplay {
    mp: MultiProgress,
    main: ProgressBar,
    in_tty: bool,
}

impl LiveDisplay {
    /// `verb` is a short label used in the live status line, e.g.
    /// "syncing" or "fetching".
    pub fn new(total: u64, verb: &str) -> Self {
        let in_tty = Term::stderr().is_term();
        let mp = MultiProgress::new();
        let main = mp.add(ProgressBar::new(total));
        main.set_style(
            ProgressStyle::with_template(&format!("[{{pos}}/{{len}}] {verb} {{wide_msg}}"))
                .expect("static template")
                .progress_chars("=> "),
        );
        Self { mp, main, in_tty }
    }

    /// Begin a task. In TTY mode returns a handle whose spinner is
    /// shown until `finish_task` is called with it. In non-TTY mode
    /// returns `None` and there's no visible "in-flight" indicator.
    pub fn start_task(&self, name: &str) -> Option<ProgressBar> {
        if !self.in_tty {
            return None;
        }
        let s = self.mp.add(ProgressBar::new_spinner());
        s.set_style(
            ProgressStyle::with_template("  {spinner:.cyan} {msg}").expect("static template"),
        );
        s.set_message(name.to_string());
        s.enable_steady_tick(Duration::from_millis(80));
        Some(s)
    }

    /// Finalize a task: in TTY mode clears its spinner and prints
    /// `result_line` above the live region; in non-TTY mode just
    /// emits the line on stdout. Either way, advances the overall
    /// progress bar by one.
    pub fn finish_task(&self, spinner: Option<ProgressBar>, result_line: &str) {
        if let Some(s) = spinner {
            s.finish_and_clear();
            // mp.println goes to stderr (the draw target), above the
            // overall bar. We mirror to stdout below so the line is
            // pipeable AND visible in the terminal.
            self.mp.println(result_line).ok();
            // Plain println would interleave with the live region;
            // mp.println handles redraw. No second print needed when
            // we have the spinner.
        } else {
            println!("{result_line}");
        }
        self.main.inc(1);
    }

    /// Tear down the live region (must be called before final summary
    /// output to avoid leaving the bar dangling on stderr).
    pub fn finish(self) {
        self.main.finish_and_clear();
    }
}
