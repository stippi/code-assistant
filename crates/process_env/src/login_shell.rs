//! Capturing the environment of the user's login shell.
//!
//! `$SHELL -l -i -c` reads the profile and the interactive startup files
//! (many tools add themselves to `PATH` in `.zshrc` or `.bashrc`, which a
//! login shell alone skips) and prints the environment with `env -0`
//! between two markers, so whatever the startup files print is ignored.
//! Reading stops at the second marker: a startup file may leave a
//! background process holding the output open.

use std::ffi::OsString;
use std::io::Read;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

/// How long the startup files may take.
const TIMEOUT: Duration = Duration::from_secs(5);

const MARKER: &str = "__CODE_ASSISTANT_LOGIN_ENV__";

/// Variables that describe the capturing shell, not the user's setup.
const SKIPPED: &[&str] = &["PWD", "OLDPWD", "SHLVL", "_"];

pub(crate) fn capture() -> Result<Vec<(OsString, OsString)>, String> {
    capture_with(&shell(), TIMEOUT)
}

fn shell() -> PathBuf {
    match std::env::var_os("SHELL") {
        Some(shell) if !shell.is_empty() => shell.into(),
        _ if cfg!(target_os = "macos") => "/bin/zsh".into(),
        _ => "/bin/sh".into(),
    }
}

fn capture_with(shell: &Path, timeout: Duration) -> Result<Vec<(OsString, OsString)>, String> {
    let script = format!("printf '%s' {MARKER}; command env -0; printf '%s' {MARKER}");
    let mut cmd = Command::new(shell);
    cmd.args(["-l", "-i", "-c", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        // Its own process group: an interactive shell must not take over a
        // terminal the app may have.
        .process_group(0);
    if let Some(home) = std::env::var_os("HOME") {
        cmd.current_dir(home);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("could not start {}: {e}", shell.display()))?;
    let mut stdout = child.stdout.take().expect("stdout is piped");

    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(read_until_end_marker(&mut stdout));
    });
    let output = rx.recv_timeout(timeout);
    let _ = child.kill();
    let _ = child.wait();

    let output = output.map_err(|_| {
        format!(
            "{} did not print its environment within {} s",
            shell.display(),
            timeout.as_secs_f32()
        )
    })?;
    parse(&output).ok_or_else(|| format!("{} printed no environment", shell.display()))
}

/// Read until the second marker or the end of the output.
fn read_until_end_marker(reader: &mut impl Read) -> Vec<u8> {
    let mut output = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        match reader.read(&mut chunk) {
            Ok(0) | Err(_) => return output,
            Ok(n) => output.extend_from_slice(&chunk[..n]),
        }
        if between_markers(&output).is_some() {
            return output;
        }
    }
}

fn between_markers(output: &[u8]) -> Option<&[u8]> {
    let marker = MARKER.as_bytes();
    let start = find(output, marker)? + marker.len();
    let end = start + find(&output[start..], marker)?;
    Some(&output[start..end])
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// The variables of `env -0` output between the markers.
fn parse(output: &[u8]) -> Option<Vec<(OsString, OsString)>> {
    let body = between_markers(output)?;
    let vars: Vec<_> = body
        .split(|b| *b == 0)
        .filter_map(|entry| {
            let eq = entry.iter().position(|b| *b == b'=')?;
            let (name, value) = (&entry[..eq], &entry[eq + 1..]);
            if name.is_empty() || SKIPPED.iter().any(|s| s.as_bytes() == name) {
                return None;
            }
            Some((
                OsString::from_vec(name.to_vec()),
                OsString::from_vec(value.to_vec()),
            ))
        })
        .collect();
    (!vars.is_empty()).then_some(vars)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::time::Instant;

    /// A stand-in for `$SHELL`: runs the capture script after `prelude`.
    fn fake_shell(dir: &Path, prelude: &str) -> PathBuf {
        let path = dir.join("fake-shell");
        // Called as `fake-shell -l -i -c <script>`.
        let script = format!("#!/bin/sh\n{prelude}\nexec /bin/sh -c \"$4\"\n");
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn get<'a>(vars: &'a [(OsString, OsString)], name: &str) -> Option<&'a OsString> {
        vars.iter().find(|(k, _)| k == name).map(|(_, v)| v)
    }

    #[test]
    fn the_environment_is_read_between_the_markers() {
        let output = format!(
            "Last login: today\n{MARKER}PATH=/opt/bin:/usr/bin\0EQ=a=b\0MULTI=one\ntwo\0\
             PWD=/home\0SHLVL=2\0=x\0junk\0{MARKER}prompt noise"
        );
        let vars = parse(output.as_bytes()).unwrap();
        assert_eq!(get(&vars, "PATH").unwrap(), "/opt/bin:/usr/bin");
        assert_eq!(get(&vars, "EQ").unwrap(), "a=b");
        assert_eq!(get(&vars, "MULTI").unwrap(), "one\ntwo");
        assert_eq!(vars.len(), 3, "{vars:?}");
    }

    #[test]
    fn output_without_both_markers_is_no_environment() {
        assert!(parse(b"PATH=/usr/bin\0").is_none());
        assert!(parse(format!("{MARKER}PATH=/usr/bin\0").as_bytes()).is_none());
        assert!(parse(format!("{MARKER}{MARKER}").as_bytes()).is_none());
    }

    #[test]
    fn a_shell_that_prints_while_starting_still_gives_its_environment() {
        let dir = tempfile::tempdir().unwrap();
        let shell = fake_shell(
            dir.path(),
            "echo 'Welcome!'; export CA_FROM_PROFILE=yes; export PATH=/from/profile:$PATH",
        );
        let vars = capture_with(&shell, TIMEOUT).unwrap();
        assert_eq!(get(&vars, "CA_FROM_PROFILE").unwrap(), "yes");
        let path = get(&vars, "PATH").unwrap().to_string_lossy();
        assert!(path.starts_with("/from/profile:"), "{path}");
    }

    #[test]
    fn a_background_process_holding_the_output_does_not_delay_the_capture() {
        let dir = tempfile::tempdir().unwrap();
        let shell = fake_shell(dir.path(), "sleep 30 &");
        let started = Instant::now();
        let vars = capture_with(&shell, Duration::from_secs(10)).unwrap();
        assert!(get(&vars, "HOME").is_some());
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn a_shell_that_hangs_gives_up_after_the_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let shell = fake_shell(dir.path(), "sleep 30");
        let started = Instant::now();
        let error = capture_with(&shell, Duration::from_millis(300)).unwrap_err();
        assert!(error.contains("did not print its environment"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn a_missing_shell_is_an_error() {
        let error = capture_with(Path::new("/does/not/exist"), TIMEOUT).unwrap_err();
        assert!(error.contains("could not start"), "{error}");
    }
}
