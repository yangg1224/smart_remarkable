# Select Mode

Select a region of handwriting, get an LLM answer drawn into a box of your
choosing. Because the answer is real pen strokes, you can afterwards move and
resize it with reMarkable's native selection (lasso) tool.

## How to use on the device

Run with:

```sh
ANTHROPIC_API_KEY=sk-... ./smart_remarkable --select-mode
```

Then, in a notebook, use your **finger** (not the pen):

1. **Tap the upper-right corner** of the screen to arm the assistant
   (change with `--trigger-corner`).
2. **Tap two opposite corners** of the handwriting you want answered
   (e.g. top-left then bottom-right of your question). A minimum box size
   of 40 px is enforced, so imprecise taps are fine.
3. **Tap two opposite corners** of where the answer should go.
4. Wait: the selected region is cropped from a screenshot, sent to the
   vision LLM (`claude-sonnet-4-6` by default, `-m`/`--engine` to change),
   and the answer is scaled into your box and drawn as pen strokes.
5. To move or resize the answer afterwards, use xochitl's own selection
   tool — the strokes are part of the page and sync like normal ink.

There is no on-screen guidance between taps; the sequence is always
trigger → 2 selection taps → 2 placement taps. Watch the log output over
SSH (`--log-level debug`) when learning the flow.

## Deploying to a reMarkable Paper Pro

Requires Developer mode (Settings → General → Software → Advanced —
enabling it factory-resets the device and voids the warranty).

```sh
# Build (macOS host, no Docker):
#   brew tap messense/macos-cross-toolchains && brew install aarch64-unknown-linux-gnu
#   rustup target add aarch64-unknown-linux-gnu
CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-unknown-linux-gnu-gcc \
  cargo build --release --target aarch64-unknown-linux-gnu

# Copy and run (device IP shown in Settings → Help → About, password too)
scp target/aarch64-unknown-linux-gnu/release/smart_remarkable root@<device-ip>:
ssh root@<device-ip>
ANTHROPIC_API_KEY=sk-... ./smart_remarkable --select-mode
```

The bundled uinput kernel module is loaded automatically (prebuilt for
OS 3.16–3.18; other versions may need a rebuilt module — see `utils/rmpp/`).

## LLM button and Draw button

Lassoing text with xochitl's own selection tool shows two extra buttons
beside the usual cut/copy/paste menu, injected by `xovi-ext/llmbutton`:

- **LLM** — answers the selection (same flow as above), using
  `prompts/selection.json` and the `draw_answer` tool.
- **Draw** — writes to its own trigger file (`/tmp/draw_button_trigger`) and
  uses `prompts/draw.json` with the `draw_sketch` tool instead. The model
  looks at the selection and reports which of two cases it is (via the
  tool's `selection_is_drawing` argument), which changes both the artwork
  style and where it's drawn:
  - **Selection is mostly text** → the model invents a small pencil-scratch
    doodle illustrating it, drawn as new content in the answer-placement box
    below/above the selection (same placement behavior as the LLM button).
  - **Selection is already a drawing/sketch** → the model redraws an
    improved, more detailed version of the *same* subject (ornate
    single-weight line art; see `prompts/draw.json` for the full style
    spec), and the app **erases the original ink first** and draws the
    refined version into that *same* box in place, rather than adding a
    second copy elsewhere.

Which button fired is tracked as `touch::TriggerSource` (`Touch`, `LlmButton`,
`DrawButton`) — set as a side channel on `Touch` by `wait_for_real_trigger`
when it consumes one of the two trigger files, then read via
`Touch::last_trigger_source()` in `coordinator::trigger_task` and threaded
through `TriggerEvent`/`processing_task`, which picks `draw.json` over the
configured `--prompt` only when the source is `DrawButton`. The original
selection rect is also threaded through as a `selection_slot` (parallel to
the existing `placement_slot`), so the `draw_sketch` tool callback in
`main.rs` can pick the original box instead of the placement box when
`selection_is_drawing` is true.

### Image-generation mode (`--image-model`, "nano banana")

LLM-authored SVG tops out at schematic, cartoonish line art — a text model
writing path data can't produce concept-sketch-quality curves. Passing
`--image-model` (default value `gemini-2.5-flash-image`, Google's
"nano banana") reroutes the Draw button through an image-generation model:

1. The chat LLM still classifies the selection (text vs. drawing) but now
   writes an *image-generation prompt* instead of SVG
   (`prompts/draw_image.json` + `prompts/tool_draw_sketch_image.json`,
   registered under the same `draw_sketch` tool name).
2. `src/image_gen.rs` calls the Gemini API with that prompt. For a
   drawing selection, the cropped screenshot of the user's rough sketch is
   attached (armed via `input_image_slot`, parallel to the placement slots),
   so the model refines the actual sketch rather than imagining one. A
   strict style suffix (pure black line art, white background, no
   gray/stipple/fills) is appended in Rust so every generation is traceable.
3. The returned PNG goes through `util::image_to_ink_bitmap` (grayscale,
   ≤1024 px, threshold) and `Pen::draw_bitmap_centerline` (Zhang–Suen
   thinning → skeleton tracing → speck filter → uniform fit into the
   target box) — the same skeleton pipeline the SVG path uses.
4. In-place erase happens only *after* generation succeeds, so an API
   failure can't destroy the user's original sketch.

Needs `GEMINI_API_KEY` or `GOOGLE_API_KEY` (or `--image-api-key`), separate
from the chat model's key:

```sh
ANTHROPIC_API_KEY=sk-... GEMINI_API_KEY=... ./smart_remarkable --select-mode --image-model
```

Offline pipeline check without any API key (traces an existing image the
same way the device would draw it):

```sh
TRACE_IMAGE_INPUT=sketch.png TRACE_IMAGE_OUTPUT=out.png \
  cargo test --test trace_image -- --nocapture
```

### Erasing before an in-place redraw

Erasing turned out to need the pen's actual hardware eraser-tip signal:
xochitl only erases ink in response to a `BTN_TOOL_RUBBER` stroke (the
signal a stylus sends when flipped to its eraser end) — a normal
`BTN_TOOL_PEN` stroke never erases, regardless of which tool is selected in
the on-screen toolbar (confirmed on-device 2026-07-08: selecting the
toolbar eraser icon and then stroking with `BTN_TOOL_PEN` silently does
nothing). `Pen::erase_rect` sweeps horizontal `BTN_TOOL_RUBBER` passes
across the box (see `Pen::draw_line_rubber_screen`); no toolbar tool switch
is needed at all before calling it.

## Implementation notes

- Trigger + corner taps are collected in `trigger_task`
  (`src/coordinator.rs::collect_selection`) while it holds the touch event
  stream, and shipped as `TriggerEvent::UserSelection`.
- The screenshot is cropped to the selection (`Screenshot::base64_cropped`)
  before being sent to the model, with the `prompts/selection.json` prompt
  (or `prompts/draw.json` for the Draw button).
- The model's SVG answer is fitted into the placement box by
  `util::fit_svg_to_rect`: the ink bounding box is measured by rasterizing,
  then the SVG is wrapped in a uniform scale + translate (centered).
  Unit tests: `cargo test --lib util`.
