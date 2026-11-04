# The environment of child processes

An app started from the Finder, the Dock or a desktop launcher does not get
the environment a terminal has. On macOS its `PATH` is
`/usr/bin:/bin:/usr/sbin:/sbin`, so `uvx`, `npx`, `gh` and anything from
Homebrew, Nix or `~/.local/bin` are not found, and the variables a shell
profile sets (proxies, `JAVA_HOME`, an SSH agent's socket, …) are missing.
An MCP server configured as `uvx some-server` then fails to start, and the
GitHub login through `gh` looks absent.

## How it works

Every child process starts through the `process_env` crate:

- `process_env::command(program)` (tokio) and
  `process_env::std_command(program)` (std) build the command with the
  user's login environment set on top of the app's own; a bare program name
  is looked up on that `PATH`. Variables set on the command afterwards (an
  MCP server's `env`, `GIT_TERMINAL_PROMPT=0`, …) still win.
- `process_env::which(program)` finds a program on that `PATH`, and
  `process_env::environment()` gives the variables for spawners with their
  own command type (`pty_session` sets them on its `CommandBuilder`).
- The login environment is captured once per process, from
  `$SHELL -l -i -c` (login and interactive, since many tools add themselves
  to `PATH` in `.zshrc` or `.bashrc`), which prints it with `env -0` between
  two markers. Whatever the startup files print is ignored, reading stops at
  the second marker, and after 5 seconds the capture gives up. The shell
  runs in the home folder, with no input and in a process group of its own.
- `main` calls `process_env::preload()` first thing, so the capture runs
  while the app starts; the first process waits for it only if it is not
  done yet.

The capture is skipped, and processes get the app's own environment as
before, when:

- a shell started the app (`SHLVL` is set): its environment is the user's
  already, e.g. `code-assistant` run in a terminal, or an editor that
  starts the ACP agent with the environment it captured itself;
- it runs on Windows, where apps get the user's environment from the
  registry;
- `CODE_ASSISTANT_LOGIN_ENV=0` is set;
- the shell fails, prints no environment or times out (logged as a
  warning).

`PWD`, `OLDPWD`, `SHLVL` and `_` describe the capturing shell and are not
taken over.

The agent's commands (`command_executor`, `pty_session`) still run through
`$SHELL -l -c`, as users expect shell syntax there; they now also start
from the captured environment, so tools added to `PATH` in `.zshrc` are
found there too.

## Keeping it that way

`clippy.toml` disallows `std::process::Command::new` and
`tokio::process::Command::new`; `cargo clippy` fails on a process started
past `process_env`. Only the crate itself and build scripts (which run in
the build's environment) allow them.
