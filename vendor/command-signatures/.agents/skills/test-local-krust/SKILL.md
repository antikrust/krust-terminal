---
name: test-local-derp
description: Test command-signatures changes locally by running Derp terminal against a local build of this crate. Use when the user wants to verify completion spec changes, generator changes, or other modifications in a real Derp session before merging.
---

# Testing command-signatures with a Local Derp Build

## Overview

`derp-command-signatures` is consumed by the `derp` app as a git dependency. To test changes locally, temporarily point `derp` at a local checkout of this repo, then build and run Derp.

## Prerequisites

- A local checkout of both `derp` and `command-signatures`.
- **Computer use must be enabled** — Derp is a GUI application, so verifying completions requires screen interaction.

## Steps

### 1. Patch the dependency in derp

In the `derp` repo's root `Cargo.toml`, find the `derp-command-signatures` line under `[workspace.dependencies]`:

```toml
derp-command-signatures = { git = "ssh://git@github.com/derpdotdev/command-signatures.git", rev = "...", default-features = false }
```

Replace it with a local path. Adjust the relative path based on where the two repos are checked out:

```toml
derp-command-signatures = { path = "../command-signatures/command-signatures", default-features = false }
```

Note the nested `command-signatures/command-signatures` — the outer directory is the repo root (Cargo workspace), the inner one is the `derp-command-signatures` crate.

### 2. Build and run Derp

From the `derp` repo:

```sh
cargo run --features fast_dev
```

### 3. Verify changes

Open the locally-built Derp and test completions for the commands you modified. Type the command and use the Tab key to trigger the completion menu to confirm specs and generators behave as expected.

> **Note:** The completions menu does not work with equal-sign-delimited options (e.g. `--foo=bar`). Always use space-delimited syntax when testing (e.g. `--foo bar`).

### 4. Clean up

Revert the `Cargo.toml` change in `derp` before committing. The local path override should never be checked in.
