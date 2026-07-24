# Vairë

Vairë is a small macOS-first terminal chat client with one active conversation across three
providers: Codex through the installed app-server, OpenRouter text chat, and Claude Code through
the installed non-interactive CLI. Codex uses its owned ChatGPT subscription login flow.
OpenRouter and Claude use user-supplied API keys. There is no approval UI; Codex and Claude
command/file tools run with unrestricted same-user access.

The TUI can create and revisit provider-labelled histories, search models from all providers,
show Codex-emitted reasoning in a Reasoning side panel, animate the wait for reply text, and report
remaining model context when the active provider supplies usable token data.

## Run

Requirements:

- macOS and stable Rust
- codex-cli 0.144.6 or newer available as `codex` on `PATH`
- Claude Code 2.1.178 or newer available as `claude` on `PATH` to use the Claude provider

```bash
cargo run --bin vaire
```

**Warning:** Vairë runs Codex with `danger-full-access` and
`approval_policy="never"`, and Claude with its documented dangerous permission bypass.
Supported commands execute without confirmation and can run local programs, use the network,
invoke authenticated tools, and read, create, modify, or delete anything your macOS account can
access. This is not a sandbox; use it only with prompts and content you trust.

Set `VAIRE_CODEX_BIN=/absolute/path/to/codex` or
`VAIRE_CLAUDE_BIN=/absolute/path/to/claude` only when that executable is not on `PATH`.

On first launch, enter **/login** and choose a provider:

- Codex opens the supported HTTPS ChatGPT sign-in. If the callback flow fails, run **/logout** to
  cancel it and use **/login device**.
- OpenRouter opens a masked API-key editor, validates the key, and lets you use **c** in the login
  popup to choose the enabled model subset.
- Claude opens a masked Anthropic Console API-key editor. Vairë does not discover, import, reuse,
  or automate Claude.ai Free/Pro/Max subscription OAuth: Anthropic's policy requires third-party
  products to use Console API keys or a supported cloud provider. The key is treated as configured
  after a local CLI credential-source check and is remotely validated by the first real turn.

Vairë stores non-secret preferences and dedicated provider runtime state under
`~/Library/Application Support/vaire/`. Preferences may include the normalized ChatGPT email,
provider model selections, registered conversation IDs, and a non-secret Codex
thread-to-account registry. The preferences file and its directory are owner-only.

On the first launch after upgrading from the legacy product name, Vairë migrates the complete
`~/Library/Application Support/AgentHarness` root to
`~/Library/Application Support/vaire` before opening state, diagnostics, credentials, or provider
processes. It performs one exclusive same-parent rename only when the legacy entry is a real,
current-user-owned `0700` directory and the new entry is absent. It does not follow root symlinks
or inspect, copy, deserialize, log, or rewrite descendants. A symlink, file, wrong owner, unsafe
mode, or any state in which both roots exist fails closed without choosing, deleting, or merging
data. Stop all Vairë versions and move one complete root aside for inspection before retrying; do
not merge or overwrite the roots. A failure to sync the parent after the verified rename means the
move is committed but its crash durability is unverified, so Vairë reports the failure and never
attempts to reverse it automatically.

For compatibility with existing Codex threads created before the source was explicit,
`/resume` discovers both `appServer` and legacy `vscode` sources, but exposes only thread IDs
already registered to the signed-in account whose cwd exactly matches the dedicated conversation
directory. It permanently searches both the current cwd and the historical pre-rename cwd so
registered conversations remain visible after the product rename; those searches share resource
ceilings and never auto-register a discovered thread.

Codex tools start in persistent non-project `runtime/conversation`; Claude tools start in the separate persistent non-project `runtime/claude-conversation` directory.
Files created there survive restarts. This directory and the dedicated provider homes are
organizational boundaries only: full-access commands can leave them and reach arbitrary same-user
paths, including provider authentication state and Vairë's plaintext API-key files.

The OpenRouter key is stored in owner-only plaintext
`runtime/openrouter-home/api-key`; the Anthropic Console key is stored in owner-only plaintext
`runtime/anthropic-home/api-key`. Each directory is exact mode `0700` and each regular,
current-user-owned key file is exact mode `0600`. This is not encryption, secure storage, a
sandbox, or protection from same-user processes, backups, snapshots, disk recovery, process
inspection, or full-access model-run commands. Logout deletion is not secure erasure. OpenRouter's
migration to macOS Keychain through the injected credential-store port remains explicit technical
debt: a future migration must save and verify the Keychain item before deleting the file and
preserve the file on any failure.

OpenRouter histories and Vairë's Claude registrations/display histories are owner-only plaintext.
If either stream emits nonempty assistant text and then fails, Vairë retains that partial only for
display and restores it under **Agent (incomplete; turn failed):**. Failed partials are never model
context; in-progress and interrupted output is not checkpointed.

Claude remains the source of its model context. Vairë uses only documented print-mode stream JSON,
explicit UUID creation/resume, safe mode, and direct process spawning. It never parses or
enumerates Claude's private transcript/session files and never automates the interactive Claude
TUI. `/resume` shows only Vairë-registered Claude sessions. Deleting an inactive Claude row
forgets Vairë's registration and bounded display history; it does not claim to erase opaque
Claude-owned session data.

Claude runs with a dedicated `runtime/claude-home` configuration directory. Safe mode and direct
flags disable inherited `CLAUDE.md`, user/project settings, hooks, plugins, skills, MCP, Chrome,
WebFetch/WebSearch, interactive questions, subagents, and agent teams for this milestone. Vairë
removes inherited `ANTHROPIC_*` and `CLAUDE_*` variables before injecting only the selected
Console key into the Claude child environment. Environment cleanup is not a security boundary:
other ambient authority such as `DATABASE_URL`, `SSH_AUTH_SOCK`, Keychain, credential/config
files, and authenticated CLIs may remain available to model-run commands.

The initial Claude model choices are Anthropic's documented selectors `default`, `opus`,
`sonnet`, and `haiku`; they are aliases, not a fabricated account catalog. Stream initialization
and terminal metadata establish the provider-reported model. Selecting a different Claude alias
starts a fresh blank Claude conversation because resumed CLI sessions retain their original model.

OpenRouter SSE and Claude stream-json events are bounded and terminally validated. Malformed
required payloads, correlation mismatches, duplicate terminals, resource exhaustion, or EOF
without a terminal result fail visibly. Optional usage/metadata never clears earlier valid data,
and raw provider payloads are not persisted in diagnostics.

The Codex app-server inherits the launcher environment except inherited `CODEX_*` values are
removed and Vairë supplies its dedicated `CODEX_HOME`; tool shells request environment
inheritance. Codex's default name-based filtering of variables containing `KEY`, `SECRET`, or
`TOKEN` is incomplete. Sanitized diagnostics are written to `diagnostics/vaire.log`; a migrated
legacy diagnostics file remains untouched.

Vairë enforces explicit resource ceilings. Drafts are limited to 128 KiB; the UI retains at most
1 MiB and 2,048 recent transcript entries. Protocol frames, saved preferences, each local
conversation record, and each rotating diagnostics file are bounded. A limit violation is
reported instead of allocating without bound or silently replacing the active conversation.

## Commands and keys

- **/login**, **/login browser**, **/login device**, **/logout**, **/new**, **/resume**,
  **/thinking**, **/help**, **/quit**
- **/model** opens a searchable, provider-labelled picker. Switching providers immediately starts
  a blank conversation; selecting a different Claude alias does likewise. **/resume** is the only
  operation that deliberately restores cross-provider history.
- **/reasoning [value]** uses Codex choices. OpenRouter and Claude reasoning effort are unsupported
  in this milestone.
- **/new** eagerly creates a fresh conversation for the active provider without deleting history.
- **/resume** opens the provider-labelled conversation picker; arrows or **j/k** navigate and
  **Enter** resumes.
- In the conversation picker, **d** requests deletion of the selected inactive history and **D**
  requests deletion of all inactive histories. Both require a second **Enter**; **Escape** cancels.
  The active conversation is protected. Claude deletion is registration/display-history removal
  only.
- **/thinking** toggles the right-side Reasoning panel. For Codex it shows only reasoning summaries
  or reasoning text explicitly emitted by app-server. OpenRouter and Claude reasoning fields are
  not collected. Hidden/private chain-of-thought is unavailable; Vairë neither exposes nor infers
  it.
- The header shows provider-specific authentication/conversation/model state and right-aligned
  **Context N%** when usable data exists, or **Context --** otherwise.
- A display-only squiggle appears before the first assistant text and disappears on text or any
  terminal turn state.
- **Enter** sends; **Alt-Enter** inserts a newline.
- **PageUp/PageDown**, arrow keys, **Home**, and **End** scroll the transcript.
- **Escape** closes local help/errors or interrupts an active turn.
- **Ctrl-C** quits cleanly.

Default tests are offline:

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets
```

Installed-CLI smoke tests are explicit, ignored, and do not require provider login or network
access.
