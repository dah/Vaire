# AgentHarness

AgentHarness is a small macOS-first terminal chat client with one active conversation across two
providers: Codex through the installed app-server and OpenRouter text chat. Codex uses its owned
ChatGPT subscription login flow; OpenRouter uses a user-supplied API key. There is no approval UI,
and Codex's built-in command-line and file tools are enabled.

The TUI can create and revisit provider-labelled histories, search models from both providers,
show Codex-emitted reasoning in a Reasoning side panel, animate the wait for reply text, and report
remaining model context when the active provider supplies usable token data.

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
On first launch, enter **/login** to choose Codex or OpenRouter. For Codex, complete the HTTPS browser sign-in. If the callback-based
OpenAI page fails, run **/logout** to cancel the pending attempt and use **/login device**; the app
opens the device verification page and displays the one-time code in the TUI. For OpenRouter, enter
an API key in the masked editor, validate it, then use **c** in the login popup to choose the enabled
model subset. AgentHarness stores its
non-secret preferences and dedicated Codex runtime under
~/Library/Application Support/AgentHarness/; Codex owns credentials in that dedicated home.
Preferences may include the normalized ChatGPT email and a non-secret thread-to-account registry
used to prevent cross-account thread listing or resume; the preferences file and its directory are
owner-only. New non-ephemeral threads are explicitly created with `threadSource: "appServer"`.
For compatibility with existing AgentHarness threads created before that source was explicit,
`/resume` discovers both `appServer` and legacy `vscode` sources, but exposes only thread IDs
already registered to the signed-in account whose cwd exactly matches the dedicated conversation
directory.
Tool commands start in the dedicated `runtime/conversation` directory, and files created there are
kept across launches. That directory and the dedicated Codex home are organizational boundaries,
not security boundaries: commands can leave the starting directory and can reach other same-user
paths, including Codex-owned authentication state.

The OpenRouter key is stored as plaintext in the owner-only
`runtime/openrouter-home/api-key` file (directory mode `0700`, file mode `0600`). This is
organizational isolation, not encryption or secure storage: same-user processes, backups,
snapshots, disk recovery, and full-access Codex commands may reach it, and logout deletion is not
secure erasure. Migration to macOS Keychain through the injected credential-store port remains
explicit technical debt; a future migration must save and verify the Keychain item before deleting
the file and preserve the file on any failure. Local OpenRouter histories are also owner-only
plaintext files. When an OpenRouter stream emits nonempty assistant text and then fails, schema V2
retains that partial only for display. Startup and **/resume** restore it under the explicit
**Agent (incomplete; turn failed):** label, but it is excluded from canonical history and every later
model request. In-progress and interrupted output is not checkpointed. OpenRouter SSE
events are bounded and validated through terminal completion. Provider error objects take
precedence over malformed completion metadata; a valid numeric status controls classification even
when symbolic metadata conflicts. Malformed optional usage is discarded without failing an
otherwise valid answer, and a provider-resolved semantic model is checked for internal stream
consistency without being compared to the requested alias. Parser failures add a closed static
stream stage to the visible turn-failure message without persisting response payload or stage data.

The app-server inherits the launching environment except that inherited `CODEX_*` values are
removed and AgentHarness supplies its dedicated `CODEX_HOME`; tool shells request environment
inheritance. Codex's default name-based filtering of variables containing `KEY`, `SECRET`, or
`TOKEN` is incomplete. Ambient authority such as `DATABASE_URL`, `SSH_AUTH_SOCK`, macOS Keychain,
credential/config files, and authenticated CLIs may remain available to model-run commands.
Sanitized diagnostics are written to the diagnostics subdirectory.

To stay responsive when app-server or local state is unexpectedly large, AgentHarness enforces
explicit resource ceilings. Drafts are limited to 128 KiB; the UI retains only a recent bounded
transcript slice (at most 1 MiB and 2,048 entries), while Codex remains the source of truth for
complete thread history. Protocol frames, saved preferences, and each rotating diagnostics file
are also capped at 1 MiB. A limit violation is reported instead of allocating without bound or
silently replacing the active thread.

## Commands and keys

- **/login**, **/login browser**, **/login device**, **/logout**, **/new**, **/resume**,
  **/thinking**, **/help**, **/quit**
- **/model** opens a searchable, provider-labelled picker. Switching providers immediately starts
  a blank conversation; **/resume** is the only operation that restores cross-provider history.
- **/reasoning [value]** uses Codex choices; OpenRouter reasoning effort is unsupported.
- **/new** eagerly creates a fresh conversation for the active provider without deleting history
- **/resume** opens the provider-labelled conversation picker; arrows or **j/k** navigate and **Enter** resumes
- In the conversation picker, **d** requests deletion of the selected inactive history and **D** requests
  deletion of all inactive histories. Both actions show their exact scope and require a second
  **Enter** confirmation; **Escape** cancels. The active saved thread is always protected.
- **/thinking** toggles the right-side Reasoning panel. AgentHarness requests detailed reasoning
  summaries for each turn and configures its dedicated runtime with
  `show_raw_agent_reasoning=true` at process and thread start/resume boundaries. This is
  best-effort configuration: for Codex the panel shows reasoning text only when the selected model
  explicitly emits it, with summaries as the fallback. OpenRouter reasoning fields are not collected.
  Hidden/private
  chain-of-thought is unavailable; AgentHarness neither exposes nor infers it. `/thinking`
  controls panel visibility; `/reasoning [value]` separately selects the reasoning effort level.
- The header uses the authenticated account identity instead of a generic signed-in label and shows
  right-aligned **Context N%** when usable usage data is available, or **Context --** when it is not.
- A small animated squiggle appears while the active provider is working before the first assistant text. It is
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
