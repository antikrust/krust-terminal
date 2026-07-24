# txtFX design and safety audit

Written: 2026-07-21

txtFX is an optional visual-effect system. It is not the terminal renderer and it does not own terminal cells, Agent messages, font shaping, or the GPU scene. The general text-rendering paths are documented separately in [RENDER-DESIGN.md](RENDER-DESIGN.md).

This document is the entry point for changing txtFX effects, colours, spinners, settings, lifecycles, or performance limits.

## Scope and source-of-truth rule

txtFX may alter the rendered appearance of newly produced text. It must never alter the canonical data used by terminal emulation, Agent Mode conversations, copy, search, selection, persistence, export, or secret redaction.

```text
Canonical content                     Render-only txtFX state

terminal GridStorage cells      ->    absolute cell snapshots and batches
Agent Mode text sections        ->    observed streamed suffixes and frames
                                      effect/colour/spinner configuration
                                      repaint deadlines
```

txtFX must remain a downstream, removable overlay. If disabling the setting changes terminal parsing, Agent message handling, copy behaviour, or stored content, the boundary has been broken.

## Current architecture

The central implementation is [app/src/terminal/text_effects/mod.rs](app/src/terminal/text_effects/mod.rs). It contains effect and colour types, settings, sampling, terminal timelines, Agent timelines, automatic spinners, and the private OSC spinner protocol.

Configuration is exposed in [app/src/settings_view/appearance_page.rs](app/src/settings_view/appearance_page.rs). The compact title-bar picker is [app/src/terminal/text_effects/picker.rs](app/src/terminal/text_effects/picker.rs).

### Terminal path

```text
PTY output
  -> ANSI/GridHandler writes canonical cells
  -> record AbsolutePoint for changed cells
  -> end of ANSI byte batch snapshots display content
  -> TextEffectsTimeline batch
  -> grid_renderer samples a visible cell at one frame timestamp
  -> visual copy of the cell, colour, and optional preceding spinner
```

`GridHandler` records a display-independent `AbsolutePoint`, then snapshots the current display content at the end of byte processing. The sampler compares the snapshot fingerprint with the current cell before changing its appearance. A rewritten cell therefore stops matching an old animation rather than showing stale animation data.

Both ligature paths in [app/src/terminal/grid_renderer.rs](app/src/terminal/grid_renderer.rs) call `text_effect_cell` and `render_text_effect_spinner`. Effects must remain consistent with ligatures enabled and disabled, and must be checked in normal block output plus alternate screen.

### Agent Mode path

```text
Agent stream
  -> AIAgentTextSection
  -> AgentTextEffectsTimeline::frame(section, text)
  -> render_text_sections
  -> Text / FormattedTextElement with render-only substitutions and highlights
  -> scheduled repaint while a frame needs one
```

[app/src/ai/blocklist/block/view_impl/common.rs](app/src/ai/blocklist/block/view_impl/common.rs) requests a frame only for a streaming, non-restored, non-selecting Agent block. It preserves the plain-text or formatted-text section shape; it must not flatten Markdown merely to animate it.

[app/src/ai/blocklist/block.rs](app/src/ai/blocklist/block.rs) owns a single timer handle per Agent block. The timer is aborted and timeline cleared when the Agent Mode txtFX setting is disabled.

### Effects, colours, and spinners

Effects control reveal or substitution: immediate, word reveal, typewriter, Matrix trail, decode, case pulse, and word decode. Colours are independent: terminal palette, solid colours, classic cycle, and rainbow. A colour can remain after reveal motion completes.

Spinners are separate from the main effect:

- The automatic spinner follows the newest printed line and is purely a paint-layer prefix.
- A private OSC `9281` protocol supports explicit start, update, and stop spinner lifecycles from terminal output.
- Spinner characters must never be inserted into terminal cells or Agent message strings.

Matrix sequences continue across nearby output batches using row order and a short continuation window. They must not restart merely because one directory listing arrived in several PTY reads.

## Existing bounds and protections

The terminal path currently has explicit limits in `text_effects/mod.rs`:

| Protection | Current value | Purpose |
| --- | ---: | --- |
| Pending terminal writes | 4,096 | Limits a captured terminal byte batch |
| Retained terminal batches | 8 | Bounds terminal timeline history |
| Active explicit spinners | 64 | Bounds OSC spinner state |
| Explicit spinner FPS | 1–60 | Prevents unbounded redraw requests |
| Matrix continuation window | 250 ms | Joins adjacent stream reads only |
| Automatic spinner linger | 350 ms | Stops after output settles |
| Automatic spinner maximum | 4 s | Bounds spinner duration for long effects |

These are useful safeguards, but they do not currently apply consistently to the Agent Mode timeline.

## Current audit — 2026-07-21

The focused suite passes: `cargo test -p derp text_effects` ran 24 tests successfully. It covers the effect samplers, Matrix stream continuity, terminal cell snapshotting, colour persistence, automatic spinners, OSC spinner parsing/lifecycle, and a basic Agent suffix case.

The audit did not find an uncontrolled direct panic from terminal input. The `expect` calls in the timeline are guarded by non-empty batch checks or fixed non-empty spinner-frame tables; explicit spinner IDs and FPS values are bounded before being stored.

It did find three current load risks that can make the UI unresponsive or exhaust resources under enough output. They are plausible crash causes, but they have not been tied to the earlier crash without its backtrace.

### 1. Collection continues while txtFX is visually off

`GridHandler` records writes and builds timeline batches unconditionally in [app/src/terminal/model/grid/ansi_handler.rs](app/src/terminal/model/grid/ansi_handler.rs) and [app/src/terminal/model/grid/grid_handler.rs](app/src/terminal/model/grid/grid_handler.rs). The enabled setting is consulted later, when the grid is painted.

That means disabling txtFX stops visible effects but does not stop point recording, fingerprint string allocation, batch construction, or retention of recent batches. The terminal bounds prevent unbounded growth, but this is unnecessary work during high-output commands and contradicts the expectation that “off” has no txtFX cost.

### 2. Agent Mode capture is unbounded and lookup is quadratic

`AgentTextEffectsTimeline::frame` builds a `written_cells` entry for every character in an appended Agent delta. Unlike the terminal path, it does not apply the 4,096-cell capture cap before allocating and storing the batch.

The same frame then samples every character. `TextEffectBatch::cell` performs a linear search of its `Vec<AnimatedCell>`, so one large appended section can require approximately quadratic work: each of *n* visible characters searches up to *n* recorded cells. The `large_output` preference checks the batch size only after that lookup and allocation have already happened.

This is the highest-risk current path for a freeze or memory-pressure termination when Agent Mode txtFX is enabled and a provider sends a large response or a large delta.

### 3. Dynamic colours can keep completed content repainting forever

Classic cycle and rainbow set `needs_repaint` even after their reveal effect completes. Terminal output retains up to eight batches, and Agent Mode's timer repeatedly scans all stored section text while a dynamic colour is active.

That behaviour keeps cycling colour visible, but it also creates continuous CPU and layout work after a response has finished. For a long Agent reply, the repeated full-section scan magnifies the second issue.

## Required test gaps

The current passing tests are necessary but do not cover the three risks above. Before changing txtFX, add focused tests for:

- no terminal batch capture or retention while the feature is disabled;
- a large Agent delta, including allocation/capture cap and bounded lookup behaviour;
- completed dynamic-colour output reaching a defined idle state, or an explicit user-selected reason to keep cycling;
- Unicode spinner rendering in both terminal ligature paths;
- normal output and alternate screen with txtFX enabled;
- Agent plain text and formatted Markdown while streaming, restored, selected, and copied.

Do not launch the app solely to test this. Build-only and unit/integration validation are sufficient until the system administrator explicitly authorizes running it.

## Recommended remediation order

No code change is made by this audit. These would be behavioural/performance changes and require project-owner approval before implementation.

1. Make the disabled state a capture gate so the terminal timeline is cleared and does no per-write work when txtFX is off.
2. Apply a hard Agent capture limit before allocating `written_cells`, and replace per-character linear batch lookup with indexed lookup or an equivalently bounded representation.
3. Decide the intended lifecycle for classic-cycle and rainbow after a reveal completes. A final settled colour preserves the user's colour choice without a permanent repaint loop; continuous cycling should be an explicit, separately bounded mode.
4. Add regression tests for every chosen bound and idle transition before re-enabling Agent Mode txtFX by default.

## Invariants for future changes

- Effects and colours are separate settings; do not couple a colour choice to one animation.
- The visual layer must retain original terminal and Agent text for copy, search, selection, and persistence.
- Secret redaction must win before any txtFX substitution, highlight, spinner, tooltip, or accessibility content is emitted.
- Keep terminal `Point` and scrollback `AbsolutePoint` distinct. Wide cells, zero-width extensions, and grapheme clusters are not one-character-per-cell data.
- Use one timestamp per terminal paint so cells agree on the same frame.
- Request a repaint only until the visual result can change. Never sleep on the UI thread or repaint at maximum rate.
- Handle normal output, commands, alternate screen, ligatures on/off, and Agent rich text deliberately; they are separate render paths.
- Local harness messages remain provider-neutral and use Derp's normal conversation machinery. txtFX must not introduce provider-specific renderer branches.

## Where to make a change

| Desired change | Start here |
| --- | --- |
| Add an effect, colour mode, spinner, or safety bound | `app/src/terminal/text_effects/mod.rs` |
| Record terminal writes or batches | `app/src/terminal/model/grid/ansi_handler.rs` and `grid_handler.rs` |
| Paint terminal effect cells/spinners | `app/src/terminal/grid_renderer.rs` |
| Change txtFX appearance controls | `app/src/settings_view/appearance_page.rs` |
| Change title-bar picker | `app/src/terminal/text_effects/picker.rs` |
| Change Agent Mode decoration or scheduling | `app/src/ai/blocklist/block/view_impl/common.rs` and `block.rs` |
| Change general renderer or text layout | [RENDER-DESIGN.md](RENDER-DESIGN.md) |

## Validation

Start with:

```sh
cargo test -p derp text_effects
cargo build --bin derp
```

For a code change that will be pushed, also run the repository's required formatting and Clippy checks. Visual validation, when explicitly authorized, must cover ligatures on and off, normal and alternate terminal screens, output split across multiple reads, large output, secrets, Unicode/wide glyphs, Agent streaming, restored Agent messages, selection/copy, and the disabled state.
