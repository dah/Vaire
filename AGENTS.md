# AgentHarness project guide

## Scope of this guide

This file governs contributors and coding agents developing AgentHarness. It is not an instruction source for the conversational agent that users chat with inside AgentHarness. Never copy, summarize, expose, or inject this file into Codex `baseInstructions`, `developerInstructions`, thread history, or user input. Changing this file alone must never change end-user agent behavior.

AgentHarness currently sends no custom base or developer instructions to runtime threads. If product-specific runtime instructions are introduced, keep them in a separate, explicitly named source or configuration, supply them deliberately through fields supported by the installed app-server schema, apply them consistently on thread start and resume, and cover their precedence and lifecycle with tests. Do not use `AGENTS.md` as that source.

## Mission

AgentHarness is a small, dependable local terminal client built on Codex app-server. Its first two milestones are complete: users can authenticate with a ChatGPT subscription, select an available Codex model and reasoning level, manage one active thread among account-scoped saved threads, stream replies and emitted reasoning, run Codex command/file tools, and automatically resume the working thread after restarting when the authenticated account can be safely matched.

Preserve this working vertical slice while evolving the product deliberately. Prefer small end-to-end increments over abstractions for hypothetical features, and make every expansion beyond the shipped baseline an explicit milestone decision.

## Shipped product baseline

The following behavior is implemented and is a regression contract unless an explicitly approved milestone changes it:

- Use stable Rust and Ratatui.
- Support macOS only today. Keep operating-system seams small enough to add Linux later.
- Do not add Windows support, Windows-specific code, or Windows CI.
- Treat the Codex CLI as an installed runtime dependency.
- Require `codex-cli` 0.144.6 or newer and communicate with one long-lived `codex app-server` process over stdio.
- Use the supported Codex ChatGPT sign-in flow so usage follows the user's ChatGPT subscription. API-key authentication is not supported.
- Query app-server for the available model catalog and supported reasoning levels. Do not hard-code model availability.
- Keep exactly one active chat thread in the UI.
- Persist the active Codex thread ID and automatically resume that same working thread on the next launch when authenticated account scope is available and matches.
- Let the user eagerly create a thread with `/new`; use `/resume` for the account-scoped saved-thread picker and confirmed deletion of one or all inactive threads. Never delete the active thread through the picker.
- Explicitly create every new non-ephemeral thread with `threadSource: "appServer"`. For compatibility, `/resume` discovers both `appServer` and legacy `vscode` sources, then retains only threads with the exact dedicated conversation cwd whose IDs are already registered to the authenticated account. Never auto-register discovery results.
- Stream agent-message deltas into the transcript and show a clear terminal state when each turn completes or fails.
- Request detailed reasoning summaries for every turn and show them with any reasoning text explicitly emitted by Codex in the optional Reasoning panel. Configure `show_raw_agent_reasoning=true` at dedicated app-server process and thread start/resume boundaries as best-effort provider/model-dependent behavior; summaries are the fallback. Hidden/private chain-of-thought is unavailable and must never be exposed or inferred.
- Enable Codex command-line and file tools with unrestricted same-user access and no approval prompts. The TUI remains conversational and has no tool cards, approval controls, or rich command-progress workflow.
- Show the authenticated account identity and remaining context percentage in the header, and an ephemeral activity squiggle before the first assistant text.
- Keep normal tests and CI offline; installed-CLI smoke tests remain explicit and ignored by default.

## Current scope boundaries

The following capabilities are outside the shipped baseline. Do not introduce them incidentally. Before implementing one, define an explicit milestone with user experience, protocol and safety implications, acceptance tests, and corresponding updates to this guide:

- Multiple simultaneously active threads, forking, or branching.
- Approval UI, per-command confirmation, tool cards, or a rich command-output workflow. Do not describe `approval_policy="never"` as automatic approval; supported command/file operations execute without requesting approval.
- Additional model providers or API-key login.
- Plugins, skills, MCP servers, orchestration, or multi-agent execution.
- Elaborate theming or customization that gets ahead of functional work.

These are durable constraints unless the project owner explicitly changes them:

- Windows support.
- A runtime or build dependency on RepoPrompt.

The completed milestone plans under `docs/plans/` are historical implementation records, not a live roadmap. Establish and document the next milestone before widening product scope.

## Completed milestone: richer interactive runtime

The post-MVP milestone documented in `docs/plans/agentharness-interactive-runtime-2026-07-22.md` is complete. Its capabilities are part of the regression baseline:

- create a new thread with `/new`, choose among saved Codex threads through `/resume`, and delete one or all old threads with confirmation while retaining exactly one active thread;
- toggle a right-side Reasoning panel that displays only reasoning summaries or reasoning text actually emitted by app-server, never inferred or hidden chain-of-thought;
- enable Codex command-line and file tools with full local access, without adding an approval UI;
- show the authenticated account identity in the header;
- show an animated pre-response thinking indicator that disappears on the first assistant-text delta; and
- show the remaining context-window percentage in the top-right header when app-server supplies enough token-usage data.

Treat further expansion beyond these capabilities as a new milestone decision with protocol, safety, UX, documentation, and test implications made explicit first.

## Current user experience

The application opens directly into the chat TUI. On startup it must continue to:

1. Restore non-secret local preferences.
2. Start and initialize the Codex app-server connection.
3. Detect the current Codex authentication state.
4. Automatically resume the saved thread when authentication and the saved thread are valid.
5. Otherwise remain usable and explain the next action, such as `/login` or `/resume`.

Normal text entered in the composer starts a turn in the active thread. Slash commands are the control surface for application actions. The current command vocabulary is:

- `/login` or `/login browser` — start the Codex ChatGPT browser sign-in flow; `/login device` is the supported fallback when callback login is unavailable.
- `/logout` — sign out through Codex, or cancel the currently pending sign-in.
- `/model` — inspect or select a model returned by app-server.
- `/reasoning` — inspect or select a reasoning level supported by the current model.
- `/new` — eagerly create and activate a fresh thread without deleting the previous thread.
- `/resume` — open the account-scoped saved-thread picker.
- `/thinking` — toggle the right-side Reasoning panel of reasoning summaries or reasoning text emitted by app-server. Distinct from `/reasoning`, which selects the reasoning effort level.
- `/help` — show supported commands and essential keys.
- `/quit` — shut down cleanly and exit.

Unknown slash commands stay local and produce an actionable error directing the user to `/help`; never send them to the model as prompts.

The current key contract is:

- `Enter` sends and `Alt-Enter` inserts a newline.
- `PageUp`, `PageDown`, arrow keys, `Home`, and `End` scroll the transcript.
- In the thread picker, arrows or `j`/`k` move, `Enter` resumes, `d` requests deletion of the selected inactive thread, and `D` requests deletion of all inactive threads. Deletion requires a second `Enter`; `Escape` cancels or closes the picker. The active thread is protected.
- `Escape` closes local help or errors, cancels a picker action, or interrupts the active turn.
- `Ctrl-C` shuts down cleanly.

The cyan header shows the authenticated email when app-server exposes it and a right-aligned `Context N%`; it shows `Context --` when usable context-window data is unavailable. While a turn is active but no nonempty assistant text has arrived, a display-only animated squiggle appears in the conversation pane and disappears on the first text or terminal turn state.

Do not silently create a replacement thread when resume fails. Preserve the saved ID, report the failure clearly, and let the user choose the next action. The current account scope is the normalized ChatGPT email; if app-server does not expose it, do not auto-resume or manually resume a saved thread. On a first run with no saved thread, create the single working thread when it is first needed.

## Codex integration rules

The official app-server documentation and the schema generated by the installed Codex CLI are the protocol sources of truth:

- <https://learn.chatgpt.com/docs/app-server>
- `codex app-server generate-json-schema --out <directory>`

The installed CLI schema wins when documentation, memory, examples, or the optional reference project disagree.

- Resolve and spawn the `codex` executable directly without a shell.
- Maintain one long-lived app-server child process rather than spawning a command for every message.
- Use the app-server protocol for authentication state, model discovery, thread lifecycle, turns, and streamed events. Do not scrape human-oriented Codex TUI or CLI output.
- Keep JSON-RPC framing and request correlation inside the transport layer. Keep thread and turn behavior out of Ratatui widgets.
- Initialize once per connection, correlate every request and response, and consume stdout and stderr continuously so the child cannot deadlock on a full pipe.
- Scope streamed deltas and terminal events to their thread, turn, and item. Reconcile final snapshots without duplicating streamed text.
- Treat process exit, malformed frames or required payloads, stale thread IDs, timeouts, and version skew as visible failures. Tolerate genuinely unknown notification methods where safe; if diagnosing them, record only sanitized method metadata. Preserve settled state where possible; when the app-server connection becomes unusable, tell the user to restart because the current product has no in-app reconnect.
- Enforce the tested minimum Codex CLI version with an actionable upgrade message. Regenerate the installed schema and update protocol fixtures, safety policy, and compatibility notes whenever the tested baseline changes.
- Derive models and reasoning choices from `model/list` or its current schema equivalent. Validate the reasoning choice when the model changes. If a saved choice is unavailable, use the server default and tell the user.
- Development code must never intentionally read, copy, print, persist, or commit tokens from any Codex home, including AgentHarness's dedicated one. Let Codex own credential storage and refresh. Unrestricted model-run commands are not OS-isolated from same-user credential files; document that risk rather than claiming containment.
- Never log authorization headers, access tokens, cookies, full auth payloads, prompts, or replies by default. Redact sensitive protocol fields from diagnostics.
- Open login URLs without shell interpolation and accept only the expected safe URL schemes.
- Scope saved thread state to the authenticated account when the protocol exposes an account identifier. Never resume a thread across an account switch.
- Keep repository development instructions out of runtime conversations. The dedicated Codex home and non-project startup directory prevent automatic inheritance of this repository's `AGENTS.md`; they do not prevent a `danger-full-access` command from reading arbitrary same-user paths. Any future runtime prompt must use a separate, explicit product path.

### Full-access runtime boundary

The current product intentionally enables Codex command-line and file tools without tool cards or an approval UI. This is an unrestricted same-user execution boundary, not a sandbox.

- Apply `sandbox_mode="danger-full-access"` / `dangerFullAccess` and `approval_policy="never"` at process, thread start/resume, and turn boundaries. Supported command/file operations execute directly without confirmation.
- The app-server inherits the launcher environment except inherited `CODEX_*` variables are removed and AgentHarness supplies its dedicated `CODEX_HOME`. Tool shells use `shell_environment_policy.inherit="all"`.
- Codex's default name-based filtering of variables containing `KEY`, `SECRET`, or `TOKEN` is incomplete and is not a security boundary. Values such as `DATABASE_URL` and `SSH_AUTH_SOCK` may remain available.
- Full-access commands may use SSH agents, macOS Keychain, credential and configuration files, authenticated CLIs, and the network to act locally or remotely.
- Use a dedicated AgentHarness Codex home and persistent non-project `runtime/conversation` starting directory to avoid automatic project inheritance. Commands can leave that directory or use absolute paths, its files survive restarts, and the dedicated Codex home—including Codex-owned authentication state—remains reachable to same-user full-access tools. These are organizational boundaries only.
- Keep optional apps, integrated web search, MCP, plugins, and multi-agent features disabled for this milestone. Command-line programs can still access the network.
- Fail closed on every approval, permission, elicitation, attestation, or other server request that lacks an explicitly implemented and tested product workflow.
- Keep exhaustive fake-server coverage proving that every unimplemented server request is denied and that tool-event volume cannot starve rendered conversation events or the fail-closed path. Add positive coverage for any request workflow that a future milestone explicitly supports.

## Architecture boundaries

Preserve these established concern boundaries:

- **Application state:** the authoritative reducer for startup, auth, connection, thread picker and active thread, current turn, emitted thinking, context usage, selected model and reasoning, and shutdown.
- **TUI:** rendering and input translation only. It emits typed intents and renders state; it does not call Codex directly.
- **Slash commands:** pure parsing, validation, help text, and conversion to application intents.
- **App-server transport:** child-process ownership, stdin and stdout framing, request IDs, pending requests, timeouts, notifications, stderr capture, and shutdown.
- **Codex protocol:** narrow typed Serde envelopes for only the methods and events the current product uses. Tolerate unknown fields and notifications where safe.
- **Codex session service:** initialization, auth, model catalog, thread start/list/resume/delete, turn start, and protocol-event reduction.
- **Persistence:** versioned, non-secret preferences such as account scope, thread ID, model, and reasoning level.
- **Platform integration:** browser launching, application-data directories, signals, and other macOS and Linux seams.

The UI consumes domain events, never raw protocol JSON. Prefer explicit message passing between the async backend and the render loop. Never block Ratatui on login, model responses, disk I/O, or a running turn.

Use traits or equivalent injection seams around transport, persistence, browser opening, terminal operations, process spawning, and configurable timeouts so core state transitions can be tested without a terminal or real Codex account. Do not reproduce a large coordinator from the reference application; preserve the useful separation with the smallest design that serves current requirements.

## Persistence and lifecycle

- Store only the minimum local state needed to restore the experience. Codex remains the source of truth for thread history.
- Put application state in the appropriate macOS application-support directory, not in the repository or current working directory.
- Use a small versioned serialization format and atomic replace-on-write behavior.
- Treat a missing or invalid state file as a clean first run, not a crash.
- Save a thread ID only after successful creation or resumption. Do not discard a previously saved ID because a transient resume failed.
- Restore available transcript history from app-server rather than duplicating the transcript in local state.
- Sanitize untrusted control characters before rendering model text in the terminal.
- Restore terminal raw mode and alternate-screen state on every exit path, including errors, panics, and signals.
- On shutdown, stop input, persist settled state, close or terminate app-server, reap the child process, and then restore the terminal.
- The TUI stdout belongs to terminal rendering. Send sanitized diagnostics to a file or another channel that cannot corrupt the screen.

## Testing and validation

Testing is a first-class part of development, not an afterthought or a final cleanup phase. Design test seams and test cases alongside each feature, write tests before or with the implementation, and consider behavior-changing work incomplete until its relevant tests exist and pass. When practical, begin every bug fix with a regression test that demonstrates the failure.

Keep the core testable without launching a terminal or signing in. Maintain coverage for:

- Slash-command parsing, including unknown commands.
- Application-state transitions for first run, signed-out startup, automatic resume, failed resume, streaming, completion, cancellation, account changes, and app-server exit.
- Model and reasoning validation.
- Persistence round trips, version handling, any migrations introduced after v1, missing files, and corrupt files.
- JSON-RPC framing, request correlation, timeouts, unknown messages, delta deduplication, and event reduction with fixtures.
- Terminal rendering with Ratatui `TestBackend`, including unauthenticated, resumed, streaming, and error states.
- Integration behavior against a fake app-server child for initialization, auth events, paginated model discovery, new and resumed threads, streaming, malformed JSON, unknown requests, EOF, timeout, and child cleanup.
- Thread-picker listing, switching, confirmed single/bulk deletion, active-thread protection, partial failures, and stale result correlation.
- Emitted-reasoning scoping, full-access policy and inherited-environment behavior, account/header sanitization, activity-animation lifecycle, context-usage scoping and arithmetic, and narrow/normal cross-feature Ratatui rendering.
- Tool-heavy event floods and unexpected server requests, proving meaningful conversation events remain deliverable and every unimplemented request still fails closed.
- If runtime-agent instructions are introduced, test new-thread and resumed-thread behavior, instruction precedence, and the invariant that this development `AGENTS.md` is never injected into a user conversation.

Keep any future real authenticated Codex smoke test manual or ignored. Default tests and CI must not require ChatGPT login or network access.

The required handoff checks are:

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets
```

Run the smallest relevant checks while iterating, then all three before handing off a completed change. Add or update tests with behavior changes. Do not weaken lint settings or delete a failing test merely to make a check pass.

## RepoPrompt-assisted development

RepoPrompt is both a development tool for AgentHarness and, separately, the home of the optional source reference below. Use it when available to keep discovery, context selection, planning, and review focused.

- Prefer RepoPrompt MCP tools when they are available. When only the CLI is available, use `rpce-cli`; start with `rpce-cli -e 'windows'`, then target the window whose workspace contains AgentHarness.
- Window IDs are runtime state. Never hard-code one in scripts or durable instructions.
- Prefer RepoPrompt `tree`, `search`, `read`, `structure`, `edit`, and read-only `git` operations for AgentHarness repository work when the relevant interface is available.
- Use selection plus Context Builder or chat and oracle for architecture work that benefits from curated cross-file context. Use read-only explore agents for narrow side investigations.
- Keep selections task-specific. Do not load the whole reference project when a transport file, protocol model, or focused test is enough.
- When both roots are loaded, qualify every mutable AgentHarness path with `AgentHarness/`. Qualify reference reads with `repoprompt-ce/`.
- RepoPrompt is a developer aid, not an application dependency. Building, testing, and running AgentHarness must not require RepoPrompt or `rpce-cli`.

## Optional external reference: strictly read-only

`/Users/danhancu/Developer/RepoPrompt/repoprompt-ce` is optional design guidance. It may be loaded as a second RepoPrompt workspace root, but it is outside the AgentHarness repository and is immutable during AgentHarness work.

Useful read-only areas include:

- `Sources/RepoPrompt/Infrastructure/AI/Providers/Codex/AppServer`
- `Sources/RepoPrompt/Features/AgentMode`

Consult it for patterns such as transport and session separation, model discovery, event reduction, auth recovery, and diagnostics. Implement needed behavior independently in Rust.

### Hard boundary

Never modify any path under `/Users/danhancu/Developer/RepoPrompt/repoprompt-ce` while working on AgentHarness.

This prohibition includes:

- Source, tests, fixtures, documentation, configuration, and repository metadata.
- Formatting, generated files, schemas, logs, caches, build products, and test artifacts.
- Staging, committing, branching, rebasing, cleaning, or any other Git mutation.
- Running commands in that repository that may write, format, build, test, generate, install, or clean.

Only read-only operations are allowed there: tree, search, read, structure, and read-only Git inspection. Before every RepoPrompt edit or file operation, verify that the target is root-qualified under `AgentHarness/`. If the target is ambiguous or points at `repoprompt-ce/`, stop without writing.

Do not vendor, symlink, copy wholesale, add a path dependency to, or assume the reference exists in CI or on another developer machine. If a small licensed idea or excerpt is intentionally reused, verify and preserve all attribution and license obligations. Capture borrowed behavior in AgentHarness tests or documentation so contributors without the reference path are unaffected.

AgentHarness requirements, official Codex documentation, and the installed Codex schema always take precedence over the reference implementation.

## Working rules

- Keep changes scoped to explicitly approved work; avoid speculative provider systems, plugin systems, tool frameworks, or multi-thread orchestration.
- Prefer small, typed domain models over unstructured JSON beyond the protocol boundary.
- Preserve unknown protocol fields and tolerate unknown notifications where practical, but fail clearly when a required field is missing.
- Add dependencies intentionally. Favor well-maintained crates and avoid overlapping libraries for the same concern.
- Keep user-facing failures actionable and low-level details in sanitized diagnostics.
- Treat shipped behavior changes as incomplete until relevant regression tests and user-facing documentation are updated.
- Update this file when established commands, architecture, supported platforms, runtime-instruction behavior, or current product boundaries change.
