# AgentHarness MVP: Implementation Plan

## Implementation status

- [x] Milestone 0: protocol foundation and pragmatic safety defaults.
- [x] Milestones 1–4: application core, local services, transport/session integration, and single-thread workflow.
- [x] Milestones 5–6: Ratatui shell, lifecycle hardening, and release validation.
- [x] Independent final review and full verification.

## Goal

Build a small macOS-first Rust/Ratatui terminal chat client that uses one long-lived installed `codex app-server` process to:

- sign in through the supported ChatGPT browser flow;
- discover models and reasoning levels dynamically;
- maintain exactly one working thread and resume it on restart;
- accept conversational prompts and stream one assistant reply at a time; and
- remain recoverable when authentication, resume, protocol, or process operations fail.

The MVP excludes tools, approvals, MCP, file/shell/web access, plugins, skills, images, and sub-agents. Linux-friendly seams are allowed; Windows work is not.

## Background and protocol baseline

`AGENTS.md:3-193` is authoritative. The repository contained no Rust package or implementation when this plan was written.

Discovery on 2026-07-21 established this baseline:

- The installed runtime was `codex-cli 0.144.6`. Its stable generated schema confirmed the MVP methods `initialize`, `account/read`, `account/login/start`, `account/logout`, paginated `model/list`, `thread/start`, `thread/resume`, `thread/read`, `turn/start`, and `turn/interrupt`.
- The response stream is scoped by thread, turn, and item. The relevant events are `thread/started`, `turn/started`, `item/started`, `item/agentMessage/delta`, `item/completed`, `turn/completed`, and `error`.
- The server can initiate approval, permission, tool, elicitation, token-refresh, and attestation requests. Every such request is outside the MVP.
- The schema exposes `approvalPolicy: "never"`, read-only sandboxing, a non-project `cwd`, configuration overrides, and typed negative/error responses for server requests.
- Official configuration documents provide individual switches for shell, web search, apps, MCP servers, skills, plugins, hooks, and multi-agent behavior. AgentHarness uses these pragmatically, but a complete app-server “chat only” mode is not required.

The installed schema wins over this plan whenever names or shapes differ. Regenerate it at implementation time and record the tested CLI version.

Reference-only patterns may be consulted at:

- `repoprompt-ce/Sources/RepoPrompt/Infrastructure/AI/Providers/Codex/AppServer/CodexAppServerClient.swift:940-1049` for direct process launch and FIFO stdout/stderr draining;
- `repoprompt-ce/Sources/RepoPrompt/Infrastructure/AI/Providers/Codex/AppServer/CodexNativeSessionController.swift:2482-2621` for item-scoped delta reconciliation; and
- `repoprompt-ce/Sources/RepoPrompt/Infrastructure/AI/Providers/Codex/AppServer/CodexNativeSessionController.swift:3908-3947` for classifying server requests.

These files are non-authoritative and strictly read-only.

## Milestone 0 safety foundation

Milestone 0 establishes pragmatic defense in depth for a conversation-focused client. It does not require proof that every Codex built-in tool has been removed, and retained built-in capability does not block release.

### Isolation policy

Run app-server with:

1. A dedicated `CODEX_HOME` under AgentHarness application support, with owner-only permissions. Codex alone creates and reads its auth files.
2. A dedicated empty conversation directory as `cwd`; never use the launch directory or a user project.
3. No project-local configuration, instructions, MCP definitions, plugins, skills, hooks, or workspace roots.
4. Explicit startup overrides that disable tool-bearing optional features recognized by the tested CLI where practical, including shell/unified execution, web search, apps/connectors, skill dependency installation, plugins, hooks, computer/browser use, image generation, and multi-agent operation.
5. `approvalPolicy: "never"` and a read-only sandbox at thread and turn boundaries, with `networkAccess: false` in the turn sandbox policy. App-server’s own authenticated Codex transport remains available.
6. Initialize capabilities that leave experimental APIs, MCP form elicitation, and attestation disabled.
7. A client handler that explicitly denies or errors every server request and treats its arrival as a visible safety violation.

For `codex-cli 0.144.6`, the generated request inventory is `item/commandExecution/requestApproval`, `item/fileChange/requestApproval`, `item/tool/requestUserInput`, `mcpServer/elicitation/request`, `item/permissions/requestApproval`, `item/tool/call`, `account/chatgptAuthTokens/refresh`, `attestation/generate`, `applyPatchApproval`, and `execCommandApproval`. Regenerate the inventory for every supported CLI version.

A dedicated `CODEX_HOME` and empty `mcp_servers` configuration reduce inherited extension risk. Registry inspection may be used for diagnostics, but exhaustive extension suppression and authenticated adversarial conformance are not release requirements.

Inject every current server-request variant through the fake server, including legacy approval names, and prove the client never approves it. A safety violation fails pending work, surfaces an actionable error, and makes the connection unusable until restarted.

## Proposed package layout

Use one binary crate and keep tests alongside the owning module where practical.

```text
Cargo.toml
src/
  main.rs                 # dependency wiring and top-level shutdown
  app.rs                  # AppState, Intent, AppEvent, Effect, pure reducer
  command.rs              # pure slash-command parser and help text
  ports.rs                # narrow test seams: backend, store, browser, clock/spawner
  diagnostics.rs          # allowlisted, sanitized file diagnostics
  persistence.rs          # PreferencesV1, paths, migration, atomic writes
  platform/
    mod.rs                # platform traits and cfg selection
    macos.rs              # Application Support path, direct `open`, signals
    unix.rs               # shared Unix behavior; future Linux implementation seam
  codex/
    mod.rs
    protocol.rs           # narrow Serde request/response/event types
    transport.rs          # child process, JSONL, IDs, pending requests, timeouts
    session.rs            # auth/catalog/thread/turn orchestration and event reduction
    safety.rs             # tested startup policy and fail-closed request handling
  tui/
    mod.rs                # render loop boundary
    terminal.rs           # RAII raw-mode/alternate-screen guard
    input.rs              # key events -> typed Intent
    view.rs               # stateless rendering from AppState
tests/
  app_server_integration.rs
  support/
    fake_app_server.rs
  fixtures/
    protocol/             # curated JSONL fixtures, not a vendored full schema
```

Initial runtime dependencies should stay narrow: Tokio, Ratatui, Crossterm, Serde/serde_json, thiserror, a platform-directory crate, and a URL parser. Add a logging facade only with an allowlisted sink. Test-only crates may provide temporary directories, process assertions, and snapshot helpers. Run `tests/support/fake_app_server.rs` as a helper mode of the integration-test executable (or an equivalently test-only binary) so no fake server ships with the application. Do not add provider abstractions, databases, plugin frameworks, or a dependency on RepoPrompt.

## Architecture and event flow

### Authoritative state

`AppState` is the only UI model. Use explicit enums rather than loosely related booleans:

- `StartupState`: loading preferences, starting server, initializing, ready, failed.
- `AuthState`: unknown, signed out, login pending, signed in with account scope.
- `CatalogState`: loading, ready with models, failed.
- `ThreadState`: none, resuming, active, resume failed while preserving the saved ID.
- `TurnState`: idle, starting, streaming, completed, interrupting, failed.
- selected model/reasoning, transcript entries, composer contents, notices, and shutdown state.

Transcript entries contain sanitized display text and stable thread/turn/item identifiers. They never contain raw protocol envelopes.

### One-way flow

```text
terminal key
  -> tui::input -> Intent
  -> app::reduce(AppState, AppEvent) -> [Effect]
  -> backend / persistence / browser / shutdown worker
  -> typed DomainEvent
  -> app::reduce
  -> immutable AppState snapshot
  -> tui::view
```

The reducer is synchronous and side-effect free. Effects carry request tokens so stale responses can be ignored. Ratatui never calls Codex, opens a browser, or performs disk I/O.

A coordinator task owns `AppState` and selects over input intents, backend events, signals, and worker results. The backend task owns the app-server child. The terminal-rendering task alone owns stdout. Bounded channels prevent unbounded event growth; assistant deltas may be coalesced between render ticks without changing transcript content.

### Injection seams

Keep traits small:

- `CodexBackend`: typed commands in, domain events out.
- `PreferencesStore`: load and atomic save.
- `BrowserOpener`: open a validated URL without a shell.
- `ProcessSpawner` and `Clock`: deterministic transport timeout and cleanup tests.

Prefer concrete types inside each subsystem; traits exist only at asynchronous or operating-system boundaries.

## Protocol and application lifecycle

### Transport contract

`codex::transport` must:

- resolve `codex` and spawn `codex app-server` directly, never through a shell;
- own one child, stdin, stdout, stderr, and a connection generation;
- drain stdout and stderr continuously from process start;
- frame stdout as bounded UTF-8 JSONL and treat malformed or oversized frames as recoverable connection errors;
- allocate monotonic request IDs and correlate result/error responses through pending one-shot senders;
- apply method-specific request timeouts, fail all pending work on EOF/exit, and ignore stale events from older generations;
- separate responses, notifications, and server requests before decoding method-specific payloads;
- tolerate unknown notifications by recording only their method name; and
- perform idempotent shutdown: stop writes, close stdin, wait briefly, terminate if required, and reap the child.

Diagnostics may include timestamps, method names, IDs, CLI version, byte counts, and redacted error categories. Never log raw frames, stderr payloads, account email, URLs with query strings, prompts, replies, or auth payloads.

### Startup sequence

1. Create application-support, dedicated Codex home, empty conversation, and diagnostic directories with owner-only permissions.
2. Load `PreferencesV1`; missing, unsupported, or corrupt state becomes a clean first run plus a local notice.
3. Resolve the executable and check the supported CLI version or required features.
4. Spawn app-server with the isolated conversation-focused safety policy.
5. Send `initialize` once with non-experimental capabilities, then send `initialized`.
6. Call `account/read`. Accept only a managed `chatgpt` account for the MVP.
7. Fetch every `model/list` page with `includeHidden: false`; deduplicate by model ID.
8. Validate the saved model and reasoning effort against the catalog. Use the server model/default effort when unavailable and show a notice.
9. If signed in and the saved account scope matches, call `thread/resume`. On success, fetch/consume authoritative history through the schema-supported thread snapshot/read path.
10. If resume fails, preserve the saved thread ID, enter `ThreadState::ResumeFailed`, and offer `/resume`; never create a replacement automatically.

Subscribe to inbound events before start/resume requests and buffer events until the response establishes the authoritative thread binding.

### Authentication

- `/login` sends `account/login/start` with `type: "chatgpt"`.
- `/login device` sends the supported `chatgptDeviceCode` variant, opens its HTTPS verification page, and renders the one-time code without persisting or logging it.
- Parse `authUrl`, allow only the schema/documented HTTPS login flow, and spawn macOS `open` directly with the URL as one argument.
- Correlate `account/login/completed`, including schema-valid ID-less failure notifications when exactly one login is active, then refresh state with `account/read`.
- While a login is pending, `/logout` sends `account/login/cancel` and reconciles with `account/read`; when signed in it sends `account/logout`. Neither path destroys the saved thread record.
- API-key and externally managed token modes have no command, configuration, or fallback path.
- Reject `account/chatgptAuthTokens/refresh`; its appearance indicates a configuration/version violation.

Persist an account scope only from a stable identifier exposed by the protocol. With the current schema, a normalized ChatGPT email is the available scope when non-null; store it only in the owner-readable preferences file and never log or render it. If no stable scope is available, fail closed by disabling automatic resume rather than risk cross-account attachment.

After an account change, never resume the prior account’s thread. Preserve the record and show an actionable account-mismatch message.

### Model and reasoning selection

Each catalog item retains `id`, display name, `isDefault`, `defaultReasoningEffort`, and `supportedReasoningEfforts`.

- `/model` lists choices; `/model <id>` selects a catalog entry.
- Model changes immediately revalidate reasoning. If the prior effort is unsupported, select the new model’s server default and notify the user.
- `/reasoning` lists the current model’s choices; `/reasoning <value>` selects one.
- Invalid selections remain local and show available values.
- Persist settled selections. Never hard-code model IDs or effort names.

A model change affects the next turn and the saved preference; use the current schema’s supported thread/turn override field rather than mutating raw JSON.

### Thread and turn lifecycle

- On first prompt with no saved thread, call `thread/start` using the selected model, isolated `cwd`, read-only sandbox, never-approve policy, and validated safety overrides. Persist the returned ID only after success.
- On an active thread, call `turn/start` with one text input plus current model/effort and the same safety policy.
- Scope all events by expected thread and turn; scope assistant text by item ID. Ignore stale/mismatched events and diagnose IDs only.
- Append `item/agentMessage/delta` exactly once.
- Treat completed assistant items as authoritative: emit only a missing UTF-8 suffix when the final text extends the streamed prefix; accept identical text; surface a reconciliation error for a non-prefix contradiction rather than duplicating or silently replacing output.
- `turn/completed` is terminal for completed, interrupted, or failed turns. The composer becomes sendable only after terminal state.
- Map Escape during an active turn to `turn/interrupt`; Ctrl-C and `/quit` initiate application shutdown.
- A process exit, malformed frame, timeout, unknown required field, or version mismatch becomes a visible recoverable connection error. Restart requires an explicit local action; it must not create a thread.

Any approval, tool, permission, elicitation, attestation, or unknown server request is denied with the schema-required negative/error response, marks the turn failed, and raises a safety violation. Never render it as a tool card or ask the user to approve it.

## Slash commands and TUI

`command::parse` trims the leading command token, preserves optional arguments, and returns a typed intent or local error. Supported commands are exactly:

- `/login [browser|device]`, `/logout`, `/model [id]`, `/reasoning [value]`,
  `/resume`, `/help`, and `/quit`.

Unknown slash commands never reach Codex. Normal non-empty text becomes `Intent::SendMessage`. Reject sends while signed out, disconnected, resuming, or already running a turn with an actionable notice.

Keep the layout simple:

- status bar: connection, auth, thread, model, reasoning, turn state;
- scrollable transcript;
- one composer;
- one-line help/error area.

Essential keys: Enter sends, a documented alternate inserts a newline, Escape interrupts an active turn or clears a local overlay, scrolling keys move transcript history, Ctrl-C quits cleanly. Sanitize C0/C1 control characters and unsafe escape sequences from every untrusted string before measuring or rendering it.

## Persistence and account scope

Store JSON at the platform application-data location—on macOS, under `~/Library/Application Support/AgentHarness/`—not in the repository or launch directory.

```json
{
  "version": 1,
  "account_scope": {
    "kind": "chatgpt_email",
    "value": "normalized@example.com"
  },
  "thread_id": "thr_...",
  "model_id": "server-model-id",
  "reasoning_effort": "server-advertised-value"
}
```

All fields except `version` may be null. Do not persist transcripts, turns, login URLs, plan type, tokens, cookies, protocol payloads, or server stderr.

Writes use a same-directory temporary file, owner-only mode, flush/sync, and atomic rename. Save only settled state:

- after successful thread start or resume;
- after validated model/reasoning changes; and
- during orderly shutdown.

Never clear a saved ID because resume failed. A missing file is first run. A corrupt or unknown-version file is ignored with a notice and is not overwritten until new settled state exists. Migration is explicit by version.

## Terminal and shutdown lifecycle

Create a `TerminalGuard` before drawing. It owns raw mode, alternate screen, cursor visibility, and any paste/mouse modes; `Drop` restores them idempotently. Install panic and signal handling so restoration occurs on normal return, application error, panic, Ctrl-C, and termination signals supported on macOS/Linux.

Shutdown order:

1. stop accepting input and mark the UI as shutting down;
2. interrupt or settle the active turn within a bounded grace period;
3. persist settled preferences;
4. close app-server stdin, request/await child exit, terminate if necessary, and reap it;
5. stop workers and diagnostic flushing; then
6. restore cursor, alternate screen, and raw mode.

The TUI owns stdout. App-server stderr and sanitized diagnostics go only to the diagnostic file/sink.

## Milestones and work items

### Milestone 0 — Protocol and safety foundation

1. Create the minimal crate and reusable transport/protocol surface needed for schema validation and later session work: direct process ownership, bounded JSONL, request correlation/timeouts, continuous stderr draining, one stdin writer, and bounded reap/kill shutdown.
2. Regenerate the installed stable schema outside source control; capture the tested CLI version and exact required methods/fields.
3. Implement the isolated process policy, pragmatic feature overrides, empty non-project `cwd`, never-approve/read-only/no-tool-network settings, and exhaustive server-request denial.
4. Add fake-server tests for every generated server-request method, including legacy approval names.
5. Record the tested CLI version and protocol evidence; incomplete built-in tool suppression is not a blocker.

**Exit:** the reusable transport initializes safely, all known and unknown server requests are denied fail-closed, and deterministic offline tests pass. No authenticated adversarial conformance proof is required.

### Milestone 1 — Pure application core and local services

1. Add `AppState`, intents, domain events, effects, and the pure reducer.
2. Implement slash-command parsing/help and model/reasoning validation.
3. Implement `PreferencesV1`, application paths, atomic persistence, browser URL validation, and sanitized diagnostics.
4. Unit-test startup, signed-out behavior, resume success/failure, account change, streaming terminal states, cancellation, selection fallback, persistence, and unknown commands.

**Exit:** core behavior is deterministic under fake ports with no terminal, app-server, login, or network.

### Milestone 2 — Long-lived transport and typed protocol

1. Extend the Milestone 0 transport with connection generation scoping, method-specific timeouts, stale-response handling, restart behavior, and sanitized diagnostic sinks.
2. Add the remaining stable Serde envelopes required by the MVP while tolerating unknown fields and notifications.
3. Expand the fake child and fixtures from request denial to auth, paginated models, start/resume/read, turn streaming, malformed JSON, unknown messages, request timeout, EOF, stderr flooding, and cleanup.

**Exit:** integration tests prove ordering, correlation, recovery, no pipe deadlock, no leaked child, and sanitized diagnostics.

### Milestone 3 — Session startup, auth, catalog, and persistence

1. Implement initialization and account-state reduction.
2. Implement ChatGPT browser login/logout and auth notifications.
3. Fetch all model pages and enforce catalog-derived reasoning choices.
4. Implement account-scoped automatic resume and authoritative history restoration.
5. Preserve stale IDs and expose explicit `/resume` failure/retry behavior.

**Exit:** the fake server covers first run, signed out, login, same-account resume, account mismatch, invalid saved choices, and failed resume without replacement-thread creation.

### Milestone 4 — Conversational vertical slice

1. Lazily start the first thread on the first valid prompt.
2. Start one turn at a time and reduce scoped deltas/items/terminal events.
3. Reconcile streamed and final assistant text without duplication.
4. Implement interrupt, process-exit, retryable error, and stale-event behavior.

**Exit:** a backend-level test completes new and resumed conversations, including UTF-8 suffix recovery, contradictory snapshots, failure, and interruption.

### Milestone 5 — Ratatui shell and terminal safety

1. Implement the single-screen transcript/composer/status UI and typed input translation.
2. Keep all backend, browser, and persistence operations off the render path.
3. Add control-character sanitization, scrolling, help, selection views, and actionable empty/error states.
4. Add RAII terminal restoration and ordered shutdown.

**Exit:** Ratatui `TestBackend` snapshots cover signed out, resume failed, ready, streaming, completed, and error states; terminal lifecycle tests cover every exit path available without a real terminal.

### Milestone 6 — Hardening and release validation

1. Run protocol fixtures against the pinned minimum and current installed CLI schemas.
2. Exercise app-server crashes, malformed frames, timeouts, unknown notifications/requests, login cancellation, corrupt state, and signals.
3. Run the manual conversation-focused safety and smoke flow.
4. Run formatting, linting, and all default tests on stable Rust/macOS; add macOS CI only for the MVP.

**Exit:** every acceptance criterion below passes, including the conversation-focused safety defaults and fail-closed request handling.

## Testing strategy

### Unit tests

- command parsing, whitespace/arguments, and unknown-command locality;
- reducer transitions for first run, signed out, auto-resume, failed resume, account change, streaming, completion, interruption, and app-server exit;
- catalog pagination/deduplication and reasoning fallback;
- persistence round trip, permissions, migration, missing/corrupt files, atomic replacement;
- JSONL framing, request/error correlation, timeout cleanup, unknown notification handling;
- thread/turn/item routing, duplicate suppression, UTF-8 suffix recovery, and non-prefix mismatch;
- control-character/escape sanitization and diagnostic redaction.

### Fake-child integration tests

Script JSONL scenarios for:

- initialize ordering and duplicate/pre-initialize failures;
- account read/login completion/logout;
- multi-page model discovery;
- first thread, saved-thread resume, history read, and resume failure;
- early notifications arriving before start/resume responses;
- streamed deltas plus final snapshots;
- every server-request denial path;
- malformed JSON, oversized frames, unknown notifications, EOF, timeout, stderr flooding, and child cleanup.

Default tests must not require Codex, ChatGPT login, or network access.

### Rendering tests

Use Ratatui `TestBackend` for stable layouts at small and normal terminal sizes. Assert sanitized content and state-specific actions, not elaborate styling.

### Real tests

Keep authenticated smoke coverage manual or ignored and opt-in. Use a dedicated Codex home/conversation directory and leave no repository artifacts. Validate login, transcript behavior, isolation defaults, and visible handling of any unexpected server request. Exhaustive adversarial proof that built-in tools are absent is out of scope. Real tests never run in default CI.

Before handoff:

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets
```

## Manual smoke flow

1. Start with no AgentHarness preferences or dedicated Codex home. Confirm the TUI opens signed out and remains usable.
2. Run `/login`; verify one HTTPS URL opens, browser completion returns to ready state, and no token appears in output or diagnostics.
3. Inspect `/model` and `/reasoning`; select server-advertised values and verify invalid values remain local.
4. Send a conversational message. Confirm the thread is created once, text streams without duplication, and completion is clear.
5. Quit normally and relaunch. Confirm the same account and thread resume automatically and history is restored from app-server.
6. Simulate an invalid/stale saved ID. Confirm the ID remains stored, failure is explicit, and no replacement thread is created.
7. Logout and sign in as a different account. Confirm the prior thread is not resumed.
8. Interrupt a long turn, terminate app-server, and exercise Ctrl-C. Confirm errors are visible, the child is reaped, preferences remain valid, and the terminal is restored.
9. Confirm the session uses the dedicated Codex home, empty conversation directory, read-only/no-tool-network settings, never auto-approves, and reports any unexpected server request without exposing an approval flow.
10. Repeat startup with an unsupported CLI version and verify the upgrade message is actionable.

## Acceptance criteria

- Stable Rust builds a single macOS-first Ratatui client with no Windows code or CI.
- The client directly owns one long-lived `codex app-server` stdio child and reaps it on every exit path.
- Only managed ChatGPT login is exposed; AgentHarness never reads or logs Codex credential files.
- Models and reasoning levels come entirely from the paginated server catalog.
- Exactly one thread exists in the UI; its ID is saved only after success and automatically resumed only for the matching account.
- Failed resume preserves the ID and never silently creates a replacement.
- One turn streams item-scoped assistant text, reconciles the final snapshot, and visibly completes, interrupts, or fails.
- Slash commands remain local; unknown commands are never sent as prompts.
- Ratatui never blocks on Codex, browser, or persistence work; untrusted terminal text is sanitized.
- Preferences are minimal, versioned, owner-readable, atomic, and contain no transcript or credentials.
- Process, framing, timeout, version, account, and protocol errors are visible and recoverable.
- Every server request is denied fail-closed.
- The tested CLI version uses pragmatic isolation and feature-disable defaults; retained built-in tool capability is acceptable and does not block release.
- Default tests are deterministic and offline; format, Clippy, and all-target test checks pass.

## Risks and open decisions

| Risk or decision | Plan |
|---|---|
| No stable complete tool-disable control | Accept retained built-in capability; keep the UI conversation-focused and apply pragmatic isolation, read-only/no-tool-network settings, never auto-approve, and fail-closed request handling. |
| CLI protocol/version skew | Regenerate the installed schema, keep typed coverage narrow, and feature-detect or enforce the methods needed by the tested version. |
| Tool-bearing features change after the plan | Regenerate the server-request inventory for supported CLI upgrades. Unknown server requests remain safety violations and are denied. |
| Current account data lacks a stable non-email ID | Use normalized ChatGPT email only when present and never log/render it; disable auto-resume when account equivalence cannot be proved. Revisit if the schema adds an opaque account ID. |
| Early or duplicate events corrupt transcript | Subscribe before lifecycle calls, buffer until binding, scope by thread/turn/item, and reconcile final snapshots byte-wise. |
| Terminal corruption or orphaned child | One RAII terminal guard and one idempotent ordered shutdown path, tested under errors and signals. |
| Diagnostics leak content or auth data | Allowlist metadata fields; never persist raw protocol/stderr, prompts, replies, emails, or URL queries. |
| Dependency/architecture growth | One crate, one provider, one thread, narrow traits only at I/O seams; defer everything outside the MVP contract. |

Milestone 0 records the CLI version used to generate and test the protocol surface. Selecting a long-term minimum supported version remains a later compatibility decision, not a tool-suppression release gate.

## References

- [AgentHarness requirements](../../AGENTS.md)
- [Official Codex app-server protocol](https://learn.chatgpt.com/docs/app-server)
- [Official Codex configuration reference](https://learn.chatgpt.com/docs/config-file/config-reference)
- [Official Codex MCP configuration](https://learn.chatgpt.com/docs/extend/mcp)
- [Official Codex approvals and security](https://learn.chatgpt.com/docs/agent-approvals-security)
- Installed discovery schema: `codex-cli 0.144.6`, generated outside the repository at `/private/tmp/agentharness-codex-schema-20260721/`; regenerate rather than committing or relying on that path
