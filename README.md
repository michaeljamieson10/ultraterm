# ultraterm

`ultraterm` is a macOS-only, GPU-rendered terminal emulator in Rust focused on low keypress-to-pixels latency and smooth output throughput.

## Status

This repository is an MVP optimized for:

1. Low input-to-render latency
2. Correctness for common CLI apps and Neovim workflows
3. Clean architecture for profiling and iteration

## Build and Run (macOS only)

Requirements:

- macOS 13+
- Xcode Command Line Tools (`xcode-select --install`)
- Rust stable (`rustup default stable`)

Build:

```bash
cargo build --release
```

Run:

```bash
cargo run --release
```

Run stress mode (spawns a high-output command in the PTY):

```bash
cargo run --release -- --stress
```

Run headless terminal self-tests (no window; validates basic PTY + parser/screen behavior):

```bash
cargo run --release -- --headless-self-test
```

No special system permission prompts are required for basic operation.

## Controls

- `Cmd+Shift+P`: toggle profiling overlay (FPS, frame ms, dirty rows, PTY KB/s)
- `Cmd+Shift+S`: run stress command (`yes ...`) inside current shell
- Mouse drag: text selection (when terminal mouse reporting is disabled)
- `Cmd+C`: copy current selection (falls back to sending `Ctrl-C` if no selection)
- `Cmd+V`: paste clipboard text (uses bracketed paste when enabled by app)

## Architecture

- `src/pty.rs`
  - PTY spawn/resize/read/write using `portable-pty`
  - Login shell launch (`/bin/zsh` preferred; fallback to `$SHELL`, then `/bin/sh`)
  - Reader thread and chunked byte channel

- `src/parser.rs`
  - ANSI/VT parser using `vte`
  - CSI/ESC handling for cursor movement, erase, color, scrolling, alternate screen, bracketed paste, mouse modes, cursor style
  - Basic host responses for queries like DSR/DA

- `src/screen.rs`
  - Flat cell storage (`Vec<Cell>`) and row indirection map
  - Damage tracking by row
  - Scrollback ring buffer (no large scrollback memmoves)
  - Cursor + attributes + scroll region support

- `src/renderer/mod.rs`
  - `wgpu` renderer (Metal backend on macOS)
  - One instanced quad pipeline for backgrounds, glyphs, selection, overlay
  - Glyph atlas cache with on-demand rasterization (`fontdue`)
  - Dirty-row upload path to reduce CPU/GPU traffic

- `src/input.rs`
  - Key mapping (arrows/function keys/modifiers/app cursor mode)
  - IME-safe text commit path support
  - Bracketed paste wrapping and mouse report encoders

- `src/app.rs`
  - Event loop orchestration, PTY I/O coalescing, parser integration
  - Resize propagation, copy/paste, selection, profiler stats

## TERM and Color

`ultraterm` sets:

- `TERM=xterm-256color`
- `COLORTERM=truecolor`

## Performance Notes

- Screen grid is POD-like `Cell` in flat vectors
- No per-cell heap allocation in hot render/parsing path
- PTY output is coalesced in the app loop before parsing/redraw
- Renderer updates only dirty rows
- Glyphs rasterize only on atlas misses

## Known Limitations (MVP)

- Unicode shaping/grapheme clustering is basic (single-char cells, width via `unicode-width`)
- No scrollback viewport navigation UI yet
- Cursor rendering style is approximate (block/beam/underline)
- Legacy mouse protocol support is minimal compared to full xterm behavior
- Some advanced DEC/private modes are ignored safely
