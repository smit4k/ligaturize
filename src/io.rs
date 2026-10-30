use std::{
    collections::BTreeSet,
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use tempfile::NamedTempFile;

const MAX_FONT_BYTES: u64 = 64 * 1024 * 1024;

pub fn jobs(inputs: &[PathBuf], output_dir: Option<&Path>) -> Result<Vec<(PathBuf, PathBuf)>> {
    if let Some(dir) = output_dir {
        fs::create_dir_all(dir).with_context(|| format!("could not create {}", dir.display()))?;
    }
    let mut destinations = BTreeSet::new();
    inputs
        .iter()
        .map(|input| {
            let mut name = input
                .file_stem()
                .context("input has no file name")?
                .to_os_string();
            name.push("-Liga.ttf");
            let parent = output_dir.unwrap_or_else(|| {
                input
                    .parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .unwrap_or(Path::new("."))
            });
            let output = parent.join(name);
            let canonical = resolved_path(&output)?;
            ensure!(
                destinations.insert(canonical),
                "multiple inputs would write {}; use separate output directories",
                output.display()
            );
            // Preflight the entire batch before writing, including output paths that are other inputs.
            check_alias(&output, inputs)?;
            Ok((input.clone(), output))
        })
        .collect()
}

fn resolved_path(path: &Path) -> Result<PathBuf> {
    if let Ok(canonical) = fs::canonicalize(path) {
        return Ok(canonical);
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let parent = fs::canonicalize(parent).or_else(|_| std::path::absolute(parent))?;
    Ok(parent.join(path.file_name().context("missing file name")?))
}

fn check_alias(output: &Path, inputs: &[PathBuf]) -> Result<()> {
    let output_metadata = fs::metadata(output).ok();
    let output_path = resolved_path(output)?;
    for input in inputs {
        let same_path = output_path == resolved_path(input)?;
        #[cfg(unix)]
        let same_inode = {
            use std::os::unix::fs::MetadataExt;
            output_metadata
                .as_ref()
                .zip(fs::metadata(input).ok().as_ref())
                .is_some_and(|(a, b)| a.dev() == b.dev() && a.ino() == b.ino())
        };
        #[cfg(not(unix))]
        let same_inode = false;
        ensure!(
            !same_path && !same_inode,
            "output {} aliases input {}; originals cannot be overwritten",
            output.display(),
            input.display()
        );
    }
    Ok(())
}

pub fn check_output(output: &Path, inputs: &[PathBuf], force: bool) -> Result<()> {
    check_alias(output, inputs)?;
    if fs::symlink_metadata(output).is_ok() {
        ensure!(
            force,
            "{} already exists; use --force to replace the patched copy",
            output.display()
        );
        ensure!(
            !output.is_dir(),
            "output {} is a directory",
            output.display()
        );
    }
    Ok(())
}

pub fn read_font(path: &Path) -> Result<Vec<u8>> {
    let file = File::open(path).with_context(|| format!("could not open {}", path.display()))?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file(),
        "{} is not a regular file",
        path.display()
    );
    ensure!(
        metadata.len() <= MAX_FONT_BYTES,
        "{} exceeds the 64 MiB font limit",
        path.display()
    );
    let mut bytes = Vec::new();
    file.take(MAX_FONT_BYTES + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= MAX_FONT_BYTES,
        "font exceeds the 64 MiB limit"
    );
    Ok(bytes)
}

pub fn save_font(output: &Path, bytes: &[u8], inputs: &[PathBuf], force: bool) -> Result<()> {
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut temp = NamedTempFile::new_in(parent).context("could not create temporary output")?;
    temp.write_all(bytes)?;
    temp.as_file().sync_all()?;
    check_output(output, inputs, force)?;
    if force {
        temp.persist(output)
    } else {
        temp.persist_noclobber(output)
    }
    .map_err(|error| error.error)
    .with_context(|| format!("could not save {}", output.display()))?;
    Ok(())
}
