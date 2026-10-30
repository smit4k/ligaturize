#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::{fs, process::Command};

fn cli() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ligaturize"));
    command
        .env("NO_COLOR", "1")
        .env("TERM", "dumb")
        .env("CI", "1");
    command
}

#[test]
fn batch_reports_failures_and_emits_only_successful_paths_to_stdout() {
    let dir = tempfile::tempdir().unwrap();
    let good = dir.path().join("A Font.ttf");
    let bad = dir.path().join("broken.ttf");
    let source = include_bytes!("ABeeZee-Regular.ttf");
    fs::write(&good, source).unwrap();
    fs::write(&bad, b"invalid font").unwrap();
    let result = cli()
        .arg(dir.path().join("missing-directory/missing.ttf"))
        .arg(&bad)
        .arg(&good)
        .arg("--quiet")
        .output()
        .unwrap();
    let output = dir.path().join("A Font-Liga.ttf");
    assert!(!result.status.success());
    assert_eq!(
        String::from_utf8(result.stdout).unwrap(),
        format!("{}\n", output.display())
    );
    let stderr = String::from_utf8(result.stderr).unwrap();
    assert!(stderr.contains("broken.ttf"));
    assert!(!stderr.contains('\x1b'));
    assert_eq!(fs::read(&good).unwrap(), source);
    assert!(output.exists());
    let old_output = fs::read(&output).unwrap();
    assert!(
        !cli()
            .arg(&good)
            .arg("--quiet")
            .output()
            .unwrap()
            .status
            .success()
    );
    assert_eq!(fs::read(&output).unwrap(), old_output);
    assert!(
        cli()
            .arg(&good)
            .args(["--force", "--quiet"])
            .output()
            .unwrap()
            .status
            .success()
    );
}

#[test]
fn redirected_presentation_has_no_escape_sequences() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("font.ttf");
    fs::write(&input, include_bytes!("ABeeZee-Regular.ttf")).unwrap();
    let result = cli().arg(&input).output().unwrap();
    assert!(result.status.success());
    assert!(!result.stdout.contains(&0x1b));
    assert!(!result.stderr.contains(&0x1b));
    assert!(
        String::from_utf8(result.stderr)
            .unwrap()
            .contains("1/1 patched")
    );
    let help = cli().arg("--help").output().unwrap();
    assert!(help.status.success());
    assert!(
        String::from_utf8(help.stdout)
            .unwrap()
            .contains("--no-animation")
    );
}

#[test]
fn closed_stdout_does_not_abort_batch_or_hide_failures() {
    use std::process::Stdio;
    let dir = tempfile::tempdir().unwrap();
    let bad = dir.path().join("bad.ttf");
    let a = dir.path().join("a.ttf");
    let b = dir.path().join("b.ttf");
    fs::write(&bad, b"not a font").unwrap();
    for path in [&a, &b] {
        fs::write(path, include_bytes!("ABeeZee-Regular.ttf")).unwrap();
    }
    let mut child = cli()
        .arg(&bad)
        .arg(&a)
        .arg(&b)
        .arg("--quiet")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    assert!(!child.wait().unwrap().success());
    assert!(dir.path().join("a-Liga.ttf").exists());
    assert!(dir.path().join("b-Liga.ttf").exists());
}
