# AgentHarness

AgentHarness is a small macOS-first terminal chat client with one active Codex conversation. It uses the
installed Codex app-server and the Codex-owned ChatGPT subscription login flow; it does not offer
API-key login or approval UI. Codex's built-in command-line and file tools are enabled.

The TUI can create and revisit saved threads, show emitted reasoning in a side panel, identify the
signed-in ChatGPT account, animate the wait for the first reply text, and report the remaining model
context when Codex supplies usable token data.

## Run

Requirements:

- macOS and stable Rust
- codex-cli 0.144.6 or newer available as codex on PATH

    cargo run

**Warning:** AgentHarness runs Codex with `danger-full-access` and `approval_policy="never"`.
Commands execute without confirmation and can run local programs, use the network, invoke already
authenticated tools, and read, create, modify, or delete anything your macOS account can access.
This is not a sandbox; use it only with prompts and content you trust.

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
kept across launches. That directory and the dedicated Codex home are organizational boundaries,
not security boundaries: commands can leave the starting directory and can reach other same-user
paths, including Codex-owned authentication state.

The app-server inherits the launching environment except that inherited `CODEX_*` values are
removed and AgentHarness supplies its dedicated `CODEX_HOME`; tool shells request environment
inheritance. Codex's default name-based filtering of variables containing `KEY`, `SECRET`, or
`TOKEN` is incomplete. Ambient authority such as `DATABASE_URL`, `SSH_AUTH_SOCK`, macOS Keychain,
credential/config files, and authenticated CLIs may remain available to model-run commands.
Sanitized diagnostics are written to the diagnostics subdirectory.

## Commands and keys

- **/login**, **/login browser**, **/login device**, **/logout**, **/new**, **/resume**,
  **/thinking**, **/help**, **/quit**
- **/model [id]** and **/reasoning [value]** use choices reported by app-server
- **/new** eagerly creates a fresh thread without deleting the previous one
- **/resume** opens the saved-thread picker; arrows or **j/k** navigate and **Enter** resumes
- In the thread picker, **d** requests deletion of the selected inactive thread and **D** requests
  deletion of all inactive threads. Both actions show their exact scope and require a second
  **Enter** confirmation; **Escape** cancels. The active saved thread is always protected.
- **/thinking** toggles a right-side panel containing only reasoning summaries or thinking text
  explicitly emitted by Codex app-server. It does not expose or infer hidden chain-of-thought.
- The header uses the authenticated account identity instead of a generic signed-in label and shows
  right-aligned **Context N%** when usable usage data is available, or **Context --** when it is not.
- A small animated squiggle appears while Codex is working before the first assistant text. It is
  display-only and disappears on the first nonempty text or any terminal turn state.
- **Enter** sends; **Alt-Enter** inserts a newline
- **PageUp/PageDown**, arrow keys, **Home**, and **End** scroll the transcript
- **Escape** closes local help/errors, or interrupts an active turn
- **Ctrl-C** quits cleanly

Default tests are offline:

    cargo fmt --check
    cargo clippy --all-targets --all-features -- -D warnings
    cargo test --all-targets

The installed-CLI initialization smoke is explicit and does not require ChatGPT login:

    cargo test --test installed_cli_smoke installed_cli_initializes_with_full_access_policy -- --ignored --nocapture
