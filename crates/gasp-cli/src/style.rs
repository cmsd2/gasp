//! TTY-aware colouring helpers. Wraps `console::style` so each
//! semantic role (ok / failed / repo name etc.) has one definition.
//! `console` auto-detects whether stdout is a terminal; output goes
//! through plain when piped or captured (so tests + CI logs stay
//! unstyled).

use std::fmt::Display;

use console::{Style, StyledObject};

pub fn ok<T: Display>(t: T) -> StyledObject<T> {
    Style::new().green().apply_to(t)
}

pub fn failed<T: Display>(t: T) -> StyledObject<T> {
    Style::new().red().bold().apply_to(t)
}

pub fn skipped<T: Display>(t: T) -> StyledObject<T> {
    Style::new().yellow().apply_to(t)
}

/// Verb-like labels (clone / ff / reset / rebase / fetch ...).
pub fn verb<T: Display>(t: T) -> StyledObject<T> {
    Style::new().cyan().apply_to(t)
}

pub fn name<T: Display>(t: T) -> StyledObject<T> {
    Style::new().bold().apply_to(t)
}

pub fn dim<T: Display>(t: T) -> StyledObject<T> {
    Style::new().dim().apply_to(t)
}

/// Colour the STATE column in `gasp status` by severity.
pub fn state(s: &str) -> StyledObject<&str> {
    match s {
        "clean" => Style::new().green().apply_to(s),
        "behind" | "ahead" => Style::new().yellow().apply_to(s),
        "dirty" | "diverged" | "missing" | "not-git" | "unknown" => Style::new().red().apply_to(s),
        _ => Style::new().apply_to(s),
    }
}
