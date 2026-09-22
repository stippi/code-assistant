# Frame profiling of the messages view

Opt-in instrumentation that shows where the CPU time of a frame goes in the
GPUI frontend. Off by default: the timing wrapper is not inserted and the
build scopes are `None`, so the normal build carries one atomic load per
call site and nothing else.

## Switching it on

```bash
CODE_ASSISTANT_FRAME_PROFILE=1      cargo run --release   # collect and report
CODE_ASSISTANT_FRAME_PROFILE=scroll cargo run --release   # + sweep the list by moving its offset
CODE_ASSISTANT_FRAME_PROFILE=wheel  cargo run --release   # + sweep with dispatched scroll-wheel events
```

Every two seconds a report goes to stderr:

```
[frame-profile] t=24.5s scroll=yes frames=218 draw/frame mean=7.01ms p50=6.45ms p95=10.62ms max=15.66ms
  label (ms per frame)                n/frame    build   layout prepaint    paint    total  %draw
  messages.list                          1.0     0.00     0.00     5.20     0.32     5.52    79%
  row                                    2.0     0.00     1.35     0.11     0.32     1.79    25%
  block.tool:edit                        1.2     1.01     0.29     0.09     0.27     1.66    24%
  ...
  row layout compute (derived)                                                        3.74    53%
```

* `frames` and the `draw/frame` distribution come from gpui's own frame
  timings (`gpui::set_frame_trace_enabled`, `Window::draw` only, no present).
  Idle intervals have zero frames: nothing is drawn unless something
  invalidates.
* Each label is a [`Timed`](../crates/ui_gpui/src/shared/frame_profile.rs)
  wrapper around an element: `layout` is its `request_layout` (element tree
  building including the `render` of nested views), `prepaint` and `paint`
  are those phases. `build` is a scope around the code that produced the
  element (a `Render::render` body). All numbers are per drawn frame,
  averaged over the interval; nested labels overlap (a row contains its
  blocks, the list contains its rows).
* `n/frame` is how many elements of that label a frame held, i.e. how many
  rows or blocks the virtualized list really built.
* `row layout compute (derived)` is the list's prepaint minus what its rows
  recorded: the time taffy and text measuring (line wrapping, shaping) spend
  in `layout_as_root`, which no wrapper sees from the inside.

Labels: `region.sidebar`, `region.messages`, `region.input`,
`region.review` (the main screen's columns), `messages.list`, `row`,
`block.text`, `block.thinking`, `block.compaction`, `block.image`,
`block.tool:<tool name>`.

## Sweeps

`scroll` and `wheel` wait 8 s after start, then scroll the list up and down
for 40 s (12 px every 8 ms, direction flips at the ends), then stop; reports
during the sweep say `scroll=yes`. `wheel` sends `ScrollWheelEvent`s through
`Window::dispatch_event` at the list's center, so hit testing and the scroll
handlers run like they do for a real wheel; `scroll` moves the `ListState`
offset directly.

## A reproducible load

Point the app at a copy of a long session so runs compare:

```bash
mkdir -p /tmp/perf-data/sessions
cp ~/Library/Application\ Support/code-assistant/sessions/<id>.json /tmp/perf-data/sessions/
# metadata.json lists the sessions; one entry, filtered from the real file
CODE_ASSISTANT_DATA_DIR=/tmp/perf-data CODE_ASSISTANT_FRAME_PROFILE=wheel cargo run --release
```

The window has to be on screen. gpui stops the display link while a window
is occluded (or the screen is locked), and then no frames are drawn at all.
When the app is started from a script, launch it through LaunchServices
(a minimal `.app` bundle plus `open -n -a ... --env ... --stderr ...`) so it
is activated; a plain background `exec` tends to end up behind other windows.

## Function-level attribution

The labels say which element costs what; `sample` (or Instruments) says
which library function: `sample <pid> 8 -file out.txt` on the release
binary while the sweep runs, then look at the main thread's call graph for
`layout_as_root`, `shape_line`, `wrap_line`, `compute_layout` and `paint`.

## Findings (2026-09-22, branch perf/messages-frame-profiling)

Load: session "Markdown-source copy in gpui frontend" (771 rows: 317 text,
38 thinking, 96 `edit`, 171 `execute_command`), 1680x1050 window, release
build, M-series Mac at 120 Hz. Sweeps run at 118-120 fps as long as
`sample` is not attached (attaching it suspends the process once per
millisecond and inflates `Window::draw` to 20-50 ms, so those intervals are
excluded).

Per frame, frame-weighted over the sweep (`scroll` run, 2097 frames, draw
mean 3.9 ms, p95 4.9 ms, max 13.0 ms; the `wheel` run gave 5.1 / 6.5 / 18.0 ms
with more diff cards in view):

| label | n/frame | build | layout | prepaint | paint | total | % draw |
|---|---|---|---|---|---|---|---|
| region.messages | 1.0 | 0.00 | 0.01 | 1.27 | 0.54 | 1.83 | 46 % |
| row | 2.9 | 0.01 | 0.45 | 0.04 | 0.53 | 1.03 | 26 % |
| block.text | 2.6 | 0.01 | 0.13 | 0.02 | 0.36 | 0.52 | 13 % |
| region.sidebar | 1.0 | 0.00 | 0.20 | 0.05 | 0.16 | 0.41 | 10 % |
| block.tool:edit | 0.7 | 0.19 | 0.05 | 0.01 | 0.07 | 0.32 | 8 % |
| region.input | 1.0 | 0.00 | 0.14 | 0.05 | 0.11 | 0.30 | 8 % |
| block.tool:execute_command | 1.4 | 0.03 | 0.03 | 0.01 | 0.09 | 0.15 | 4 % |
| row layout compute (derived) | | | | | | 0.76 | 19 % |

With diff cards in view (interval means up to 7 ms, `block.tool:edit`
1.2/frame) the derived row layout compute rises to 3.7 ms and the card's
build to 1.0 ms/frame.

Main-thread self time by phase (`sample`, innermost known frame wins, four
8 s captures, both sweeps):

| phase | % of draw |
|---|---|
| taffy layout compute (flexbox, tree, cache keys) | 38-41 % |
| paint (scene primitives, glyph atlas) | 13 % |
| element build (`render`, `request_layout`) | 10-14 % |
| text shaping (CoreText, `layout_line`) | 7-8.5 % |
| prepaint (hitboxes, bounds tree) | 4-7 % |
| present / Metal | 3.5-6 % |
| tree-sitter highlight query per diff row | 1-7 % |
| text wrapping (`LineWrapper`) | 2-3 % |
| event dispatch (wheel run) | 1-3 % |
| markdown element build, diff card build, list bookkeeping | 1-2 % each |

Where the taffy time comes from: 56-65 % is the list rows' `layout_as_root`
(one root per visible row, every frame), 33-40 % is gpui-component's
`InlineFlow::prepaint`, which lays every wrapped text fragment out as its
own taffy root; the window root tree (sidebars, input) is 2-4 %.
`Theme::clone` per block does not show up in the samples.

## Optimizations and their effect

Measured with the same session and sweep, both binaries run back to back
on a 60 Hz display (draw times do not depend on the refresh rate, frame
counts do).

1. **Diff row syntax styles once per theme** (`DiffSyntax` caches every
   line's styles, computed on the background thread that parsed). The
   tree-sitter query per row and frame fell from 4-7 % of the draw time to
   0.2-0.4 %; the `edit` card's build went from 0.85 ms to 0.25 ms per card.
2. **Diff rows as one element** (`DiffRows` in
   `tool_cards/diff_rows.rs`, used by tool cards and the review panel).
   Taffy's share of the draw time fell from 39-40 % to 25-27 %. Over the
   whole sweep the draw mean went from 5.44 ms to 5.16 ms (p95 6.50 to
   5.93 ms); in intervals with diff cards in view it went down by
   0.3-0.7 ms per frame, and a card-heavy frame from 4.7 ms to 2.1 ms of
   `edit` time.

What is left is text: `block.text` (markdown) is now the largest block
cost, and gpui-component's `InlineFlow` still lays every wrapped fragment
out as its own taffy root in `prepaint` (a third of the remaining taffy
time). That is the next candidate, as a contribution to gpui-component.
