# Claude Code provider milestone

Date: 2026-07-24  
Status: complete

> **Historical superseding note (2026-07-24):** This plan accurately records the original Claude
> provider milestone, whose minimum was 2.1.178 and which deliberately left effort selection out of
> scope. The completed `claude-reasoning-effort-2026-07-24.md` milestone supersedes only those two
> points: the current minimum is 2.1.218 and Claude `/reasoning` now selects a provider-wide
> requested effort for the next turn. The original session, auth, safety, and reasoning-panel
> boundaries below remain historical and durable.

## Decision

Vairë will add Claude Code as its third and final supported provider alongside Codex app-server and OpenRouter. The product continues to expose exactly one active provider, conversation, and turn.

This milestone uses the installed `claude` executable through its documented non-interactive CLI. It does not add the Claude Agent SDK, Node.js, Python, a direct Anthropic Messages API client, or a generic provider framework.

## Authentication boundary

This corrected contract supersedes the milestone's initial Console API-key design. Claude Code owns
the complete Claude.ai subscription-auth lifecycle, matching the installed CLI's supported native
flow. Vairë:

- starts `claude auth login --claudeai` for login, `claude auth logout` for logout, and uses
  `claude auth status --json` only to classify the resulting local state;
- temporarily restores the normal terminal while the foreground login/logout command runs, then
  safely re-enters and redraws the TUI after the child exits;
- never asks for, receives, reads, copies, prints, persists, migrates, or deletes an OAuth access
  token, refresh token, session cookie, or Claude Code Keychain item;
- never injects `ANTHROPIC_API_KEY`, `CLAUDE_CODE_OAUTH_TOKEN`, or another credential into a Claude
  child;
- removes inherited `ANTHROPIC_*` and `CLAUDE_*` variables before constructing auth and turn
  environments, so ambient API keys or token overrides cannot silently outrank native login;
- forces the native Claude.ai login method for the auth subprocess and rejects unsupported or
  ambiguous authenticated states;
- revalidates the native subscription state immediately before every turn and blocks the turn when
  the result is signed out, unsupported, or unverifiable.

On macOS, Claude Code owns its Keychain-backed login and token refresh. Claude logout is therefore
global to the installed CLI, and the UI warns before invoking it. Any obsolete
`runtime/anthropic-home/api-key` left by an experimental build is never referenced, inspected,
imported, overwritten, or deleted by Vairë credential code. Removal is a deliberate manual cleanup
decision. Unrestricted same-user model commands can still reach the file.

## Supported CLI contract

The tested baseline is Claude Code 2.1.178 or newer. `VAIRE_CLAUDE_BIN` is the only Claude executable override.

Vairë uses only these supported surfaces:

- `claude --version`;
- `claude auth status --json`;
- `claude auth login --claudeai`;
- `claude auth logout`;
- `-p` / `--print`;
- `--output-format stream-json`;
- `--verbose`;
- `--include-partial-messages`;
- `--session-id <uuid>` for a new session;
- `--resume <uuid>` for an existing session;
- `--model <alias>`;
- `--safe-mode`;
- `--strict-mcp-config` with `--mcp-config '{"mcpServers":{}}'` for an accepted zero-server MCP configuration;
- `--dangerously-skip-permissions`;
- documented flags that disable Chrome, prompt suggestions, slash commands, and disallowed built-in tools.

Every user turn runs in a fresh, directly spawned child process. Vairë never invokes a shell, scrapes human terminal output, drives the interactive TUI, uses `--continue`, or reads Claude's private JSONL/session layout.

The child runs from Vairë's persistent non-project `runtime/claude-conversation` directory with a dedicated owner-only `runtime/claude-home` supplied as `CLAUDE_CONFIG_DIR`. Safe mode and direct flags prevent user/project `CLAUDE.md`, settings, hooks, plugins, skills, and other customizations from being inherited. Strict MCP mode uses the CLI-accepted empty object `{"mcpServers":{}}`, so no MCP servers are configured. Subagents, agent teams, interactive questions, Chrome, WebFetch, and WebSearch are explicitly disabled for this milestone.

## Full-access boundary

Claude Code runs with its documented dangerous permission bypass so its built-in command and file tools match Vairë's existing unrestricted same-user execution contract. This is not approval automation and not a sandbox.

Claude commands can access the network, persistent conversation directory, arbitrary same-user files, SSH agents, Keychain, authenticated CLIs, and other inherited non-Claude environment values. They may also be able to use Claude Code's Keychain-backed login or reach Vairë's plaintext OpenRouter credential through same-user file access or process inspection. Dedicated homes, safe mode, environment cleanup, and a non-project cwd reduce accidental inheritance only.

## Models and reasoning

Until a supported stream initialization event reports authoritative metadata, the Claude model picker exposes only Anthropic's documented provider aliases:

- `default`
- `fable`
- `opus`
- `sonnet`
- `haiku`

These are selectors, not a fabricated account catalog. The `system/init` and assistant/result events establish the resolved model shown for a turn. A missing or unavailable alias fails visibly.

Changing into Claude from another provider is a hard blank-conversation boundary. Selecting a different Claude alias also starts a fresh blank Claude conversation because resumed Claude CLI sessions retain their original model. Use `/resume` to restore history.

Claude reasoning effort selection and reasoning-panel collection are out of scope for this milestone. Both `/reasoning` and `/reasoning <value>` report that the active provider does not expose a supported Vairë effort control and start no provider work; the Reasoning panel never infers or displays hidden thinking.

## Conversation ownership

Vairë generates and validates canonical UUID session IDs, passes them to Claude only through `--session-id` or `--resume`, and saves a pointer only after the corresponding Vairë registration is durably created or restored.

Claude remains the source of model context. Vairë separately stores a bounded display-only record containing:

- the registered session UUID;
- selected alias and last provider-reported model;
- title and timestamps;
- user text;
- completed assistant text;
- nonempty failed-stream partial text marked explicitly incomplete.

Vairë never feeds this display record back into Claude. Failed partial output is display-only; interrupted output is not checkpointed.

`/resume` lists only sessions registered by Vairë. It does not enumerate or import Claude's private sessions. Deleting a Claude row means “forget from Vairë”: it removes the inactive Vairë registration and bounded display history after confirmation, but does not claim to delete Claude-owned private session data. The active session remains protected.

Resume failures preserve the saved UUID and display history, enter a visible blocking state, and never silently create a replacement. `/new` is the explicit recovery path.

The supported `auth status --json` contract classifies native subscription state but exposes no
stable account identifier. Vairë therefore never silently auto-restores a saved Claude session at
startup or after authentication. It preserves the local pointer and requires `/resume` to
deliberately restore it or `/new` to start blank. Vairë-owned Claude registrations are same-user
local records rather than provider-account-scoped records; a direct switch between two supported
Claude.ai subscriptions cannot be distinguished through this status surface.

## Stream and process safety

Stdout is a bounded newline-delimited JSON protocol. Vairë accepts correlated `system/init`, partial `stream_event` text deltas, complete assistant metadata, and exactly one terminal `result`. It rejects malformed required payloads, mismatched UUIDs, duplicate terminals, semantic events after terminal completion, oversized frames, resource exhaustion, and EOF without a terminal result.

Unknown metadata-only events may be ignored when correlation and terminal state remain unambiguous. Thinking deltas, tool inputs/results, raw provider errors, prompts, and replies are never written to diagnostics.

Interruption targets the one active process group, drains to a bounded terminal state when possible, escalates to termination after a grace period, always reaps the child, and marks the turn interrupted. Shutdown stops input, settles or terminates the Claude child, persists only settled Vairë state, then proceeds with the existing provider and terminal shutdown sequence.

The native auth subprocess inherits the foreground terminal streams and Vairë's foreground process
group so normal terminal `Ctrl-C` and `Ctrl-Z` job control still applies to the complete application
job. The main loop stops terminal-event polling while Claude Code uses the terminal, handles
cancellation and shutdown without racing a consumed input event, terminates the bounded macOS child
tree, reaps the direct child, and restores the terminal on all settled paths. Version and status
probes have null stdin and isolated process groups.

Normal tests use fake executables, fake auth-status output, and temporary owner-only directories.
They never require network, a real Claude login, Keychain access, a token, or access to a user's
Claude configuration.

## Resource contracts

The initial Claude slice retains these hard bounds:

- 1 MiB per stream-json line;
- 16 MiB aggregate stdout per turn;
- 16,384 stream events per turn;
- 64 queued provider events;
- 128 KiB user input;
- 256 KiB completed or incomplete assistant text;
- 1 MiB per Vairë session record;
- 50 registered sessions;
- 1,024 turns per session;
- 16 MiB aggregate registered-session storage;
- 64 KiB bounded version/auth probe output;
- canonical UUID session IDs and bounded provider text fields.

Any future increase is a deliberate compatibility and documentation change.

## Acceptance criteria

The milestone is complete when:

- Codex and OpenRouter behavior remains green and independently usable if Claude is absent or broken;
- Claude login, status, refresh, and logout use only the installed CLI's native Claude.ai subscription-auth commands;
- Vairë never handles a Claude secret or accepts a higher-precedence ambient API-key/token override;
- every Claude turn revalidates the supported native subscription status and fails closed when it is signed out, unsupported, or unverifiable;
- startup and post-auth flows never silently resume an unscoped saved Claude session, instead preserving it for explicit `/resume` or `/new`;
- terminal suspension, foreground process-group inheritance, cancellation, descendant cleanup, child reaping, and TUI restoration are covered offline;
- executable discovery/version checks, environment construction, argv, parsing, cancellation, and reaping are covered offline;
- new, resume, send, interrupt, logout, auto-resume failure, and “forget from Vairë” flows are reducer/backend tested;
- stale or cross-provider events cannot mutate the active transcript or turn;
- the three-provider auth/model/history/header layouts render correctly at normal and narrow sizes;
- preferences migrate atomically to V3 and permit only the active provider's resume pointer;
- README and `AGENTS.md` describe CLI-owned subscription auth, system-wide logout, inert legacy-key handling, opaque provider history, and the unrestricted execution boundary;
- `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, and `cargo test --all-targets` pass.
