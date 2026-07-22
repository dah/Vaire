# AgentHarness

AgentHarness is a small macOS-first terminal chat client with one active Codex conversation. It uses the
installed Codex app-server and the Codex-owned ChatGPT subscription login flow; it does not offer
API-key login or approval UI. Codex's built-in command-line and file tools are enabled.

## Run

Requirements:

- macOS and stable Rust
- codex-cli 0.144.6 or newer available as codex on PATH

    cargo run

**Warning:** AgentHarness runs Codex tools with `danger-full-access` and no approval prompts. The
agent can run local programs, access the network, and create, modify, or delete any file your user
account can access. This is not a sandbox; use it only with prompts and content you trust.

Set AGENTHARNESS_CODEX_BIN=/absolute/path/to/codex only when codex is not on PATH.
On first launch, enter **/login** and complete the HTTPS browser sign-in. If the callback-based
OpenAI page fails, run **/logout** to cancel the pending attempt and use **/login device**; the app
opens the device verification page and displays the one-time code in the TUI. AgentHarness stores its
non-secret preferences and dedicated Codex runtime under
~/Library/Application Support/AgentHarness/; Codex owns credentials in that dedicated home.
Preferences may include the normalized ChatGPT email and a non-secret thread-to-account registry
used to prevent cross-account thread listing or resume; the preferences file and its directory are
owner-only.
Tool commands start in the dedicated `runtime/conversation` directory, and files created there are
kept across launches. The app-server receives the launching environment's `PATH`, while Codex's
default filtering of sensitive environment-variable names remains in effect.
Sanitized diagnostics are written to the diagnostics subdirectory.

## Commands and keys

- **/login**, **/login device**, **/logout**, **/new**, **/resume**, **/thinking**, **/help**, **/quit**
- **/model [id]** and **/reasoning [value]** use choices reported by app-server
- **/new** eagerly creates a fresh thread without deleting the previous one
- **/resume** opens the saved-thread picker; arrows or **j/k** navigate and **Enter** resumes
- In the thread picker, **d** requests deletion of the selected inactive thread and **D** requests
  deletion of all inactive threads. Both actions show their exact scope and require a second
  **Enter** confirmation; **Escape** cancels. The active saved thread is always protected.
- **/thinking** toggles a right-side panel containing only reasoning summaries or thinking text
  explicitly emitted by Codex app-server. It does not expose or infer hidden chain-of-thought.
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
