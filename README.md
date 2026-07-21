# AgentHarness

AgentHarness is a small macOS-first terminal chat client for one Codex conversation. It uses the
installed Codex app-server and the Codex-owned ChatGPT subscription login flow; it does not offer
API-key login or tool/approval UI.

## Run

Requirements:

- macOS and stable Rust
- codex-cli 0.144.6 or newer available as codex on PATH

    cargo run

Set AGENTHARNESS_CODEX_BIN=/absolute/path/to/codex only when codex is not on PATH.
On first launch, enter **/login** and complete the HTTPS browser sign-in. If the callback-based
OpenAI page fails, run **/logout** to cancel the pending attempt and use **/login device**; the app
opens the device verification page and displays the one-time code in the TUI. AgentHarness stores its
non-secret preferences and dedicated Codex runtime under
~/Library/Application Support/AgentHarness/; Codex owns credentials in that dedicated home.
Preferences may include the normalized ChatGPT email used to prevent cross-account thread resume;
the preferences file and its directory are owner-only.
Sanitized diagnostics are written to the diagnostics subdirectory.

## Commands and keys

- **/login**, **/login device**, **/logout**, **/resume**, **/help**, **/quit**
- **/model [id]** and **/reasoning [value]** use choices reported by app-server
- **Enter** sends; **Alt-Enter** inserts a newline
- **PageUp/PageDown**, arrow keys, **Home**, and **End** scroll the transcript
- **Escape** closes local help/errors, or interrupts an active turn
- **Ctrl-C** quits cleanly

Default tests are offline:

    cargo fmt --check
    cargo clippy --all-targets --all-features -- -D warnings
    cargo test --all-targets

The installed-CLI initialization smoke is explicit and does not require ChatGPT login:

    cargo test --test installed_cli_smoke -- --ignored --nocapture
