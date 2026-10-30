mod font;
mod io;
mod layout;
mod ui;

#[cfg(test)]
mod tests;

use std::{
    io::{self as stdio, Write},
    path::PathBuf,
    process::ExitCode,
    time::Instant,
};

use anyhow::{Context, Result};
use clap::Parser;

#[derive(Parser)]
#[command(
    version,
    about = "Your font. Fira Code's ligatures.",
    long_about = "Patch static TrueType fonts with Fira Code's contextual programming ligatures.\nWorks offline. Originals stay untouched. Enable contextual ligatures in your editor.",
    after_help = "Examples:\n  ligaturize MyFont.ttf\n  ligaturize fonts/*.ttf --output-dir patched\n  ligaturize MyFont.ttf --no-animation"
)]
struct Args {
    /// TrueType fonts to patch (shell globs work)
    #[arg(required = true, num_args = 1.., value_name = "FONT")]
    fonts: Vec<PathBuf>,
    /// Write patched copies into this directory
    #[arg(short = 'o', long, value_name = "DIR")]
    output_dir: Option<PathBuf>,
    /// Replace existing outputs; never an input font
    #[arg(short, long)]
    force: bool,
    /// Only print output paths and errors
    #[arg(short, long)]
    quiet: bool,
    /// Disable animated terminal effects
    #[arg(long)]
    no_animation: bool,
}

fn main() -> ExitCode {
    match run(Args::parse()) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("ligaturize: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: Args) -> Result<bool> {
    let jobs = io::jobs(&args.fonts, args.output_dir.as_deref())?;
    let ui = ui::Ui::new(args.quiet, args.no_animation);
    ui.welcome();
    let started = Instant::now();
    let mut successes = 0;
    let mut stdout = stdio::stdout().lock();
    for (input, output) in &jobs {
        let progress = ui.progress(input);
        let result = (|| {
            io::check_output(output, &args.fonts, args.force)?;
            let bytes = io::read_font(input)?;
            let font = font::patch(&bytes, |stage| progress.stage(stage))
                .with_context(|| format!("could not patch {}", input.display()))?;
            progress.stage("Saving patched copy");
            io::save_font(output, &font.bytes, &args.fonts, args.force)?;
            Ok::<_, anyhow::Error>(font)
        })();
        match result {
            Ok(font) => {
                successes += 1;
                progress.success(&font);
                if let Err(error) = writeln!(stdout, "{}", output.display()) {
                    if error.kind() != stdio::ErrorKind::BrokenPipe {
                        return Err(error.into());
                    }
                }
            }
            Err(error) => progress.failure(&error),
        }
    }
    ui.summary(successes, jobs.len(), started.elapsed());
    Ok(successes == jobs.len())
}
