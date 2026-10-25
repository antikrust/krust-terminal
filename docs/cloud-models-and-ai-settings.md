# Krust: AI settings, BYOK / Cloud models, and how to build

A guide for an AI agent working on Krust's AI settings, the "Bring your own
key" page, and the cloud-model providers — and on building and installing the
app afterwards.

## 0. House rules (read first)
- **Never create git branches.** Commit straight to `main` and push `main` to both remotes:
  - `origin` is shithub, at `shithub.space/n01/krust`.
  - `github` is `antikrust/krust-terminal`. Push with:
    `git -c credential.helper= -c credential.helper='!f() { echo username=antikrust; echo "password=$(gh auth token -u antikrust)"; }; f' push github main`
- **Don't run `cargo fmt` or `./script/format`.** The committed tree was formatted by a different rustfmt version, so either command rewrites about 1,000 files. Hand-format your code to match the code around it.
- Read `AGENTS.md` and `docs/local-harness-lifecycle.md` before touching AI paths. `docs/ai-settings-functions.md` also exists.

## 1. Where the settings live (GUI)
- **Settings page code:** `app/src/settings_view/ai_page.rs`.
  - `AISubpage` has the variants `KrustAgent`, `Profiles`, `Knowledge`, `ThirdPartyCLIAgents` (shown as "General"), `Grok` and `BringYourOwnKey`.
  - Each arm of the `match` in the page constructor pushes that page's widgets (look for `Some(AISubpage::BringYourOwnKey) =>`).
- **Navigation and registration:** `app/src/settings_view/mod.rs`.
  - The enum is `SettingsSection`.
  - `ai_subpages()` gives the nav order, `parent_page_section()` gives the backing page, and `FromStr` handles deep-link names.
  - Ollama (`ollama_page.rs`) and CustomRouters are their own backing pages.
- **Bring your own key page:** one section, **`CloudModelKeysWidget`** ("Custom inference"): one password field per provider (OpenAI, Anthropic, Google Gemini, Z.ai, Groq, OpenRouter, Venice), each with a clickable **Get a … API key** link.
  - Keys are stored in the keychain item `ApiProviderKeys`, read through `ApiProviderKeys` in `app/src/ai/api_providers.rs`, and sent only to their provider.
  - If the stored keys can't be read at startup (a denied keychain prompt), nothing is saved until a read succeeds, so they can't be overwritten.
  - Upstream's `ApiKeysWidget` (OpenAI/Anthropic/Google keys and custom endpoints, backed by `ApiKeyManager` in `crates/ai/src/api_keys.rs`) is no longer shown on this page: those keys only ever went to Warp's cloud backend. The widget still serves the Grok page. Warp's request code (`api_keys_for_request`) is the natural place to hand keys to Moloch once Moloch has providers — point it at `ApiProviderKeys` then.
- **Plain settings:** `app/src/settings/ai.rs`, inside `define_settings_group!(AISettings, …)`. Example entry:

  ```rust
  ollama_enabled: OllamaEnabled {
      type: bool, default: false, supported_platforms: SupportedPlatforms::ALL,
      sync_to_cloud: SyncToCloud::Never, surface: settings::SettingSurfaces::GUI,
      private: false, toml_path: "agents.krust_agent.other.ollama_enabled",
      description: "...",
  },
  ```

  - These values land in the user's TOML settings file. **Never store secrets here.**
  - Read: `*AISettings::as_ref(ctx).ollama_enabled.value()`.
  - Write: `AISettings::handle(ctx).update(ctx, |s, ctx| s.ollama_enabled.set_value(v, ctx))`.
  - A new toggleable setting also needs a Command Palette enable/disable entry (AGENTS.md rule).

## 2. How cloud models work, end to end
1. **Providers:** `PROVIDERS` in `app/src/ai/api_providers.rs`. Each `ApiProvider` has these fields:
   - `id`, `display_name`;
   - `base_url`: an OpenAI-compatible base, to which `/chat/completions` is appended;
   - `key_url`: a full `https://` URL, used for the link;
   - `pricing_note`: honest text shown on the settings page;
   - `models`: curated, in order of preference;
   - `live_free_models_url`: optional, for providers whose free roster rotates.

   Current providers: OpenAI, Anthropic (its OpenAI-compatible endpoint, which returns no thinking), Google Gemini, Z.ai, Groq, OpenRouter (live free list) and Venice.
2. **Model ids** look like `api/<provider>/<model>`, and the model part may contain `/`. Use `api_model_parts()` and `model_id_for()` to split and build them.
3. **Keys:** the singleton `ApiProviderKeys` loads them from the keychain at startup. `set_key()` emits `Changed` and writes back deferred.
   - It's registered in `app/src/lib.rs` **before** `HarnessAvailabilityModel`, which reads it.
   - Test apps register `ApiProviderKeys::new_empty()` instead (see `test_util/terminal.rs` and the other test setups).
4. **Picker list:** `app/src/ai/harness_availability.rs`.
   - API models are attached to the **Codex row**, as Ollama's are.
   - **`with_api_models()` must be called at every place that rebuilds that row:** `new`, `refresh_codex_catalog`, `refresh_ollama_models` (both branches) and `refresh_api_models`. Otherwise a codex catalog refresh silently drops them.
   - Selection is persisted under the key `"api"` (`persisted_api_model_for`).
   - `selected_local_harness` also understands `"api"`.
5. **Picker menu:** `app/src/terminal/profile_model_selector.rs`.
   - `ModelGroupKind { Harness, Ollama, Api }` splits the Codex row into sections: one section per provider, with no Default row and no Reasoning rows.
   - Selecting a model persists the provider key `"api"`.
6. **Spawn:** `run_local_claude_query` in `app/src/terminal/view.rs`.
   - An `api/` id maps to `LocalHarnessKind::Api`.
   - The key is read while `ctx` is still available. If there's no key, a toast says where to add one.
   - It builds the curl command, snapshots the exposed MCP tools (web search), and the pump calls `run_api_stream`.
7. **Runner:** `app/src/ai/agent_sdk/driver/harness/api_stream_exec.rs`. It mirrors the Ollama tool loop.
   - It offers the same built-in tools as the Ollama runner (`read_file`, `write_file`, `run_code`, and anything else `builtin_local_tool_specs()` returns) and emits the same `TurnEvent`s.
   - **The key is passed to curl only through its environment**, as `KRUST_API_KEY`, using `--variable %KRUST_API_KEY --expand-header "Authorization: Bearer {{KRUST_API_KEY}}"`. This needs curl 8.3 or newer. Never put the key in argv, never log it, never write it to disk.
   - The request body goes over stdin.
   - Streaming is SSE: `data:` lines, terminated by `[DONE]`. Text is held until the end of each round; reasoning (`reasoning_content`, or Groq's `reasoning`) streams as `Thinking`.
   - Tool-call fragments are joined by `index`.
   - Error bodies come in three shapes, all handled by `error_from_body`:
     - `{"error":{…}}` (OpenAI, Groq, Z.ai)
     - `[{"error":…}]`, pretty-printed over several lines (Gemini)
     - a bare `{"message":…}`
   - Sizes scale with the model's context window (`context_window` on each `ApiModel`; `DEFAULT_CONTEXT_WINDOW` when unlisted). `read_file` returns about a quarter of the window per call (`read_window_chars`: ~128 KB on a 128K model, up to 1 MB), so most files come back whole; local Ollama models keep their small window (`READ_WINDOW_BYTES`).
   - A tool result is clipped (head and tail kept) only past a whole read window (`max_tool_result_chars`).
   - Every request resends the whole conversation. Before each one, `compact_old_tool_results` checks whether the tool output carried in full is over about half the window (`tool_output_budget_chars`); only then does it swap the oldest long results for a one-line note, until the rest fits. The latest result always stays, and a large-window model keeps everything it read until it nears that budget.
   - There is no step limit (for Ollama either): a turn runs until the model answers or the user stops it.
   - `TurnUsage` token counts are the last request's and `context_window` is the model's real window (together they drive the context meter); `cost_usd` adds up every request in the turn at the model's `price_per_million` (see `model_price`).
8. **`LocalHarnessKind::Api`** is defined in `app/src/ai/agent_sdk/driver/harness/mod.rs` and serialised as `"api"` in `local_harness_threads.json`. Every exhaustive match on the kind needs an arm, including `view.rs` and `terminal/view/use_agent_footer/mod.rs`.
9. **Deleting a turn:** right-click an AI block (or open its "…" menu) and choose Delete, or ⌘+drag the block left. This is `TerminalAction::DeleteAIBlockTurn`, handled by `delete_ai_block_turn` in `terminal/view.rs`.
   - It deletes the whole turn: the user's prompt plus every exchange up to the next prompt (the follow-ups that carry tool results). A single step can't go, because it would leave a tool call without its result.
   - `AIConversation::remove_turn` removes the turn from the saved conversation, sharing its removal code with rewind (`remove_root_exchanges`). The blocks leave the screen through `remove_ai_blocks_for_exchanges`.
   - `LocalHarnessKind::forget_turn` makes the model forget it. Cloud models drop the turn from the saved history (memory and disk), and Ollama from its chat history. Both use `remove_turn_from_messages`, which finds the user message carrying the prompt (counting back from the newest when a prompt repeats) and removes everything up to the next user message.
   - Claude and Codex keep their sessions in their own CLIs, and so does Ollama's `/api/generate` token context, so there the turn is only hidden.
   - Delete isn't offered, and does nothing, while the conversation is running.

## 3. Recipes
**Add an OpenAI-compatible provider:**
1. **Confirm it's OpenAI-compatible:** a `POST {base}/chat/completions` endpoint, Bearer auth, `stream: true` returning SSE, and `tools` in the OpenAI function format.
2. **Get real model ids from the provider's live model list.** Many publish it publicly at `{base}/models`, which also says which models support tools. Pick models that can use tools.
3. **Append an `ApiProvider` to `PROVIDERS`:** use a full `https` `key_url`, an honest `pricing_note`, and `live_free_models_url: None` unless the roster rotates.
4. **Check the endpoint and key page are real:** `curl -s -o /dev/null -w '%{http_code}'` against both.
5. **Run the tests.** `api_providers_tests.rs` checks every provider automatically: id round-trips, an `https` key link, and a non-empty model list.

Nothing else needs changing: the settings field, the picker section and the runner all come from `PROVIDERS`.

**Special cases:**
- **Non-standard auth or path:** change `api_curl_args`, and keep the key in the environment.
- **A new error-body shape:** extend `error_from_body` and add a test to `api_stream_exec_tests.rs`.
- **A live list in a shape other than OpenRouter's:** generalise `parse_live_free_models`. It currently expects `{"data":[{id,name,context_length}]}` and filters ids ending `:free`.

**Add or change a settings UI element:**
1. Write a struct that implements `SettingsWidget` (`type View = AISettingsPageView`, plus `search_terms` and `render`).
2. Push it in the right `AISubpage` arm.
3. Wire its parts:
   - **Editors:** create them in the widget's `new(ctx)` with `ctx.add_typed_action_view(|ctx| EditorView::single_line(SingleLineEditorOptions{ is_password: …, ..}, ctx))`, and subscribe to `EditorEvent::Blurred | EditorEvent::Enter` to save.
   - **`MouseStateHandle`:** create it once and store it. Never call `MouseStateHandle::default()` inside `render`, or clicks stop registering.
   - **Links:** `appearance.ui_builder().link(label, Some(url), None, handle.clone()).soft_wrap(false).build().finish()`.
   - **Text:** `ui_builder().wrappable_text(text, true).with_style(UiComponentStyles{…})`, with `build_sub_header` and `render_separator` for section framing.
   - **Buttons:** reuse the shared button themes. Don't invent new ones (gui-ui-guidelines skill).
4. A secret-bearing field goes through the keychain (`ctx.secure_storage()`), never through `AISettings`.

**Golden rules for the harness (from the lifecycle doc):**
- Every run ends with `TurnCompleted`, error or not.
- Tool card `call_id`s must be unique within the conversation.
- Local conversations must never reach cloud request paths. Gate them with `local_harness_threads::lookup`.
- Provider differences stop at the parser.
- After touching message shapes, run `cargo test -p krust turn_events_to_conversation`.

## 4. Validate, commit, build, install
- **Compile:** `cargo check -p krust --bin krust --locked`
- **Targeted tests:** `cargo test -p krust --lib --locked -- api_providers api_stream_exec`. These run in the **lib** target; filtering on the bin finds no tests.
- **Clippy:** `cargo clippy -p krust --all-targets --tests`.
  - Using `-D warnings` currently fails on a pre-existing collapsible-`if` in `account_client` and some old `ollama_stream_exec` warnings. Make sure *you* add none.
- **Commit and push:** commit on `main`, staging only your own files, then push to both remotes (see section 0).
  - Every push starts `.forgejo/workflows/build.yml` on the PC runners. That's expected.
  - If a push touches LFS files, run `git lfs push github main` afterwards.
- **Build the Mac app.** The installed app is the **oss** channel (`krust.baby.Krust`), arm64, self-signed:
  ```
  ./script/macos/bundle --selfsign --nouniversal --channel oss
  ```
  - It builds the **working tree**, so another session's uncommitted code ends up in the app.
  - The output is `target/aarch64-apple-darwin/release-lto/bundle/osx/Krust.app`, plus a dmg.
  - It takes about 10–15 minutes when cached.
- **Install.** Check `pgrep -f '/Applications/Krust.app/Contents/MacOS/krust'` and don't quit the app if it's running. Then:
  ```
  rm -rf /Applications/Krust.app && ditto target/aarch64-apple-darwin/release-lto/bundle/osx/Krust.app /Applications/Krust.app
  ```
  - The Dock entry keeps pointing at the same path.
  - Verify:
    - `/usr/libexec/PlistBuddy -c "Print CFBundleIdentifier" …/Info.plist` should print `krust.baby.Krust`;
    - `lipo -archs` should print `arm64`;
    - `codesign -dv` should show `Signature=adhoc`;
    - `grep -a -c '<a string from your change>' …/MacOS/krust` should print a count above zero.
  - Tell the user to quit and reopen Krust. After each self-signed rebuild, macOS may ask once for keychain access; they should choose **Always Allow**.
