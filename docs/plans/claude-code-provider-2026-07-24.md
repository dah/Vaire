# Claude Code provider milestone

Date: 2026-07-24  
Status: complete

## Decision

Vairë will add Claude Code as its third and final supported provider alongside Codex app-server and OpenRouter. The product continues to expose exactly one active provider, conversation, and turn.

This milestone uses the installed `claude` executable through its documented non-interactive CLI. It does not add the Claude Agent SDK, Node.js, Python, a direct Anthropic Messages API client, or a generic provider framework.

## Authentication boundary

Anthropic's published policy does not permit third-party products to offer Claude.ai login or route requests through Free, Pro, or Max subscription credentials. Vairë therefore:

- accepts only an Anthropic Console API key entered in Vairë's masked provider popup;
- never discovers, imports, reuses, or automates Claude.ai OAuth or subscription login;
- removes inherited `ANTHROPIC_*` and `CLAUDE_*` variables before constructing the child environment;
- injects the selected Vairë key only into the direct Claude child environment as `ANTHROPIC_API_KEY`;
- never places the key or fragments in application state, preferences, transcripts, diagnostics, process arguments, snapshots, or user-facing errors.

The key is stored through the injected credential-store port in owner-only plaintext `runtime/anthropic-home/api-key`: the directory is exact mode `0700` and the regular current-user-owned file is exact mode `0600`. This is organizational isolation, not encryption, secure storage, or protection from same-user/full-access commands.

Vairë reports the key as configured after a supported local `claude auth status --json` compatibility check. That check establishes credential-source selection, not remote validity. A rejected or expired key is reported from the first real turn without deleting the stored credential or conversation pointer.

## Supported CLI contract

The tested baseline is Claude Code 2.1.178 or newer. `VAIRE_CLAUDE_BIN` is the only Claude executable override.

Vairë uses only these supported surfaces:

- `claude --version`;
- `claude auth status --json`;
- `-p` / `--print`;
- `--output-format stream-json`;
- `--verbose`;
- `--include-partial-messages`;
- `--session-id <uuid>` for a new session;
- `--resume <uuid>` for an existing session;
- `--model <alias>`;
- `--safe-mode`;
- `--dangerously-skip-permissions`;
- documented flags that disable Chrome, prompt suggestions, slash commands, and disallowed built-in tools.

Every user turn runs in a fresh, directly spawned child process. Vairë never invokes a shell, scrapes human terminal output, drives the interactive TUI, uses `--continue`, or reads Claude's private JSONL/session layout.

The child runs from Vairë's persistent non-project `runtime/claude-conversation` directory with a dedicated owner-only `runtime/claude-home` supplied as `CLAUDE_CONFIG_DIR`. Safe mode prevents user/project `CLAUDE.md`, settings, hooks, plugins, skills, MCP, and other customizations from being inherited. Subagents, agent teams, interactive questions, Chrome, WebFetch, and WebSearch are explicitly disabled for this milestone.

## Full-access boundary

Claude Code runs with its documented dangerous permission bypass so its built-in command and file tools match Vairë's existing unrestricted same-user execution contract. This is not approval automation and not a sandbox.

Claude commands can access the network, persistent conversation directory, arbitrary same-user files, SSH agents, Keychain, authenticated CLIs, and other inherited non-Claude environment values. They may also be able to reach Vairë's plaintext provider credentials through same-user file access or process inspection. Dedicated homes, safe mode, environment cleanup, and a non-project cwd reduce accidental inheritance only.

## Models and reasoning

Until a supported stream initialization event reports authoritative metadata, the Claude model picker exposes only Anthropic's documented provider aliases:

- `default`
- `opus`
- `sonnet`
- `haiku`

These are selectors, not a fabricated account catalog. The `system/init` and assistant/result events establish the resolved model shown for a turn. A missing or unavailable alias fails visibly.

Changing into Claude from another provider is a hard blank-conversation boundary. Selecting a different Claude alias also starts a fresh blank Claude conversation because resumed Claude CLI sessions retain their original model. Use `/resume` to restore history.

Claude reasoning effort selection and reasoning-panel collection are out of scope for this milestone. `/reasoning` reports that the active provider does not expose a supported Vairë effort control, and the Reasoning panel never infers or displays hidden thinking.

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

## Stream and process safety

Stdout is a bounded newline-delimited JSON protocol. Vairë accepts correlated `system/init`, partial `stream_event` text deltas, complete assistant metadata, and exactly one terminal `result`. It rejects malformed required payloads, mismatched UUIDs, duplicate terminals, semantic events after terminal completion, oversized frames, resource exhaustion, and EOF without a terminal result.

Unknown metadata-only events may be ignored when correlation and terminal state remain unambiguous. Thinking deltas, tool inputs/results, raw provider errors, prompts, and replies are never written to diagnostics.

Interruption targets the one active process group, drains to a bounded terminal state when possible, escalates to termination after a grace period, always reaps the child, and marks the turn interrupted. Shutdown stops input, settles or terminates the Claude child, persists only settled Vairë state, then proceeds with the existing provider and terminal shutdown sequence.

Normal tests use fake executables, fake credentials, and temporary owner-only directories. They never require a key, network, Claude login, or access to a user's Claude configuration.

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
- only a Vairë-entered Console key can authenticate Claude;
- subscription OAuth is never discovered, reused, imported, or automated;
- executable discovery/version checks, environment construction, argv, parsing, cancellation, and reaping are covered offline;
- new, resume, send, interrupt, logout, auto-resume failure, and “forget from Vairë” flows are reducer/backend tested;
- stale or cross-provider events cannot mutate the active transcript or turn;
- the three-provider auth/model/history/header layouts render correctly at normal and narrow sizes;
- preferences migrate atomically to V3 and permit only the active provider's resume pointer;
- README and `AGENTS.md` describe the auth restriction, plaintext-key risk, opaque provider history, and unrestricted execution boundary;
- `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, and `cargo test --all-targets` pass.
