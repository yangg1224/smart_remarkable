# AGENTS.md

This file provides guidance to Claude Code (claude.ai/code) and other similar tools when working with code in this repository.

## Project Overview

Ghostwriter is a Vision-LLM agent for the reMarkable tablet that watches handwritten input and responds by drawing or typing back to the screen. It takes screenshots, sends them to LLM APIs (OpenAI, Anthropic, Google), and uses the responses to interact with the device through simulated pen and keyboard input.

## Core Architecture

- **main.rs**: Main application entry point with CLI argument parsing and orchestration
- **LLM Engine Layer** (`src/llm_engine/`): Pluggable backends for different AI providers
  - `openai.rs`: OpenAI API integration (GPT models)
  - `anthropic.rs`: Anthropic API integration (Claude models) 
  - `google.rs`: Google API integration (Gemini models)
  - `mod.rs`: Common LLM engine trait and interface
- **Device Interaction** (`src/`):
  - `screenshot.rs`: Screen capture functionality for both reMarkable2 and Paper Pro
  - `pen.rs`: SVG drawing and bitmap rendering to screen 
  - `keyboard.rs`: Virtual keyboard input via uinput
  - `touch.rs`: Touch event detection and gesture recognition
- **Image Processing**:
  - `segmenter.rs`: Image segmentation to provide spatial context to LLMs
  - `util.rs`: SVG to bitmap conversion and other utilities
- **Configuration**:
  - `prompts/`: JSON-based prompt templates and tool definitions
  - `embedded_assets.rs`: Bundled configuration files

## Common Development Commands

### Building
```bash
# Local development build
cargo build --release
# or
./build.sh local

# Cross-compile for reMarkable2 (armv7)
cross build --release --target=armv7-unknown-linux-gnueabihf
# or
./build.sh

# Cross-compile for reMarkable Paper Pro (aarch64)  
cross build --release --target=aarch64-unknown-linux-gnu
# or
./build.sh rmpp
```

### Code Quality and Formatting
```bash
# Format code with rustfmt
cargo fmt

# Check formatting without applying changes
cargo fmt -- --check

# Run clippy linting
cargo clippy

# Run clippy with stricter warnings
cargo clippy -- -D warnings

# Check code compiles
cargo check --all-targets --all-features
```

### Testing and Evaluation
```bash
# Run evaluation suite across multiple models and configurations
./run_eval.sh

# Test with local input file (no device required)
./target/release/ghostwriter \
  --input-png evaluations/test_case/input.png \
  --output-file tmp/result.out \
  --save-bitmap tmp/result.png \
  --no-draw --no-loop --no-trigger

# Run with specific model and options
./ghostwriter --model gpt-4o-mini --apply-segmentation --thinking
```

### Deployment
```bash
# Deploy to reMarkable (replace IP address)
scp target/armv7-unknown-linux-gnueabihf/release/ghostwriter root@192.168.1.117:
# or for Paper Pro
scp target/aarch64-unknown-linux-gnu/release/ghostwriter root@192.168.1.117:
```

## Key Development Notes

### Cross-compilation Setup Required
- Install `cross`: `cargo install cross --git https://github.com/cross-rs/cross`
- Add targets: `rustup target add armv7-unknown-linux-gnueabihf aarch64-unknown-linux-gnu`
- Docker required for cross-compilation

### Device-Specific Considerations
- reMarkable2 uses armv7 architecture, Paper Pro uses aarch64
- Paper Pro requires uinput kernel module to be loaded (handled automatically)
- Screen resolutions and input handling differ between devices
- Virtual display size is normalized to 768x1024 pixels

### Environment Variables
Set API keys for different LLM providers:
```bash
export OPENAI_API_KEY=your-key-here
export ANTHROPIC_API_KEY=your-key-here
export GOOGLE_API_KEY=your-key-here
```

### Prompt System
- Prompts are defined in `prompts/` directory as JSON files
- Tools (draw_text, draw_svg) are defined separately and referenced by prompts
- Runtime prompt overrides supported by copying files to device
- Default prompt is `general.json`

### Testing and Evaluation Framework
- `evaluations/` contains test scenarios with input images
- `run_eval.sh` runs systematic evaluations across models and configurations
- Results include merged images showing original input + AI output in red
- Supports segmentation analysis for improved spatial awareness

### reMarkable Integration
- Touch trigger in upper-right corner activates the assistant
- Progress indication through keyboard text and screen taps
- Supports both SVG drawing (via pen simulation) and text output (via virtual keyboard)
- Background execution supported with `nohup ./ghostwriter &`
