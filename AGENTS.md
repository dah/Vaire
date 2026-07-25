# Vairë project guide

## Scope of this guide

This file governs contributors and coding agents developing Vairë. It is not an instruction source for the conversational agent that users chat with inside Vairë. Never copy, summarize, expose, or inject this file into Codex `baseInstructions`, `developerInstructions`, thread history, or user input. Changing this file alone must never change end-user agent behavior.

Vairë currently sends no custom base or developer instructions to runtime threads. If product-specific runtime instructions are introduced, keep them in a separate, explicitly named source or configuration, supply them deliberately through fields supported by the installed app-server schema, apply them consistently on thread start and resume, and cover their precedence and lifecycle with tests. Do not use `AGENTS.md` as that source.

## Mission

Vairë is a small, dependable local terminal client with one active conversation across Codex app-server, OpenRouter text chat, and Claude Code CLI. Users can authenticate independently, select a provider-labelled model, manage provider-owned or Vairë-registered saved histories, stream replies, run Codex or Claude command/file tools, and safely restore the selected provider's working conversation after restart.

Preserve this working vertical slice while evolving the product deliberately. Prefer small end-to-end increments over abstractions for hypothetical features, and make every expansion beyond the shipped baseline an explicit milestone decision.

## Shipped product baseline

The following behavior is implemented and is a regression contract unless an explicitly approved milestone changes it:

- Use stable Rust and Ratatui.
- Support macOS only today. Keep operating-system seams small enough to add Linux later.
- Do not add Windows support, Windows-specific code, or Windows CI.
- Treat the Codex and Claude Code CLIs as installed runtime dependencies.
- Require `codex-cli` 0.144.6 or newer and communicate with one long-lived `codex app-server` process over stdio.
- Require Claude Code 2.1.218 or newer and use one directly spawned non-interactive stream-json child per Claude turn.
- Use the supported Codex ChatGPT sign-in flow so usage follows the user's ChatGPT subscription. Codex API-key authentication is not supported.
- Use Claude Code's native `auth login --claudeai`, `auth status --json`, and `auth logout` flows so Claude usage follows the user's Claude.ai subscription. Claude Code owns browser login, token refresh, and macOS Keychain storage; Vairë must never receive, read, copy, persist, migrate, or delete those tokens.
- Revalidate Claude native subscription auth immediately before every turn. A signed-out, unsupported, or unverifiable status blocks the turn.
- The supported Claude status contract exposes no stable account identifier. Never silently auto-resume a saved Claude session at startup or after authentication; preserve its pointer and require explicit `/resume` or `/new`. Treat Vairë-owned Claude registrations as same-user local records, not provider-account-scoped records.
- Query app-server for the available model catalog and supported reasoning levels. Do not hard-code model availability.
- Keep exactly one active chat thread in the UI.
- Persist the active Codex thread ID and automatically resume that same working thread on the next launch when authenticated account scope is available and matches.
- Let the user eagerly create a thread with `/new`; use `/resume` for the account-scoped saved-thread picker and confirmed deletion of one or all inactive threads. Never delete the active thread through the picker.
- Explicitly create every new non-ephemeral thread with `threadSource: "appServer"`. For compatibility, `/resume` discovers both `appServer` and legacy `vscode` sources across the exact current and historical pre-rename conversation cwd filters, then retains only IDs already registered to the authenticated account. Never auto-register discovery results.
- Stream agent-message deltas into the transcript and show a clear terminal state when each turn completes or fails.
- Request detailed reasoning summaries for every turn and show them with any reasoning text explicitly emitted by Codex in the optional Reasoning panel. Configure `show_raw_agent_reasoning=true` at dedicated app-server process and thread start/resume boundaries as best-effort provider/model-dependent behavior; summaries are the fallback. Hidden/private chain-of-thought is unavailable and must never be exposed or inferred.
- Enable Codex and Claude command-line and file tools with unrestricted same-user access and no approval prompts. The TUI remains conversational and has no tool cards, approval controls, or rich command-progress workflow.
- Show the authenticated account identity and remaining context percentage in the header, and an ephemeral activity squiggle before the first assistant text.
- Keep normal tests and CI offline; installed-CLI smoke tests remain explicit and ignored by default.
- Support exactly three providers: Codex app-server, OpenRouter text chat, and Claude Code CLI. Providers beyond these remain out of scope.
- Keep one active provider, conversation, and turn. Provider-tag models, conversations, turns, transcript, reasoning, context, and histories.
- Use the official OpenRouter `GET /api/v1/key`, authenticated `GET /api/v1/models/user`, and SSE `POST /api/v1/chat/completions` endpoints; OpenRouter tools and multimodal input are not supported.
- Store the OpenRouter API key through the injected credential-store port in owner-only plaintext `runtime/openrouter-home/api-key` (`0700` directory, exact `0600` regular file).
- Never inject `ANTHROPIC_API_KEY`, `CLAUDE_CODE_OAUTH_TOKEN`, or another Claude credential. Strip inherited `ANTHROPIC_*` and `CLAUDE_*` overrides so native CLI-owned subscription auth remains authoritative.
- Use only documented Claude CLI version/auth login/status/logout/print/stream-json/model/session/safe-mode/permission flags. Never parse or enumerate Claude private transcripts or automate the interactive chat TUI. Temporarily hand the real terminal to native foreground auth commands and restore the TUI afterward.
- Keep Vairë-owned Claude registrations and bounded display history separate from Claude-owned model context. `/resume` lists only registered UUIDs; deleting an inactive Claude row forgets Vairë's registration/display record and does not claim to delete opaque provider data.
- Treat macOS Keychain migration as mandatory technical debt for a later approved milestone. Save and verify the Keychain item before deleting the plaintext file, and preserve the file on any failure.
- Cross-provider `/model` selection is a hard blank-conversation boundary with no continuity or history transfer. `/resume` is the only operation that restores cross-provider history. Codex/OpenRouter same-provider changes may retain a conversation; changing a Claude alias starts blank because resumed CLI sessions retain their original model.
- Use `Vairë` (NFC, precomposed `ë`) for human branding and `vaire` for the Cargo package, crate, executable, protocol client name, Application Support child, and environment prefix. The active diagnostics file is `diagnostics/vaire.log`; `VAIRE_CODEX_BIN` and `VAIRE_CLAUDE_BIN` are the only Codex and Claude executable overrides, respectively.

## Current scope boundaries

The following capabilities are outside the shipped baseline. Do not introduce them incidentally. Before implementing one, define an explicit milestone with user experience, protocol and safety implications, acceptance tests, and corresponding updates to this guide:

- Multiple simultaneously active threads, forking, or branching.
- Approval UI, per-command confirmation, tool cards, or a rich command-output workflow. Do not describe `approval_policy="never"` as automatic approval; supported command/file operations execute without requesting approval.
- Additional model providers beyond Codex, OpenRouter, and Claude Code, or credential mechanisms beyond the approved OpenRouter file-backed key and CLI-owned Codex/Claude subscription flows.
- Plugins, skills, MCP servers, orchestration, or multi-agent execution.
- Elaborate theming or customization that gets ahead of functional work.

These are durable constraints unless the project owner explicitly changes them:

- Windows support.
- A runtime or build dependency on RepoPrompt.

The completed milestone plans under `docs/plans/` are historical implementation records, not a live roadmap. Establish and document the next milestone before widening product scope.

## Completed milestone: OpenRouter chat provider

The milestone documented in `docs/plans/openrouter-chat-provider-2026-07-22.md` is complete and part of the shipped regression baseline. Its contracts remain intact inside the bounded three-provider design.

The approved durable contracts are:

- preserve the complete shipped Codex app-server experience and its safety, account-scoping, resource, testing, and full-access contracts;
- add OpenRouter only as a text-chat provider using the official `GET /api/v1/key`, authenticated `GET /api/v1/models/user`, and SSE `POST /api/v1/chat/completions` endpoints;
- keep exactly one active conversation and one active turn across both providers, with provider-tagged model, conversation, turn, history, reasoning, and context ownership;
- store the OpenRouter API key through an injected credential-store port in the dedicated owner-only `runtime/openrouter-home/api-key` file: the directory is `0700`, the regular current-user-owned file is exactly `0600`, and the plaintext file is organizational isolation only—not encryption, secure storage, a sandbox, or protection from same-user processes and full-access Codex tools;
- never put the OpenRouter key or its fragments in application state, preferences, transcripts, notices, diagnostics, runtime arguments, environment, or test snapshots;
- treat migration of OpenRouter credentials to macOS Keychain as explicit technical debt for a later approved milestone: that migration must use the injected credential-store port, save and verify the Keychain item before deleting the plaintext file, and preserve the file on failure;
- make every cross-provider `/model` selection a hard fresh-conversation boundary with a blank transcript and no automatic continuity or history transfer; a same-provider model change may retain the current conversation;
- make `/resume` the only cross-provider operation that deliberately restores prior history, while retaining provider-owned inactive Codex threads and local OpenRouter conversations;
- use OpenRouter conversation schema V2 to retain nonempty assistant output from `Failed` streams only in the dedicated display-only `incomplete_assistant_text` field: `assistant_text` remains completed-only, startup and `/resume` restore failed partials with explicit `FailedIncomplete` transcript metadata, canonical history and outbound requests exclude them, and `InProgress`/`Interrupted` output is never checkpointed;
- parse bounded OpenRouter SSE events envelope-first: a non-null top-level provider-error object takes precedence over malformed completion siblings; a valid numeric or numeric-string error status is authoritative for HTTP classification and symbolic code/metadata is fallback only; missing, null, or empty choices are metadata-only; malformed optional usage is dropped atomically without clearing earlier valid usage; and strict semantic/terminal/EOF checks expose only closed secret-free failure stages that reach the transient turn-failure message but are never persisted;
- on choice-bearing OpenRouter chunks, let the first present nonempty server-reported model establish the stream model and require later present semantic models to match that server value; never compare response models with the requested model ID, and never establish or compare identity from choice-empty metadata;
- keep normal tests and CI offline with fake credentials, a fake credential store or temporary directories, and loopback fake HTTP/SSE; and
- keep RepoPrompt CE optional, strictly read-only, and absent from runtime/build dependencies.

Retain the plaintext-risk disclosure and Keychain-migration debt in future documentation changes.

## Completed milestone: Claude Code provider

The milestone documented in `docs/plans/claude-code-provider-2026-07-24.md` is complete and part of the shipped regression baseline. Its durable contracts are:

- use only the installed supported Claude Code CLI with direct no-shell process spawning and bounded stream-json parsing; this milestone originally established version 2.1.178, while the later reasoning-effort milestone below supersedes the current minimum with 2.1.218;
- authenticate only through Claude Code's native Claude.ai subscription login/status/logout commands, never a Vairë-entered key or an inherited ambient Anthropic/Claude override;
- leave OAuth tokens, refresh, browser login, and macOS Keychain storage entirely to Claude Code; never receive, inspect, copy, persist, migrate, or delete a Claude secret;
- revalidate supported native subscription status before every Claude turn and fail closed when it is signed out, unsupported, or unverifiable;
- because the supported status contract exposes no stable account identifier, never silently auto-resume a saved Claude session at startup or after authentication; preserve the pointer for explicit `/resume` or `/new` and treat registrations as same-user local records rather than provider-account-scoped records;
- suspend the TUI while a native auth command uses the real terminal, preserve the application's foreground process group and normal `Ctrl-C`/`Ctrl-Z` job control, reap it on completion or cancellation, and restore terminal state before redrawing;
- leave any obsolete experimental `runtime/anthropic-home/api-key` untouched and unreferenced by Vairë credential code; never auto-import or delete it, while documenting that unrestricted same-user tools can still reach the file;
- run each turn from the persistent non-project conversation cwd with a dedicated owner-only Claude home, safe mode, disabled customizations/plugins/MCP/hooks/skills/Chrome/web tools/subagents, and the documented dangerous permission bypass;
- treat this as unrestricted same-user execution rather than a sandbox or automatic approval, including the possibility that full-access commands reach Claude Code's Keychain-backed login or other credentials;
- use explicit canonical UUIDs for new/resumed sessions, retain only Vairë-owned bounded display history, and never parse, enumerate, mutate, or delete Claude private transcript/session files;
- expose only documented Claude aliases until authoritative stream metadata resolves the active model and start blank on every Claude alias change; this milestone historically left Claude reasoning effort and panel collection unsupported, and the later reasoning-effort milestone below supersedes only the effort-selection part;
- make Claude picker deletion registration/display-history removal only, with explicit confirmation wording and active-session protection;
- preserve saved UUID and display history on resume failure, never silently create a replacement, and keep Codex/OpenRouter independently usable when Claude is absent or broken; and
- keep normal tests offline with fake auth-status output, fake CLI children, temporary configuration directories, bounded malformed-stream fixtures, and explicit ignored installed-CLI smoke checks; never inspect Keychain or real Claude credentials.

## Completed milestone: Claude reasoning effort

The milestone documented in `docs/plans/claude-reasoning-effort-2026-07-24.md` is complete and supersedes the earlier Claude provider and CLI-compatibility plans only where they said Claude effort selection was unsupported. Its durable contracts are:

- require Claude Code 2.1.218 or newer for the statically supported `--effort` contract;
- keep one provider-wide optional typed effort with exactly `low`, `medium`, `high`, `xhigh`, and `max`; provider default is represented only by absence of the flag;
- snapshot the selected effort when `Intent::SendMessage` creates `Effect::SendClaudeMessage`, then preserve that owned value through auth revalidation, lazy UUID creation, verified pointer commit, recursive requeue, service preparation, and launch without rereading preferences after an await;
- append exactly one `--effort <value>` pair to every configured fresh or resumed Claude invocation and append neither item for provider default, while preserving direct spawn, prompt-on-stdin, empty strict MCP configuration, permission/tool flags, environment scrubbing, and all bounds;
- preserve active/new/resumed/creation-uncertain session semantics when effort changes, keep `ClaudeSessionV1` and turn records unchanged, and never claim historical effort ownership;
- display Vairë's current requested effort, not a verified effective effort; Claude Code may apply a model default or clamp an unsupported request;
- let Claude `/reasoning` report or select `default`, `low`, `medium`, `high`, `xhigh`, or `max`, confirm that a successful selection applies to the next turn, and avoid persistence when the selection is already current;
- keep `/thinking` separate: Claude reasoning output is still not collected, exposed, or inferred; and
- keep normal tests offline and limit the ignored installed-CLI smoke to `--version` plus top-level `--help`, never auth/config/private sessions or a real turn.

## Completed milestone: richer interactive runtime

The richer interactive runtime milestone is complete. Its capabilities are part of the regression baseline:

- create a new thread with `/new`, choose among saved Codex threads through `/resume`, and delete one or all old threads with confirmation while retaining exactly one active thread;
- toggle a right-side Reasoning panel that displays only reasoning summaries or reasoning text actually emitted by app-server, never inferred or hidden chain-of-thought;
- enable Codex command-line and file tools with full local access, without adding an approval UI;
- show the authenticated account identity in the header;
- show an animated pre-response thinking indicator that disappears on the first assistant-text delta; and
- show the remaining context-window percentage in the top-right header when app-server supplies enough token-usage data.

Treat further expansion beyond these capabilities as a new milestone decision with protocol, safety, UX, documentation, and test implications made explicit first.

## Product-rename migration contract

Before opening or creating state, diagnostics, credentials, or provider processes on macOS, Vairë classifies the same-parent legacy `~/Library/Application Support/AgentHarness` and current `~/Library/Application Support/vaire` entries without following symlinks. This is a whole-root, pre-start migration with these durable rules:

- Neither entry means a clean first run; migration itself creates nothing.
- A lone current root succeeds only when it is a real, effective-user-owned directory with exact mode `0700`.
- A lone legacy root with those same properties is moved to `vaire` by an exclusive Darwin same-parent rename that cannot replace a destination. Verify that the destination is the same directory object, then synchronize the parent directory.
- Both entries in any form, or a lone symlink, non-directory, wrong-owner, or non-`0700` entry, fail closed without selecting, deleting, merging, chmodding, or otherwise mutating either root.
- Never enumerate, deserialize, copy, log, or inspect descendants during migration. Treat Codex-owned files, the plaintext OpenRouter key and histories, preferences, diagnostics, and persistent conversation contents as opaque.
- A parent-directory synchronization failure after a verified rename is a committed but durability-unverified move. Report it and never attempt an automatic reverse rename. A later launch may validate the current root normally.
- Leave a migrated legacy diagnostics file untouched; only new writes target `diagnostics/vaire.log`.

The historical Codex conversation cwd is permanent discovery-only metadata. `/resume` queries the current cwd first and an optional distinct historical cwd second, with exact per-query cwd validation, per-query cursor-cycle detection, and shared page/item/text/cursor/result ceilings across the complete operation. Identical thread IDs are deduplicated; the same ID reported under conflicting cwd values is an error. Resource exhaustion fails the complete listing rather than returning partial history. New thread creation, thread resume, turn start, the app-server process, and tool execution use only the current `runtime/conversation` cwd.

## Current user experience

The application opens directly into the chat TUI. On startup it must continue to:

1. Restore non-secret local preferences.
2. Start Codex and independently inspect OpenRouter credentials and Claude native CLI auth state so one provider's failure does not disable the others.
3. Detect each provider's authentication and model-selection state.
4. Automatically resume only the active provider's saved conversation when its scope and local state are valid.
5. Otherwise preserve any saved pointer, block silent replacement, and explain the next action such as `/login`, `/resume`, or `/new`.

Normal text entered in the composer starts a turn in the active thread. Slash commands are the control surface for application actions. The current command vocabulary is:

- `/login` — open the provider/status popup. Choose Codex browser login, masked OpenRouter API-key entry, or Claude Code's native Claude.ai login; `d` starts Codex device login, `c` edits the OpenRouter enabled-model draft, and `r` refreshes the selected provider when supported. Claude login temporarily suspends the TUI while the CLI owns the terminal.
- `/login browser` and `/login device` — direct Codex ChatGPT sign-in shortcuts.
- `/logout` — open the provider popup and settle active work before signing out the selected provider. Claude logout invokes the native CLI and affects the installed Claude Code login outside Vairë too.
- `/model` — open the searchable, scrollable, provider-labelled model picker. Cross-provider selection immediately starts blank; use `/resume` for history.
- `/reasoning` — inspect or select a Codex reasoning level or Claude requested effort (`default`, `low`, `medium`, `high`, `xhigh`, `max`); OpenRouter reasoning is unsupported. Claude changes preserve the session and apply to the next turn.
- `/new` — eagerly create and activate a fresh conversation for the active provider without deleting prior history.
- `/resume` — open the unified provider-labelled Codex/OpenRouter/Claude conversation picker; this is the only explicit cross-provider history restoration path.
- `/thinking` — toggle the right-side Reasoning panel of reasoning summaries or reasoning text emitted by app-server. Distinct from `/reasoning`, which selects the reasoning effort level.
- `/help` — show supported commands and essential keys.
- `/quit` — shut down cleanly and exit.

Unknown slash commands stay local and produce an actionable error directing the user to `/help`; never send them to the model as prompts.

The current key contract is:

- `Enter` sends and `Alt-Enter` inserts a newline.
- `PageUp`, `PageDown`, arrow keys, `Home`, and `End` scroll the transcript.
- In the conversation picker, arrows or `j`/`k` move, `Enter` resumes, `d` requests deletion of the selected inactive history, and `D` requests deletion of all inactive histories. Deletion requires a second `Enter`; `Escape` cancels or closes the picker. The active conversation is protected.
- `Escape` closes local help or errors, cancels a picker action, or interrupts the active turn.
- `Ctrl-C` shuts down cleanly.

The cyan header identifies the active provider and its provider-specific auth/conversation/model/reasoning state, including Claude's requested effort rather than any unverified effective/clamped effort, plus right-aligned `Context N%`; it shows `Context --` when usable context-window data is unavailable. While a turn is active but no nonempty assistant text has arrived, a display-only animated squiggle appears in the conversation pane and disappears on the first text or terminal turn state. The Reasoning panel shows only emitted Codex reasoning; OpenRouter and Claude reasoning fields are not collected. Nonempty OpenRouter or Claude assistant text retained from a failed stream is restored through startup and `/resume` with explicit `FailedIncomplete` domain metadata and the visible label `Agent (incomplete; turn failed):`; it is display-only and never model context.

Do not silently create a replacement conversation when resume fails. Preserve the saved ID and history, enter an explicit blocking resume-failed state, and let the user choose `/resume` or `/new`. Codex account scope remains the normalized ChatGPT email; if app-server does not expose it, do not auto-resume or manually resume a saved Codex thread. The supported Claude auth status has no stable account identifier, so never auto-resume a saved Claude session at startup or after authentication; require explicit `/resume` or `/new`, and revalidate subscription auth before every turn. OpenRouter histories and Vairë-owned Claude registrations/display histories are validated local owner-only files. Claude registrations are same-user local records rather than provider-account-scoped records, and Claude-owned session files remain opaque. On a first run with no saved conversation, create the single working conversation when it is first needed.

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
- Treat event receipt as the cancellation boundary: once an app-server event has been received, finish its reduction and any required follow-up RPC before handling a competing UI intent. Never race a combined receive-and-process future against input, because cancellation could discard an already-consumed event.
- Preserve the established resource ceilings: 1 MiB per JSON-RPC frame, 128 pending requests, 256 pagination pages, 1,024 models or 50 threads per page, 16,384 retained paginated items / 16 MiB of estimated retained text, and 16 KiB cursors. Reject violations visibly and keep a usable connection alive only when correlation remains unambiguous.
- Scope streamed deltas and terminal events to their thread, turn, and item. Reconcile final snapshots without duplicating streamed text.
- Treat process exit, malformed frames or required payloads, stale thread IDs, timeouts, and version skew as visible failures. Tolerate genuinely unknown notification methods where safe; if diagnosing them, record only sanitized method metadata. Preserve settled state where possible; when the app-server connection becomes unusable, tell the user to restart because the current product has no in-app reconnect.
- Enforce the tested minimum Codex CLI version with an actionable upgrade message. Regenerate the installed schema and update protocol fixtures, safety policy, and compatibility notes whenever the tested baseline changes.
- Derive models and reasoning choices from `model/list` or its current schema equivalent. Validate the reasoning choice when the model changes. If a saved choice is unavailable, use the server default and tell the user.
- Development code must never intentionally read, copy, print, persist, or commit tokens from any Codex home, including Vairë's dedicated one. Let Codex own credential storage and refresh. Unrestricted model-run commands are not OS-isolated from same-user credential files; document that risk rather than claiming containment.
- Never log authorization headers, access tokens, cookies, full auth payloads, prompts, or replies by default. Redact sensitive protocol fields from diagnostics.
- Open login URLs without shell interpolation and accept only the expected safe URL schemes.
- Scope saved thread state to the authenticated account when the protocol exposes an account identifier. Never resume a thread across an account switch.
- Keep repository development instructions out of runtime conversations. The dedicated Codex home and non-project startup directory prevent automatic inheritance of this repository's `AGENTS.md`; they do not prevent a `danger-full-access` command from reading arbitrary same-user paths. Any future runtime prompt must use a separate, explicit product path.

### Full-access runtime boundary

The current product intentionally enables Codex and Claude command-line and file tools without tool cards or an approval UI. This is an unrestricted same-user execution boundary, not a sandbox.

- Apply Codex `sandbox_mode="danger-full-access"` / `dangerFullAccess` and `approval_policy="never"` at process, thread start/resume, and turn boundaries. Apply Claude's documented dangerous permission bypass on every real turn. Supported command/file operations execute directly without confirmation.
- The app-server inherits the launcher environment except inherited `CODEX_*` variables are removed and Vairë supplies its dedicated `CODEX_HOME`. Claude auth and turn children remove inherited `ANTHROPIC_*` and `CLAUDE_*` variables, then receive only non-secret runtime configuration such as Vairë's dedicated `CLAUDE_CONFIG_DIR`; authentication stays internal to Claude Code. Tool shells otherwise retain same-user ambient authority.
- Codex's default name-based filtering of variables containing `KEY`, `SECRET`, or `TOKEN` is incomplete and is not a security boundary. Values such as `DATABASE_URL` and `SSH_AUTH_SOCK` may remain available.
- Full-access commands may use SSH agents, macOS Keychain, credential and configuration files, authenticated CLIs, and the network to act locally or remotely.
- Use dedicated Vairë Codex and Claude homes plus persistent non-project `runtime/conversation` (Codex) and `runtime/claude-conversation` (Claude) starting directories to avoid automatic project inheritance. Commands can leave that directory or use absolute paths, its files survive restarts, and the dedicated Codex home—including Codex-owned authentication state—remains reachable to same-user full-access tools. These are organizational boundaries only.
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
- **Claude process/protocol/service:** executable/version/native-auth checks and foreground auth actions, direct bounded stream-json children, Vairë registration/display store, UUID start/resume, turn reduction, process-group interruption, and shutdown; private Claude storage stays opaque.
- **Persistence:** versioned, non-secret preferences such as account scope, provider conversation ID, model, and reasoning level.
- **Platform integration:** browser launching, application-data directories, signals, and other macOS and Linux seams.

The UI consumes domain events, never raw protocol JSON. Prefer explicit message passing between the async backend and the render loop. Never block Ratatui on login, model responses, disk I/O, or a running turn.

Use traits or equivalent injection seams around transport, persistence, browser opening, terminal operations, process spawning, and configurable timeouts so core state transitions can be tested without a terminal or real Codex account. Do not reproduce a large coordinator from the reference application; preserve the useful separation with the smallest design that serves current requirements.

## Persistence and lifecycle

- Store only the minimum local state needed to restore the experience. Codex and Claude remain the sources of truth for their model context; Vairë stores only bounded Claude registration/display history.
- Keep interactive and local-state memory bounded: 128 KiB composer drafts, 256 KiB reducer-level messages, 1 MiB / 2,048-entry transcript retention with explicit newline and display-width ceilings, 32 KiB / 128-entry emitted reasoning retention, 1 MiB preferences, and a rotating 1 MiB diagnostics file. Treat these values as tested compatibility contracts; change them deliberately with tests and user-facing documentation.
- Put application state in the appropriate macOS application-support directory, not in the repository or current working directory.
- Use a small versioned serialization format and atomic replace-on-write behavior.
- Treat a missing or invalid state file as a clean first run, not a crash.
- Save a thread ID only after successful creation or resumption. Do not discard a previously saved ID because a transient resume failed.
- Restore Codex transcript history from app-server rather than duplicating it. Restore OpenRouter and Claude display history only from their bounded validated Vairë-owned stores; never use failed partials as model context.
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
- Integration behavior against fake app-server and Claude CLI children for initialization, auth events, model metadata, new and resumed conversations, streaming, malformed JSON, unknown events/requests, EOF, timeout, interruption, process-group cleanup, and provider-independent failure.
- Thread-picker listing, switching, confirmed single/bulk deletion, active-thread protection, partial failures, and stale result correlation.
- Emitted-reasoning scoping, full-access policy and inherited-environment behavior, account/header sanitization, activity-animation lifecycle, context-usage scoping and arithmetic, and narrow/normal cross-feature Ratatui rendering.
- Tool-heavy event floods and unexpected server requests, proving meaningful conversation events remain deliverable and every unimplemented request still fails closed.
- Rendering above `u16::MAX` aggregate logical rows, proving transcript and reasoning pre-windowing reaches the true requested viewport without relying on a saturated Ratatui scroll offset.
- If runtime-agent instructions are introduced, test new-thread and resumed-thread behavior, instruction precedence, and the invariant that this development `AGENTS.md` is never injected into a user conversation.

Keep any future real authenticated Codex or Claude smoke test manual or ignored. Default tests and CI must not require ChatGPT/Claude login, API keys, user configuration, or network access.

The required handoff checks are:

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets
```

Run the smallest relevant checks while iterating, then all three before handing off a completed change. Add or update tests with behavior changes. Do not weaken lint settings or delete a failing test merely to make a check pass.

Ratatui 0.29's `unstable-rendered-line-info` feature is intentionally enabled only so the TUI can measure Ratatui's own wrapped-line layout before pre-windowing a viewport. It does not require nightly Rust or add platform coupling. Keep its use isolated to rendering calculations, retain the Ratatui 0.29 pin until an explicit compatibility review, and exercise wrapping/windowing tests before any Ratatui upgrade or feature change.

## RepoPrompt-assisted development

RepoPrompt is both a development tool for Vairë and, separately, the home of the optional source reference below. Use it when available to keep discovery, context selection, planning, and review focused.

- Prefer RepoPrompt MCP tools when they are available. When only the CLI is available, use `rpce-cli`; start with `rpce-cli -e 'windows'`, then target the window whose workspace contains Vairë.
- Window IDs are runtime state. Never hard-code one in scripts or durable instructions.
- Prefer RepoPrompt `tree`, `search`, `read`, `structure`, `edit`, and read-only `git` operations for Vairë repository work when the relevant interface is available.
- Use selection plus Context Builder or chat and oracle for architecture work that benefits from curated cross-file context. Use read-only explore agents for narrow side investigations.
- Keep selections task-specific. Do not load the whole reference project when a transport file, protocol model, or focused test is enough.
- When both roots are loaded, qualify every mutable Vairë path with `vaire/`. Qualify reference reads with `repoprompt-ce/`.
- RepoPrompt is a developer aid, not an application dependency. Building, testing, and running Vairë must not require RepoPrompt or `rpce-cli`.

## Optional external reference: strictly read-only

`/Users/danhancu/Developer/RepoPrompt/repoprompt-ce` is optional design guidance. It may be loaded as a second RepoPrompt workspace root, but it is outside the Vairë repository and is immutable during Vairë work.

Useful read-only areas include:

- `Sources/RepoPrompt/Infrastructure/AI/Providers/Codex/AppServer`
- `Sources/RepoPrompt/Features/AgentMode`

Consult it for patterns such as transport and session separation, model discovery, event reduction, auth recovery, and diagnostics. Implement needed behavior independently in Rust.

### Hard boundary

Never modify any path under `/Users/danhancu/Developer/RepoPrompt/repoprompt-ce` while working on Vairë.

This prohibition includes:

- Source, tests, fixtures, documentation, configuration, and repository metadata.
- Formatting, generated files, schemas, logs, caches, build products, and test artifacts.
- Staging, committing, branching, rebasing, cleaning, or any other Git mutation.
- Running commands in that repository that may write, format, build, test, generate, install, or clean.

Only read-only operations are allowed there: tree, search, read, structure, and read-only Git inspection. Before every RepoPrompt edit or file operation, verify that the target is root-qualified under `vaire/`. If the target is ambiguous or points at `repoprompt-ce/`, stop without writing.

Do not vendor, symlink, copy wholesale, add a path dependency to, or assume the reference exists in CI or on another developer machine. If a small licensed idea or excerpt is intentionally reused, verify and preserve all attribution and license obligations. Capture borrowed behavior in Vairë tests or documentation so contributors without the reference path are unaffected.

Vairë requirements, official Codex documentation, and the installed Codex schema always take precedence over the reference implementation.

## Working rules

- Keep changes scoped to explicitly approved work; avoid speculative provider systems, plugin systems, tool frameworks, or multi-thread orchestration.
- Prefer small, typed domain models over unstructured JSON beyond the protocol boundary.
- Preserve unknown protocol fields and tolerate unknown notifications where practical, but fail clearly when a required field is missing.
- Add dependencies intentionally. Favor well-maintained crates and avoid overlapping libraries for the same concern.
- Keep user-facing failures actionable and low-level details in sanitized diagnostics.
- Treat shipped behavior changes as incomplete until relevant regression tests and user-facing documentation are updated.
- Update this file when established commands, architecture, supported platforms, runtime-instruction behavior, or current product boundaries change.
