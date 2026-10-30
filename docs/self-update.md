# Self-update

A released app notices a newer release, downloads it when the user asks,
and replaces itself on *Restart to update*. Modelled on T3 Code's desktop
updater (electron-updater driven by a small state machine), without an
installer framework: our release artifacts are zips of a single binary or of
the macOS `.app` bundle, so the swap is ours to do, and GPUI's
`App::restart` already restarts the app on every platform.

Code: `crates/app_update` (Layer 0: manifest, installation detection,
download, install, cleanup), `code_assistant_core::updates` (the
`UpdateService`, its state and poller), `ui_gpui` (title-bar pill, About
dialog).

## Feed

Every release carries an `update-manifest.json` asset, written by the
`publish` job of `.github/workflows/release.yml` from the zips it publishes:

```json
{
  "version": "0.2.19",
  "assets": {
    "Code-Assistant-macos-aarch64.app.zip": {
      "url": "https://github.com/stippi/code-assistant/releases/download/v0.2.19/Code-Assistant-macos-aarch64.app.zip",
      "sha256": "…",
      "size": 31457280
    },
    "code-assistant-linux-x86_64.zip": { "…": "…" }
  }
}
```

The app reads it from
`https://github.com/stippi/code-assistant/releases/latest/download/update-manifest.json`,
not from the REST API: unauthenticated API calls are limited to 60 an hour
per IP, which users behind one company NAT would share.

The manifest is keyed by asset file name. Which name an installation needs
is the app's business (`updates::asset_name`): the target triple it was
built for plus whether it runs from an `.app` bundle. An Intel build running
under Rosetta asks for the `aarch64` asset, so the next update moves it to
the native build (T3 does the same).

The sha256 comes from the same release, so it protects the download, not
against a compromised release. On macOS the new bundle must in addition
carry the running app's code-signing team (below). Signing the manifest
(minisign, key compiled in) would close the gap; not done yet.

## When updates are on

Only builds of the release workflow update themselves: it sets
`CODE_ASSISTANT_UPDATE_FEED` while building, `build.rs` compiles the URL in
(`version::update_feed()`). Everything else — `cargo build`, the
`target/release` binary Zed starts over ACP — has no feed and never replaces
itself. The git tree state can not decide this: an explicit release version
is applied as a working-tree edit, so release builds are "dirty".

The service reports *disabled* with a reason when

- the build has no feed (runtime `CODE_ASSISTANT_UPDATE_FEED` sets one, for
  testing against a local server),
- `CODE_ASSISTANT_DISABLE_AUTO_UPDATE` is set,
- macOS runs the app translocated (started from `~/Downloads` without moving
  it; Gatekeeper runs it from a read-only random path) — *move it to
  Applications*,
- the folder holding the bundle or binary is not writable (e.g. a binary in
  `/usr/local/bin`),
- the platform has no release asset.

Only the GPUI frontend runs the service; ACP, MCP-server and terminal modes
do not check for updates.

## States

T3 keeps one flat record with `status`, `errorContext` and `canRetry`; an
enum carries the same:

| Status | Pill | Click |
| --- | --- | --- |
| `Disabled { reason }` | — | — |
| `Idle` / `Checking` / `UpToDate` | — | — |
| `Available { version }` | *Update 0.2.19* | download |
| `Downloading { version, received, total }` | progress | — |
| `Downloaded { version }` | *Restart to update* | install + restart |
| `Installing { version }` | spinner | — |

Next to the status: the last error with its context (check, download,
install) and `checked_at`. Rules, as in T3:

- A failed download goes back to `Available` with the error; the pill offers
  the retry ("Download failed — retry").
- A failed install goes back to `Downloaded` with the error.
- A failed check keeps the status it had. Unlike T3 the pill shows nothing
  for it — a laptop offline for a day would otherwise carry a warning in its
  title bar; the About dialog shows the error. A check asked for by hand
  shows its result there.
- A check finding the version already downloaded (and verified) goes
  straight to `Downloaded`.
- One action at a time: a check while downloading or installing is skipped.

## Checking

First check 15 seconds after start, then every 4 hours, and by hand from the
About dialog. Downloading starts only on a click (an *automatically
download* setting can come later).

## Download

`<cache dir>/code-assistant/updates/<version>/<asset>` (macOS:
`~/Library/Caches/…`).

- Bytes go to `<asset>.part`. A new attempt asks for the rest with
  `Range: bytes=<len>-`; a `206` appends, a `200` starts over, a `416` drops
  the part and starts over.
- Network errors, timeouts, `5xx`, `408` and `429` are retried with backoff
  (5 attempts: 1 s, 2 s, 4 s, 8 s), every attempt resuming the part. Other
  `4xx` fail at once. After the last attempt the state goes back to
  `Available`; the part stays, so the user's retry resumes it, also after a
  restart of the app.
- The finished part must have the manifest's size and sha256; then it is
  renamed to `<asset>`. A mismatch deletes it and counts as a failed
  attempt.
- Progress is published when the whole percentage changes.

electron-updater (T3) does not resume: it retries server errors and, for
some targets, downloads only the blocks that changed (blockmaps). A single
binary changes almost entirely between releases, so blockmaps would not
help us; resuming does.

## Install and restart

*Restart to update* first asks when sessions are working (their turn is
cut off; T3 stops its backends the same way). Then, on the backend runtime:

1. Extract next to the installation, in a hidden folder on the same volume,
   so the swap is a `rename`.
2. Check: macOS: `codesign --verify --deep --strict` on the new bundle, and
   when the running app is signed by a team, the new one must be signed by
   the same team. Elsewhere: the binary is a regular file.
3. Write the install record (`updates/install.json`:
   `{ from_version, target_version }`).
4. Swap:
   - **macOS bundle**: running `.app` → hidden backup, new `.app` into place,
     backup deleted.
   - **Linux/macOS binary**: new file → `rename` over the old one. The
     running process (and any ACP process Zed started) keeps the old inode.
   - **Windows**: a running `.exe` can not be overwritten but can be
     renamed: `code-assistant.exe` → `code-assistant.exe.old`, new file into
     place. The `.old` file is deleted on the next start (or the one after,
     while another process still runs it).
   If the second rename fails, the first is undone.
5. GPUI calls `cx.restart()`: a script waits for the process to exit and
   starts the same path again (`open` for the bundle).

Any failure before the swap leaves the installation untouched and goes back
to `Downloaded` with the error.

## Cleanup and "did not apply"

At start the service

- reads the install record: running `from_version` still means the update
  did not apply — the state offers it again with that note (T3: "Update X
  did not apply. Click to retry."); otherwise the record is deleted,
- deletes `updates/<version>` folders for versions not newer than the
  running one,
- deletes leftovers of an interrupted install next to the installation
  (hidden staging folder, macOS backup, Windows `.old`).

When a check finds a newer version than one partly or fully downloaded, the
older folder goes.

## Release workflow

- The build jobs set `CODE_ASSISTANT_UPDATE_FEED` for `cargo build`.
- The `publish` job writes `update-manifest.json` (`scripts/update-manifest.sh`:
  name, URL, sha256 and size of every zip) and uploads it with the zips.

## Not done yet

- Release notes in the pill (the release text is "Release vX" for now).
- A setting to download automatically.
- A signed manifest.
- Channels (T3 has stable and nightly).
