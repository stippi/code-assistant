//! The shell every command line runs through.
//!
//! The system prompt tells the model which shell this is, so this module is
//! the one place that decides it.
//!
//! On Windows the order follows Zed: bash first (scoop, then the one Git
//! for Windows ships), because models write POSIX commands by habit; then
//! PowerShell (pwsh before Windows PowerShell); cmd.exe only as the last
//! resort. `bash.exe` is never looked up on `PATH`: `System32\bash.exe` is
//! the WSL launcher, which runs commands in a Linux VM.

use std::path::{Path, PathBuf};

/// What syntax the shell speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellKind {
    Posix,
    PowerShell,
    Cmd,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shell {
    pub program: String,
    /// The arguments preceding the command line.
    pub args: &'static [&'static str],
    pub kind: ShellKind,
}

impl Shell {
    /// The full argv running `command_line`.
    pub fn argv(&self, command_line: &str) -> Vec<String> {
        let mut argv = Vec::with_capacity(self.args.len() + 2);
        argv.push(self.program.clone());
        argv.extend(self.args.iter().map(|arg| arg.to_string()));
        argv.push(command_line.to_string());
        argv
    }

    #[cfg_attr(not(target_family = "windows"), allow(dead_code))]
    fn bash(program: &Path) -> Self {
        Self {
            program: program.to_string_lossy().into_owned(),
            // Not a login shell: Git Bash's profile may cd to $HOME, and the
            // Windows environment is the user's environment already.
            args: &["-c"],
            kind: ShellKind::Posix,
        }
    }

    #[cfg_attr(not(target_family = "windows"), allow(dead_code))]
    fn powershell(program: &Path) -> Self {
        Self {
            program: program.to_string_lossy().into_owned(),
            // Profiles are slow and may print; the agent needs neither.
            args: &["-NoProfile", "-NonInteractive", "-Command"],
            kind: ShellKind::PowerShell,
        }
    }

    fn cmd() -> Self {
        Self {
            program: "cmd".to_string(),
            args: &["/C"],
            kind: ShellKind::Cmd,
        }
    }
}

/// The shell commands run through: the user's login shell (`$SHELL`, else
/// bash) on unix; on Windows the first of bash, PowerShell, cmd.exe found
/// (detected once).
pub fn shell() -> Shell {
    #[cfg(target_family = "unix")]
    let shell = Shell {
        program: std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string()),
        args: &["-l", "-c"],
        kind: ShellKind::Posix,
    };
    #[cfg(target_family = "windows")]
    let shell = {
        static DETECTED: std::sync::OnceLock<Shell> = std::sync::OnceLock::new();
        DETECTED
            .get_or_init(|| {
                let shell = detect_windows_shell(
                    |program: &str| process_env::which(program),
                    |path| path.is_file(),
                    std::env::var_os("USERPROFILE").map(PathBuf::from),
                );
                tracing::info!("Commands run through {}", shell.program);
                shell
            })
            .clone()
    };
    shell
}

/// Pick the Windows shell. `which` finds a program on `PATH`, `is_file`
/// checks a candidate path; both are parameters for testing.
#[cfg_attr(not(target_family = "windows"), allow(dead_code))]
fn detect_windows_shell(
    which: impl Fn(&str) -> Option<PathBuf>,
    is_file: impl Fn(&Path) -> bool,
    user_profile: Option<PathBuf>,
) -> Shell {
    let scoop_bash = user_profile.map(|home| home.join("scoop").join("shims").join("bash.exe"));
    // git.exe lives in `<Git>\cmd` or `<Git>\mingw64\bin`; bash in `<Git>\bin`.
    let git_bash = which("git").into_iter().flat_map(|git| {
        git.ancestors()
            .skip(2)
            .take(2)
            .map(|dir| dir.join("bin").join("bash.exe"))
            .collect::<Vec<_>>()
    });
    if let Some(bash) = scoop_bash
        .into_iter()
        .chain(git_bash)
        .find(|path| is_file(path))
    {
        return Shell::bash(&bash);
    }
    ["pwsh", "powershell"]
        .into_iter()
        .find_map(&which)
        .map(|powershell| Shell::powershell(&powershell))
        .unwrap_or_else(Shell::cmd)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// A fake machine: programs on PATH and files on disk.
    struct Machine {
        on_path: HashMap<&'static str, PathBuf>,
        files: Vec<PathBuf>,
    }

    impl Machine {
        fn new() -> Self {
            Self {
                on_path: HashMap::new(),
                files: Vec::new(),
            }
        }

        fn program(mut self, name: &'static str, path: &str) -> Self {
            self.on_path.insert(name, PathBuf::from(path));
            self.files.push(PathBuf::from(path));
            self
        }

        fn file(mut self, path: &str) -> Self {
            self.files.push(PathBuf::from(path));
            self
        }

        fn detect(&self) -> Shell {
            detect_windows_shell(
                |program| self.on_path.get(program).cloned(),
                |path| self.files.iter().any(|file| file == path),
                Some(PathBuf::from("/Users/me")),
            )
        }
    }

    #[test]
    fn prefers_scoop_bash() {
        let shell = Machine::new()
            .file("/Users/me/scoop/shims/bash.exe")
            .program("git", "/Git/cmd/git.exe")
            .file("/Git/bin/bash.exe")
            .program("pwsh", "/PowerShell/7/pwsh.exe")
            .detect();
        assert_eq!(shell.program, "/Users/me/scoop/shims/bash.exe");
        assert_eq!(shell.kind, ShellKind::Posix);
        assert_eq!(shell.args, &["-c"]);
    }

    #[test]
    fn finds_git_bash_next_to_git() {
        let shell = Machine::new()
            .program("git", "/Git/cmd/git.exe")
            .file("/Git/bin/bash.exe")
            .program("pwsh", "/PowerShell/7/pwsh.exe")
            .detect();
        assert_eq!(shell.program, "/Git/bin/bash.exe");
        assert_eq!(shell.kind, ShellKind::Posix);
    }

    #[test]
    fn finds_git_bash_when_git_is_the_mingw_binary() {
        let shell = Machine::new()
            .program("git", "/Git/mingw64/bin/git.exe")
            .file("/Git/bin/bash.exe")
            .detect();
        assert_eq!(shell.program, "/Git/bin/bash.exe");
    }

    #[test]
    fn never_takes_bash_from_path() {
        // System32\bash.exe is the WSL launcher.
        let shell = Machine::new()
            .program("bash", "/Windows/System32/bash.exe")
            .program("powershell", "/Windows/System32/powershell.exe")
            .detect();
        assert_eq!(shell.kind, ShellKind::PowerShell);
    }

    #[test]
    fn falls_back_to_pwsh_before_windows_powershell() {
        let shell = Machine::new()
            .program("git", "/Git/cmd/git.exe") // but no bash next to it
            .program("pwsh", "/PowerShell/7/pwsh.exe")
            .program("powershell", "/Windows/System32/powershell.exe")
            .detect();
        assert_eq!(shell.program, "/PowerShell/7/pwsh.exe");
        assert_eq!(shell.kind, ShellKind::PowerShell);
        assert_eq!(shell.args, &["-NoProfile", "-NonInteractive", "-Command"]);
    }

    #[test]
    fn takes_windows_powershell_without_pwsh() {
        let shell = Machine::new()
            .program("powershell", "/Windows/System32/powershell.exe")
            .detect();
        assert_eq!(shell.program, "/Windows/System32/powershell.exe");
    }

    #[test]
    fn ends_with_cmd() {
        let shell = Machine::new().detect();
        assert_eq!(shell, Shell::cmd());
        assert_eq!(shell.argv("dir"), ["cmd", "/C", "dir"]);
    }
}
