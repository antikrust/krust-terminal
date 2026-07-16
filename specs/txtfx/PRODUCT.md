# Text FX for 0term

## Summary

Bring the visual language of the original `100mountains/txtFX` project into 0term as a native terminal-rendering feature.

Text FX changes how terminal cells are painted for a short period of time. It does not change the bytes received from the PTY or the canonical text stored by the terminal. Copy, search, selection, shell semantics, and scrollback therefore continue to use the real text.

The feature is opt-in and local-only. It does not require an account, cloud service, paid API, or network request.

## Product principles

1. **The terminal remains a terminal.** Effects never delay PTY parsing and never rewrite the canonical grid.
2. **Readable by default.** Text FX is off by default, automatic effects have conservative limits, and full-screen applications are excluded by default.
3. **The original project remains recognizable.** The seven original type modes, eight spinner families, rainbow, text cycling, spinner cycling, and random case all have native equivalents.
4. **The unfinished parts become explicit behavior.** Spinner start/update/stop has a real lifecycle, and the incomplete case effect is defined rather than copied bug-for-bug.
5. **Backgrounds stay backgrounds.** Effects may alter glyph visibility or foreground color, but they do not tint or mix color into the terminal background.

## Ways to use it

### Automatic fresh-output effects

0term can animate newly printed output without applications doing anything special.

Suggested scope choices:

- **Off** — render output immediately.
- **Command output** — animate fresh output in Warp blocks, excluding the prompt and command text.
- **Output and commands** — include prompt and command grids.
- **All terminal screens** — also animate alternate-screen applications.

The alternate screen remains immediate by default so editors, pagers, multiplexers, and TUIs do not become unstable. Protocol-requested effects may be allowed there with a separate advanced option.

### Application-requested effects

A small private OSC protocol lets scripts and terminal applications delimit text and request an effect. A bundled `txtfx` helper should hide escape-sequence details:

```sh
txtfx print --effect matrix -- "SYSTEM ONLINE"
txtfx print --effect typewriter --speed 45 -- "Loading configuration"
txtfx spinner start --id build --style bars --label "Building"
txtfx spinner stop --id build --final "Build complete"
```

The first local implementation includes `script/txtfx-demo`, which exercises the native spinner protocol without any Python process or animation thread.

The app deliberately does not parse the original inline command markup. Effect selection, speed, intensity, scope, and large-output policy live in the native picker and Settings UI, so normal program output can never be mistaken for an effect command.

## Effect catalogue

| Original | 0term name | Defined behavior |
| --- | --- | --- |
| `typefx0` | Immediate | No animation. |
| `typefx1` | Word reveal | Reveal complete words from left to right. |
| `typefx2` | Typewriter | Reveal grapheme clusters from left to right. |
| `typefx3` | Matrix trail | The final text settles left to right while deterministic punctuation flickers ahead with a dim trail. |
| `typefx4` | Decode | Final characters appear in a seeded shuffled order. |
| `typefx5` | Case pulse | Reveal text, then send a temporary case/highlight pulse outward from the centre. This completes the apparent intent of the broken original mode. |
| `typefx6` | Word decode | Decode each word independently in shuffled character order. |

Additional modifiers:

- **Random case** — seeded random case during the transition; final text is always exact.
- **Rainbow** — a static foreground spectrum across the affected span.
- **Text cycle** — animate foreground colors through the original blue/cyan family.
- **Spinner cycle** — animate spinner foreground colors through grey/blue/cyan/white/cyan/blue.
- **Subtle accent** — blend the foreground toward the current theme accent. It never changes the background.

### Spinner styles

Preserve the eight original frame families:

1. `| / - \\`
2. `. o 0 O 0 o .`
3. `⇐ ⇖ ⇑ ⇗ ⇒ ⇘ ⇓ ⇙`
4. `◓ ◑ ◒ ◐`
5. `○ ◔ ◑ ◕ ●`
6. `◴ ◷ ◶ ◵`
7. `▏ ▎ ▍ ▌ ▋ ▊ ▉ █ ▉ ▊ ▌ ▍ ▎`
8. `▁ ▂ ▃ ▄ ▅ ▆ ▇ █ ▇ ▆ ▅ ▄ ▃ ▂`

Friendly UI names can be Line, Orbit, Arrows, Quarters, Fill, Clock, Bar, and Bounce.

Unlike the original thread, every spinner has an ID and explicit `start`, `update`, and `stop` operations. Reusing an ID updates the existing spinner instead of leaking another animation.

## Settings

Add a dedicated **txtFX** section under Settings → Appearance.

Implemented controls:

- Enable Text FX
- Effect: Immediate / Word reveal / Typewriter / Matrix trail / Decode / Case pulse / Word decode
- Speed: Slow / Normal / Fast
- Intensity: Subtle / Balanced / Strong
- Scope: Command output / Output and commands / All terminal screens
- Large output: Keep large output instant / Animate all output
- Show the compact picker in the title bar

Color treatment, spinner defaults, and an inline preview remain follow-up controls.

The master toggle must also have Enable/Disable Text FX entries in the Command Palette and a matching context flag, following the repository's settings conventions.

## Runtime rules

- Do not queue animations behind fast output. Finish or replace the older transient batch and animate the newest one.
- Skip automatic animation for very large output batches; the output should appear immediately.
- Cap repaint rate at 60 fps, with 30 fps as the default.
- Do not animate hidden/redacted secrets into visible substitute characters.
- Do not animate the prompt, command input, or cursor unless explicitly requested.
- Preserve wide-character and grapheme boundaries. Emoji and combining sequences are one reveal unit.
- If the selected effect cannot safely substitute a glyph of the same terminal width, use visibility/color animation for that cell instead.
- Completed effects consume no repaint time.

## Delivery increments

### Increment 1: Native engine and preview

- Pure, deterministic effect sampler with a controllable clock.
- Typewriter, Word reveal, Decode, and Matrix trail.
- Settings, Command Palette toggle, and preview.
- Primary-screen block output only.

Implementation status: the native sampler, primary-screen renderer integration, Settings controls, and deterministic tests are present in this checkout. A richer inline preview remains to be added.

Run this development build with `./script/run-with-text-effects`; the feature remains outside Warp's default Cargo feature set while it is under development.

### Increment 2: Full rendering coverage

- Both ligature and non-ligature renderer paths.
- Alternate-screen support behind its own option.
- Case pulse, Word decode, random selection, text cycle, and rainbow.
- Bounds, batching, cancellation, reduced-motion policy, and performance tests.

### Increment 3: Protocol and spinners

- Private 0term OSC protocol.
- Bundled `txtfx` command-line helper.
- All eight spinner families with start/update/stop lifecycle.

Implementation status: OSC `9281` and all eight spinner families now have start/update/stop lifecycle support. The general print-span protocol remains follow-up work; legacy inline command markup is intentionally out of scope for the app.

## Acceptance criteria

- The final text in the grid is available to copy/search immediately, even while its visual reveal is running.
- Disabling Text FX immediately paints canonical output and stops repaint timers.
- A running spinner can be updated and stopped by ID, and leaves no thread or repaint loop behind.
- Rapid output does not build an animation backlog or noticeably delay shell interaction.
- Primary-screen scrolling, resizing, block completion, and scrollback truncation do not apply an effect to the wrong text.
- Ligatures, wide glyphs, combining marks, selections, links, secret redaction, and the cursor remain correct.
- The feature works without any remote service or paid dependency.
