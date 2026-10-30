use std::{
    env,
    io::{self, IsTerminal},
    path::Path,
    time::Duration,
};

use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};

use crate::font::PatchedFont;

pub struct Ui {
    quiet: bool,
    animated: bool,
    color: bool,
}
pub struct Progress {
    bar: ProgressBar,
    quiet: bool,
    color: bool,
    file: String,
}

impl Ui {
    pub fn new(quiet: bool, no_animation: bool) -> Self {
        let terminal =
            io::stderr().is_terminal() && env::var_os("TERM").is_none_or(|v| v != "dumb");
        Self {
            quiet,
            animated: terminal && !quiet && !no_animation && env::var_os("CI").is_none(),
            color: terminal && env::var_os("NO_COLOR").is_none(),
        }
    }
    pub fn welcome(&self) {
        if !self.quiet {
            eprintln!(
                "\n  {}  {}",
                self.accent("[ => ] ligaturize"),
                env!("CARGO_PKG_VERSION")
            );
            eprintln!("  Your font. Fira Code's ligatures.\n");
        }
    }
    fn accent(&self, s: &str) -> String {
        if self.color {
            format!("\x1b[96m{s}\x1b[0m")
        } else {
            s.to_owned()
        }
    }
    pub fn progress(&self, path: &Path) -> Progress {
        let bar = ProgressBar::new_spinner();
        let template = if self.color {
            "  {spinner:.cyan} {prefix}  {msg:.dim}"
        } else {
            "  {spinner} {prefix}  {msg}"
        };
        if let Ok(style) = ProgressStyle::with_template(template) {
            bar.set_style(style.tick_strings(&[
                "[   ]", "[ . ]", "[ - ]", "[ ->]", "[ =>]", "[==>]", "[ =>]", "[ ->]", "[ - ]",
                "[ . ]", "[ok ]",
            ]));
        }
        let file = path
            .file_name()
            .unwrap_or(path.as_os_str())
            .to_string_lossy()
            .into_owned();
        bar.set_prefix(file.clone());
        if self.animated {
            bar.enable_steady_tick(Duration::from_millis(85));
        } else {
            bar.set_draw_target(ProgressDrawTarget::hidden());
        }
        Progress {
            bar,
            quiet: self.quiet,
            color: self.color,
            file,
        }
    }
    pub fn summary(&self, successes: usize, total: usize, elapsed: Duration) {
        if !self.quiet {
            eprintln!(
                "\n  {successes}/{total} patched in {:.2}s. Enable contextual ligatures (calt) in your editor.\n",
                elapsed.as_secs_f64()
            );
        }
    }
}

impl Progress {
    pub fn stage(&self, stage: &str) {
        self.bar.set_message(stage.to_owned());
        self.bar.tick();
    }
    pub fn success(self, font: &PatchedFont) {
        self.bar.finish_and_clear();
        if !self.quiet {
            let check = if self.color {
                "\x1b[96m[ok]\x1b[0m"
            } else {
                "[ok]"
            };
            eprintln!(
                "  {check} {}  ->  {}  ({}; +{} glyphs)",
                self.file, font.family, font.weight, font.added_glyphs
            );
            if !font.missing_characters.is_empty() {
                eprintln!(
                    "       Partial coverage: missing {}",
                    font.missing_characters
                        .iter()
                        .map(|c| format!("'{c}'"))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
        }
    }
    pub fn failure(self, error: &anyhow::Error) {
        self.bar.finish_and_clear();
        eprintln!("  [error] {}: {error:#}", self.file);
    }
}
