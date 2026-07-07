# Select Mode

Select a region of handwriting, get an LLM answer drawn into a box of your
choosing. Because the answer is real pen strokes, you can afterwards move and
resize it with reMarkable's native selection (lasso) tool.

## How to use on the device

Run with:

```sh
ANTHROPIC_API_KEY=sk-... ./ghostwriter --select-mode
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
scp target/aarch64-unknown-linux-gnu/release/ghostwriter root@<device-ip>:
ssh root@<device-ip>
ANTHROPIC_API_KEY=sk-... ./ghostwriter --select-mode
```

The bundled uinput kernel module is loaded automatically (prebuilt for
OS 3.16–3.18; other versions may need a rebuilt module — see `utils/rmpp/`).

## Implementation notes

- Trigger + corner taps are collected in `trigger_task`
  (`src/coordinator.rs::collect_selection`) while it holds the touch event
  stream, and shipped as `TriggerEvent::UserSelection`.
- The screenshot is cropped to the selection (`Screenshot::base64_cropped`)
  before being sent to the model, with the `prompts/selection.json` prompt.
- The model's SVG answer is fitted into the placement box by
  `util::fit_svg_to_rect`: the ink bounding box is measured by rasterizing,
  then the SVG is wrapped in a uniform scale + translate (centered).
  Unit tests: `cargo test --lib util`.
