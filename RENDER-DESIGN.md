# Text rendering design

Written: 2026-07-19

This is the newcomer map for text rendering in 0term. Read it before changing how terminal output or Agent Mode output is laid out or painted. It describes the GUI desktop application. The headless TUI has a separate cell-grid renderer and is called out explicitly below.

txtFX is an optional consumer of these rendering paths, not part of their design. Its state, settings, safety limits, and validation live in [TXTFX-DESIGN.md](TXTFX-DESIGN.md).

If the change touches local Agent Mode message creation or lifecycle, read [docs/local-harness-lifecycle.md](docs/local-harness-lifecycle.md) first. That protocol is not part of the renderer and must not be bypassed to make a visual feature work.

## TL;DR

There are two main sources of text. They converge at KrustUI's layout and scene layers, not at the data-model layer.

```text
Terminal

PTY bytes
  -> ANSI processor
  -> TerminalModel / GridHandler
  -> GridStorage cells
  -> BlockGrid or alternate-screen element
  -> terminal::grid_renderer
  -> shaped glyphs and terminal cell primitives
  -> KrustUI Scene
  -> platform GPU renderer

Agent Mode

Provider process or cloud stream
  -> provider-neutral events / API messages
  -> conversation and exchange models
  -> AIAgentTextSection values
  -> Text or FormattedTextElement
  -> KrustUI text layout cache and platform shaper
  -> KrustUI Scene
  -> platform GPU renderer
```

The most important rule is that the stored terminal cells and Agent messages are the source of truth. Search highlights, selection, cursor decoration, and optional visual overlays belong in rendering state. Rendering must not rewrite source text. Copy, search, selection, persistence, and accessibility should continue to see the original content.

## First files to read

For most rendering work, start here in this order:

1. [app/src/terminal/grid_renderer.rs](app/src/terminal/grid_renderer.rs) — paints terminal cells, glyphs, decorations, images, cursor state, and search state.
2. [app/src/terminal/model/grid/grid_handler.rs](app/src/terminal/model/grid/grid_handler.rs) — owns canonical grid-side state.
3. [app/src/ai/blocklist/block/view_impl/common.rs](app/src/ai/blocklist/block/view_impl/common.rs) — shared Agent Mode text-section renderer.
4. [crates/krustui_core/src/elements/gui/text.rs](crates/krustui_core/src/elements/gui/text.rs) and [crates/krustui_core/src/elements/gui/formatted_text_element.rs](crates/krustui_core/src/elements/gui/formatted_text_element.rs) — generic GUI plain-text and rich-text elements.

Then follow the narrower paths in the module map below.

## Source-of-truth boundaries

The renderer sits downstream of several models. Keep their responsibilities separate:

| Layer | Owns | Must not own |
| --- | --- | --- |
| ANSI/model | Parsed terminal state, cells, cursor, modes, images, secrets, scrollback | Per-frame visual animation |
| Conversation model | Agent messages, tool calls, reasoning, status | Provider-specific paint hacks |
| View element | Layout, clipping, selection, highlights, hover/click state | Protocol lifecycle or persistent message mutation |
| KrustUI layout | Shaping, wrapping, fallback fonts, glyph positions | Terminal escape-sequence meaning |
| Scene/renderer | Draw primitives submitted for the frame | Application semantics |

Feature-specific overlay state is kept separate from these canonical models so it cannot alter their data or lifecycle.

## Terminal rendering path

### 1. PTY bytes become terminal operations

Terminal output arrives in batches of PTY bytes. The ANSI processor in [app/src/terminal/model/ansi/mod.rs](app/src/terminal/model/ansi/mod.rs) parses printable characters and control sequences, then calls the active handler. [app/src/terminal/model/terminal_model.rs](app/src/terminal/model/terminal_model.rs) handles terminal-wide events and delegates grid mutations to `GridHandler`.

Do not assume that one PTY read is one line or command. A shell can split one line across reads or put many lines in one read. Synchronized-output handling can also call `on_finish_byte_processing` once for a flushed synchronized frame and once for the original raw bytes.

### 2. GridHandler updates the canonical cell grid

[app/src/terminal/model/grid/grid_handler.rs](app/src/terminal/model/grid/grid_handler.rs) owns `GridStorage` plus grid-adjacent state such as ANSI state, displayed-output filtering, images, marked text, the dirty range, and secret metadata.

Printable writes ultimately pass through [app/src/terminal/model/grid/ansi_handler.rs](app/src/terminal/model/grid/ansi_handler.rs). At the end of byte processing, the handler updates dirty ranges, scans for secrets, and performs filtering work.

Terminal coordinates require care:

- `Point` is relative to the currently displayed grid.
- `AbsolutePoint` identifies content in scrollback and survives viewport movement.
- A rendered row and column become pixels only after applying the grid origin, cell size, padding, and clipping bounds.
- Wide characters occupy a primary cell and a spacer cell. Zero-width characters extend an existing grapheme. Never treat every cell as one independent Unicode scalar.

### 3. A surface chooses which grid rows to paint

Normal block output is prepared by [app/src/terminal/blockgrid_renderer.rs](app/src/terminal/blockgrid_renderer.rs). It computes visible rows, clipping, padding, obfuscation, find state, cursor/image state, and then calls the shared grid renderer.

Full-screen terminal applications use [app/src/terminal/alt_screen/alt_screen_element.rs](app/src/terminal/alt_screen/alt_screen_element.rs). The alternate screen calls the same core `render_grid` function, but its viewport and surrounding UI are different. Any terminal visual feature must be checked in both normal scrollback and alternate-screen applications.

### 4. grid_renderer paints cells

The main entry point is `render_grid` in [app/src/terminal/grid_renderer.rs](app/src/terminal/grid_renderer.rs). It selects the ligature or non-ligature path.

The two paths are intentionally different:

- `render_grid_without_ligatures` can paint individual cells directly.
- `render_grid_with_ligatures` groups compatible cell content so the shaper can form ligatures.

A feature implemented in only one path will appear to switch off with the ligature preference. Shared visual decisions should be computed consistently, with path-specific glyph emission only where shaping demands it.

The renderer combines several kinds of state:

- foreground and background colours from the cell and theme;
- bold, dim, italic, underline, strike-through, inverse, and hidden flags;
- selections, search matches, marked text, and cursor styling;
- secret masking and block obfuscation;
- images and non-text cell decorations.

`render_cell_glyph` and the attributed-string path convert display content into shaped glyphs.

### 5. Glyphs enter the KrustUI scene

Terminal glyph work is cached by [app/src/terminal/grid_renderer/cell_glyph_cache.rs](app/src/terminal/grid_renderer/cell_glyph_cache.rs). The grid renderer emits glyphs, rectangles, images, and decorations into the current KrustUI `Scene`; [crates/krustui_core/src/scene.rs](crates/krustui_core/src/scene.rs) defines that display list. The platform renderer consumes the scene and draws the final frame on the GPU.

## Agent Mode rendering path

### 1. Provider output becomes Krust messages

For local harness conversations, provider parsers produce provider-neutral `TurnEvent` values. The mapper converts them to Krust API messages, and `LocalHarnessStream` replays those messages through Krust's normal conversation machinery. See [docs/local-harness-lifecycle.md](docs/local-harness-lifecycle.md) for the exact handshake and lifecycle ownership.

Provider differences must stop at the parser. The renderer should receive ordinary Krust messages and must not branch on “Claude text”, “Codex text”, or local-harness content. A renderer special case usually means the message shape or feature plumbing is wrong.

### 2. Conversation blocks become text sections

Agent blocks turn their content into `AIAgentTextSection` values. Shared rendering lives in [app/src/ai/blocklist/block/view_impl/common.rs](app/src/ai/blocklist/block/view_impl/common.rs), principally `render_text_sections`.

The section type determines the element:

- plain text uses KrustUI `Text`;
- parsed Markdown/rich text uses `FormattedTextElement`;
- code, tables, images, Mermaid, tool calls, reasoning, and other structured sections have dedicated renderers or containers.

Relevant call sites include [app/src/ai/blocklist/block/view_impl/output.rs](app/src/ai/blocklist/block/view_impl/output.rs) and [app/src/ai/blocklist/block/cli.rs](app/src/ai/blocklist/block/cli.rs).

Do not flatten structured content into a string to achieve a presentation change. That loses links, inline-code styling, wrapping information, selection mappings, and block semantics. Preserve the section structure and use the appropriate element or paint layer.

### 3. Plain and formatted text use KrustUI layout

[crates/krustui_core/src/elements/gui/text.rs](crates/krustui_core/src/elements/gui/text.rs) implements the normal GUI `Text` element, including wrapping, selection, highlights, and pointer interaction.

[crates/krustui_core/src/elements/gui/formatted_text_element.rs](crates/krustui_core/src/elements/gui/formatted_text_element.rs) implements rich text and Markdown layout, including styled spans, inline code, hyperlinks, and per-line state.

Both ultimately use the layout cache in [crates/krustui_core/src/text_layout.rs](crates/krustui_core/src/text_layout.rs). The cache keys layout by content and style inputs, asks the platform text system for missing-glyph fallback, and returns positioned glyph runs for scene painting.

## KrustUI text and scene layers

KrustUI elements have separate layout and paint phases. Layout establishes bounds, wrapping, line metrics, and glyph positions. Paint adds primitives to a `Scene` using those results.

The platform shaping implementations live under:

- [crates/krustui/src/platform/mac/text_layout.rs](crates/krustui/src/platform/mac/text_layout.rs) for the native macOS path;
- [crates/krustui/src/windowing/winit/fonts.rs](crates/krustui/src/windowing/winit/fonts.rs) for the winit path;
- [crates/krustui_core/src/fonts/text_layout_system.rs](crates/krustui_core/src/fonts/text_layout_system.rs) for the shared interface.

Change these only when the problem is genuinely shaping, font fallback, or glyph metrics. Terminal cell styling belongs in `grid_renderer`; Agent Markdown styling belongs in its section/element renderer. Moving a surface-specific rule into the platform shaper affects unrelated UI throughout the app.

## Coordinate and indexing rules

Rendering bugs frequently come from crossing coordinate systems:

| Surface | Logical coordinate | Render coordinate |
| --- | --- | --- |
| Terminal model | `AbsolutePoint` in scrollback storage | display `Point`, then row/column plus cell metrics |
| Terminal viewport | displayed row/column | pixels under grid origin and clip bounds |
| Agent plain text | string/section indices | wrapped layout lines and glyph positions |
| Agent rich text | parsed spans and source ranges | formatted lines, runs, and inline elements |
| KrustUI | element-local bounds | scene/window coordinates after transforms |

Unicode adds another mapping layer. Byte indices, Rust `char` indices, grapheme clusters, shaped glyphs, and terminal cells are not interchangeable. Test combining marks, emoji variation selectors, double-width CJK characters, ligatures, RTL text where supported, and fallback fonts whenever index-to-glyph behaviour changes.

## Invariants and known traps

### Preserve the running block-list grid origin

In [app/src/terminal/block_list_element.rs](app/src/terminal/block_list_element.rs), painting maintains one running `grid_origin`. `draw_block(&mut grid_origin, ...)` mutates it so the next item starts below the previous one. Do not shadow or reset that value inside the item loop; doing so paints subsequent blocks over one another.

### Never bypass local conversation machinery

Local Agent Mode output must use the normal API message representation, feature flags, conversation status, and block renderer. Local conversations must never leak into cloud request paths. `LocalHarnessStream` owns the `InProgress` to `Success` lifecycle.

### Keep secrets ahead of rendering

Secret masking and obfuscation must remain effective before text is emitted. A transformed render string, tooltip, accessibility label, or debug output must not reveal the original secret.

### Keep terminal model lock scopes short

Do not add a new `TerminalModel::lock()` without checking the call stack. A caller may already hold the same lock, and a second acquisition can freeze the UI. Prefer passing the already locked model or extracting the small immutable render snapshot needed by the view.

### Verify both terminal shaping paths

Ligatures on and off exercise different grid-rendering functions. Also verify normal block output and alternate screen. A successful test in one combination is not sufficient.

### The TUI is a different front end

The headless TUI lives in [crates/krust_tui](crates/krust_tui) and uses the cell-grid elements under [crates/krustui_core/src/elements/tui](crates/krustui_core/src/elements/tui). It does not use the GUI GPU scene, `grid_renderer`, `Text`, or `FormattedTextElement` paths described above. TUI rendering changes should follow the repository's `tui-ui-guidelines`, `tui-testing`, and `tui-verify-change` skills.

## Where to make a change

| Desired change | Start here |
| --- | --- |
| Interpret a terminal escape/control sequence | `app/src/terminal/model/ansi/` and the relevant handler |
| Change canonical terminal cell state | `app/src/terminal/model/grid/` |
| Change how terminal cells look | `app/src/terminal/grid_renderer.rs` |
| Change normal-output viewport/clipping | `app/src/terminal/blockgrid_renderer.rs` |
| Change alternate-screen layout | `app/src/terminal/alt_screen/alt_screen_element.rs` |
| Change Agent plain/rich section presentation | `app/src/ai/blocklist/block/view_impl/common.rs` |
| Change a specific Agent output block | `app/src/ai/blocklist/block/view_impl/output.rs` or its dedicated block renderer |
| Change generic GUI plain text | `crates/krustui_core/src/elements/gui/text.rs` |
| Change Markdown/rich text layout | `crates/krustui_core/src/elements/gui/formatted_text_element.rs` |
| Change shared shaping/cache behaviour | `crates/krustui_core/src/text_layout.rs` |
| Change platform font fallback or glyph shaping | the platform text-layout implementation |
| Change headless terminal UI rendering | `crates/krust_tui` and `crates/krustui_core/src/elements/tui` |

## Safe change recipes

### Change a message shape

1. Make provider-specific changes in the provider parser.
2. Map the provider-neutral event to Krust's existing message representation.
3. Keep lifecycle ownership in `LocalHarnessStream`.
4. Run `cargo test -p krust turn_events_to_conversation` before building. A malformed injected message can permanently wedge the conversation.

### Change generic text shaping

1. Reproduce the issue with the smallest `Text` or `FormattedTextElement` case.
2. Decide whether it is content styling, layout/cache input, platform shaping, or font fallback.
3. Avoid terminal-specific conditions in shared layout code.
4. Test Unicode clusters, fallback fonts, wrapping, selection, and both supported platform paths affected by the change.

## Verification checklist

Run the narrow tests first, then broaden according to the risk of the change:

```sh
# Required whenever local-harness message shapes change
cargo test -p krust turn_events_to_conversation

# Build this fork's GUI binary
cargo build --bin krust

# Repository-wide formatting, linting, and tests before review/push
./script/presubmit
```

For GUI changes, visual verification is part of the test, not an optional last look. Check the relevant combinations:

- ligatures enabled and disabled;
- normal terminal blocks and alternate screen;
- live streaming and restored Agent conversations;
- selection, copy, search matches, cursor, links, and secrets;
- fast one-line output and large multi-read output;
- ASCII, combining marks, emoji, CJK wide glyphs, and fallback fonts;
- reduced motion or equivalent appearance settings where applicable.

When a bug appears only in the packaged application, verify that the bundle was built from the same clean commit being inspected. Rendering code in a dirty checkout can mask committed behaviour and produce misleading comparisons.

## Mental model for debugging

Start at the first place the result becomes wrong:

1. Is the canonical terminal cell or Agent message already wrong? Inspect parsing/mapping and the model.
2. Is the model right but the section/cell render input wrong? Inspect surface preparation.
3. Is the render input right but layout wrong? Inspect wrapping, shaping, font fallback, and coordinate conversion.
4. Is layout right but pixels wrong? Inspect scene primitives, clipping, transforms, and the platform renderer.

Do not start by patching the last visible layer. Following the first incorrect boundary keeps the fix local and preserves the terminal, Agent protocol, and rendering invariants described above.
