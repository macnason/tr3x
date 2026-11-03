# Image aspect ratio investigation

Baseline: `5839e69657e9de78419d2505b461f57c1b4cd870`, clean worktree.

## Rendering path

`media::embeds` reads `output.htmlRender.attachmentId`, title and height
(default 800, bounded 120–4000). `Media::ask` wraps the attachment HTML in theme
variables and screenshots Chromium at 728 CSS pixels wide, requested height,
and 2x device scale. This is a static PNG; the title links to interactive T3.

Both HTML snapshots and Markdown local images go through `Media::picture`, then
`chat::picture_lines` reserves terminal rows. Kitty receives explicit `c` and `r`
and stretches the image to that cell rectangle. Rex handles the terminal graphics;
there is no embedded browser in this rendering path. The Lua multiplexer integration
creates terminal panes; it does not size or stretch the image itself.

The supplied font-sheet screenshot alone does not identify whether its source was
an HTML snapshot or a Markdown image. Both share the defective placement path.

## Confirmed defects

1. Before a Rex client attaches, TIOCGWINSZ returned `(rows=24, cols=80, x=0, y=0)`.
   After attachment it returned `(76,116,1856,2736)`. Rex's block `size` API also
   reported 16×36 pixel cells. Caching the initial fallback ratio 0.5 instead of
   16/36 makes uncapped images 12.5% taller in this setup. Placement now reads the
   current window dimensions, using columns and rows from the same ioctl result.
2. Rows were clamped to 60 without reducing columns. The regression fixture
   728×4000 at 90 available columns requested 46 columns and 60 rows, though
   those columns needed 126.37 rows at ratio 0.5. The test failed before the fix.
   Width now shrinks to fit the row budget before height is rounded.

## Visual evidence

Real Rex Beta 0.1.0 (1758) screenshots of a controlled 728×2400 PNG with five
circles. These are graphics sequences produced by the actual `Media::picture`
method, replayed using `cat` in scratch terminal panes, not a full T3 conversation.
Both captures use 16×36 cells and 46 available image columns, isolating the height
cap defect. Before: 46×60 cells; after: 40×59 cells. The source is `circles.png`.

| Before | After |
| --- | --- |
| ![Flattened circles](before.png) | ![Proportional circles](after.png) |

`capture.rs.txt` preserves the temporary test harness used on each revision. Add
it inside `media.rs`'s test module, run the ignored test with
`EVIDENCE_VERSION=before` or `after`, then replay `/tmp/t3rex-aspect/<version>.vt`
in Rex. The harness fixes the cell ratio because Cargo runs without a terminal.
Screenshots were captured from the native app. No user sessions were restarted.

## Validation and limits

- Regression test: failed before, passed after.
- `cargo test --workspace --locked`: 146 client + 138 Rex + 3 integration passed;
  one existing manual test ignored.
- `cargo fmt --all -- --check` and strict workspace Clippy passed.
- Original font-sheet HTML and a cross-device handoff were not replayed.
- Chromium's fixed viewport remains unchanged: responsive HTML can lay out
  differently from its interactive T3 version. This fix preserves the snapshot's
  aspect ratio; it does not infer an intrinsic viewport from arbitrary HTML.
- Terminals without pixel metrics retain the existing 0.5 fallback. Placement
  rounds to whole cells, so sub-cell aspect errors remain.
