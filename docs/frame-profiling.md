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
`sample` prints mangled Rust symbols; `c++filt` demangles them (v0).

## Scripts

`scripts/frame-profile/` holds what the measurements below were made with:

* `run_sweep.sh NAME BINARY MODE [SAMPLE_AT]` runs one sweep of a release
  binary through a throwaway `.app` bundle (so the window is activated),
  writes the reports to `NAME.stderr.log` and, with `SAMPLE_AT`, a
  demangled `sample` capture to `NAME.dem.txt`. `CODE_ASSISTANT_DATA_DIR`
  has to point at the session copy; `FRAME_PROFILE_OUT` sets the output
  directory. Copy the binaries of both builds first and run them back to
  back.
* `report_agg.py LOG...` aggregates the `scroll=yes` intervals of a log,
  frame-weighted: draw mean, p50, p95, max and the per-label table.
  `--max-mean MS` drops the intervals inflated by an attached `sample`.
* `sample_phase.py OUT.txt` attributes the main thread's samples below
  `Window::draw` to phases (taffy, shaping, wrapping, paint, ...; innermost
  known frame wins), lists the top self symbols and, with `--incl a,b`,
  inclusive counts. `--tree SYM` prints the merged subtree below a symbol.
  The phase rules are heuristics: check a subtree before trusting a share
  (see the mis-attribution noted under Findings).

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

Where the taffy time comes from: the list rows' `layout_as_root` (one root
per visible row, every frame) and the row trees below it; the window root
tree (sidebars, input) is 2-4 %. `Theme::clone` per block does not show up
in the samples.

An earlier version of this section attributed a third of the taffy time to
gpui-component's `InlineFlow::prepaint`. That was a mis-attribution by the
sample classifier (every nested `prepaint_as_root` under the list was
counted as a flow fragment). The gpui-component revision this workspace
pins (fork branch `pin-zed-cc053a4a`, upstream ~#2670) uses `InlineFlow`
only for paragraphs that mix inline images and text; the session above has
five of those and 457 paragraphs with code spans, and code spans render as
one `Inline` there. `InlineFlow` does not appear in any `sample` of this
session at all. The nested roots in the samples are the list rows.

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

What was left is text: `block.text` (markdown) was the largest block
cost. In the gpui-component revision pinned at the time its per-frame work
was the `Inline` paragraph's `StyledText` measurement (line wrapper plus a
shaping-cache lookup per paragraph per frame, `TextLayout::layout` 16 % of
the draw time inclusive) and the paragraph's `Div`s. Upstream gpui-component
has since addressed both (#3090, retained layouts and fewer elements per
paragraph); the gpui-kit migration below picks that up.

## InlineFlow (upstream contribution, 2026-09-22)

Upstream `main` (gpui-kit 0.6.5, `crates/base/src/text/inline_flow.rs`)
lays every paragraph with a code span out as an `InlineFlow`: the measure
callback wraps the paragraph and shapes every fragment, up to three times
a frame (taffy probes unconstrained and at the column width), and
`prepaint` builds a `div` plus `Inline` per wrapped fragment as its own
taffy root with a fresh `InlineState`, so the `Inline`'s retained layout
never hit. Measured with upstream's own bench (`cargo bench -p gpui-base
--bench text_view_scroll`, real text system, headless Metal) extended by a
variant with a code span in every paragraph and list item:

| bench (draw mean / p95) | upstream main | with the change |
|---|---|---|
| `text_view_scroll` (no code spans) | 1.61-1.63 / 1.79-1.83 ms | 1.61-1.63 / 1.75-1.80 ms |
| `text_view_scroll_inline_code` | 5.89-6.02 / 6.50-6.69 ms | 3.04-3.05 / 3.37-3.39 ms |

The change (three commits, fork branch `perf/inline-flow-frame-cache`,
PR [longbridge/gpui-kit#3180](https://github.com/longbridge/gpui-kit/pull/3180)): fragment `InlineState`s live in
the flow's element state, so the `Inline`s find their shaped text again;
the `div` around each fragment is gone (the fragment's text style is
pushed around the `Inline`'s layout instead); and the flow keeps its
layouts per wrap width, keyed by items, image sizes and typography, so a
frame that changes nothing wraps and shapes nothing. `sample` on the bench
binary: `InlineFlow::request_layout` went from 40 % of the draw time to
1 %, and what is left in `prepaint` is gpui's `TextLayout::layout` per
fragment (closure and taffy leaf per frame, a gpui cost).

gpui-kit 0.6.6 does not carry the PR yet; the workspace consumes it through
a `[patch.crates-io]` entry for `gpui-base` (fork branch
`perf/inline-flow-frame-cache-0.6.6`: the v0.6.6 tag plus the PR's commits),
measured below.

## gpui-kit 0.6.6 migration (2026-09-22)

Same session copy, same sweeps, both binaries back to back (main at
bd330ff3 on gpui 0.2.2 / gpui-component 0.5.x, and the migration branch on
gpui-kit 0.6.6, i.e. gpui-pre 0.3.6 and gpui-component 0.6.6), 60 Hz
display, ~2295 frames per sweep, no `sample` attached. Per frame,
frame-weighted over the sweep:

| sweep | binary | draw mean / p50 / p95 / max | `block.text` n/frame | layout | prepaint | paint | total (% draw) |
|---|---|---|---|---|---|---|---|
| scroll | before | 4.55 / 4.54 / 5.70 / 19.1 ms | 1.99 | 0.11 | 0.02 | 0.18 | 0.32 (7 %) |
| scroll | after | 4.54 / 4.49 / 5.72 / 19.8 ms | 1.99 | 0.11 | 0.17 | 0.16 | 0.46 (10 %) |
| wheel | before | 4.35 / 4.37 / 5.61 / 19.0 ms | 2.11 | 0.11 | 0.02 | 0.21 | 0.35 (8 %) |
| wheel | after | 4.42 / 4.43 / 5.80 / 14.3 ms | 2.15 | 0.12 | 0.20 | 0.17 | 0.50 (11 %) |

The draw time per frame is unchanged within noise; the other labels
(`region.messages`, `row`, diff cards, sidebar, input, derived row layout
compute) moved by at most 0.1 ms. `block.text` did not get cheaper: its
paint fell slightly (fewer elements per paragraph, #3090), but its prepaint
went from 0.02 to 0.17-0.20 ms per frame. That is the `InlineFlow` cost
described above: gpui-component 0.6.6 lays every paragraph with a code span
out as an `InlineFlow`, which shapes its fragments again every frame (a
`div` plus `Inline` per fragment as its own taffy root with a fresh
`InlineState`). `sample` on the wheel sweeps (8 s each, attached at 20 s)
confirms the attribution: `InlineFlow::prepaint` is on the stack in 13 % of
the draw samples after the migration and in none before; `TextLayout::layout`
inclusive went from 7.3 % to 7.9 %, `shape_line` from 7.1 % to 8.3 %, taffy
stayed at 25-26 %.

So the text improvement is
[longbridge/gpui-kit#3180](https://github.com/longbridge/gpui-kit/pull/3180)
(open at the time of writing), not the migration itself.

### With the InlineFlow frame cache (gpui-base patched, 2026-09-22)

The branch consumes the PR through a `[patch.crates-io]` entry for
`gpui-base` (fork branch `perf/inline-flow-frame-cache-0.6.6`, the v0.6.6 tag
plus the PR's commits; the PR was merged upstream on 2026-09-22 with a
follow-up that releases the states of fragments that went away, which the
branch carries too). Measured against the same branch without the
patch, back to back, same session copy and display. Both binaries include
the diff-row font fix (`fix(ui): shape diff rows with the card's text
style`), which makes the diff cards much shorter than in the table above
(the rows were wrapped in the 16 px UI font before), so the sweep covers a
different stretch of the session and these rows are not comparable with the
ones above, only with each other:

| sweep | gpui-base | draw mean / p50 / p95 / max | `block.text` n/frame | layout | prepaint | paint | total (% draw) | derived row layout |
|---|---|---|---|---|---|---|---|---|
| scroll | 0.6.6 | 7.05 / 6.94 / 8.71 / 27.0 ms | 1.68 | 0.24 | 0.45 | 0.31 | 1.02 (14 %) | 1.41 ms |
| scroll | patched | 6.91 / 6.83 / 8.26 / 26.3 ms | 1.68 | 0.26 | 0.27 | 0.32 | 0.88 (13 %) | 1.04 ms |
| wheel | 0.6.6 | 4.57 / 4.57 / 5.80 / 20.2 ms | 1.80 | 0.15 | 0.29 | 0.21 | 0.67 (15 %) | 0.82 ms |
| wheel | patched | 4.32 / 4.29 / 5.54 / 18.4 ms | 1.77 | 0.16 | 0.15 | 0.20 | 0.53 (12 %) | 0.58 ms |

The patch takes 0.14-0.25 ms off every frame: `block.text` prepaint
roughly halves (0.45 to 0.27 ms, 0.29 to 0.15 ms per frame), the derived
row layout compute (the list's `layout_as_root`, which contains the flows'
measure callbacks) drops by 0.25-0.37 ms, and `region.messages` prepaint by
0.4-0.5 ms. What remains in the text prepaint is gpui's `TextLayout::layout`
per fragment (a taffy leaf and closure per frame), which the PR does not
address.
