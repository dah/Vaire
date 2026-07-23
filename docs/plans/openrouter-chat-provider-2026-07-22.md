# Approved milestone: OpenRouter chat provider

**Date:** 2026-07-22  
**Status:** Completed and shipped  
**Scope owner:** AgentHarness  
**Implementation rule:** This document and the matching planned section in AGENTS.md approve the milestone. They do not change runtime behavior or mark the milestone complete.

## 1. Goal

Retain the complete shipped Codex app-server experience and add OpenRouter as a chat-only second provider. The product continues to expose exactly one active conversation and one active turn. Provider identity becomes explicit throughout application, backend, persistence, and UI boundaries so Codex and OpenRouter models, conversations, turns, histories, credentials, and events cannot collide or leak across providers.

OpenRouter uses:

- GET /api/v1/key to validate an API key without a billable completion;
- authenticated GET /api/v1/models/user to fetch the user-visible model catalog; and
- POST /api/v1/chat/completions with server-sent events for text chat.

The production base URL is fixed to https://openrouter.ai. Only offline tests may inject a loopback base URL.

## 2. Current state and regression contract

The current product has one authoritative AppState, a pure Intent → Effect → DomainEvent reducer, a BackendCoordinator backed by one Codex SessionService, and a cancellation-safe runtime scheduler. Codex owns authentication, model discovery, threads, history, and streamed events through one long-lived app-server process. PreferencesV1 stores non-secret Codex selection and account-scoped thread state. The TUI currently has a thread picker plus local overlays, and the watch-published AppState is cloned.

Implementation must preserve:

- every shipped Codex login, model/reasoning, thread, streaming, emitted-reasoning, context, command/file-tool, full-access, failure, resource-limit, and shutdown contract;
- the current Codex JSON-RPC transport, protocol, session, safety, pagination, and fail-closed request handling;
- exact account-scoped Codex resume rules, thread source/cwd filtering, and active-thread protection;
- the already-received-event processing boundary before competing UI intent;
- transcript sanitization and bounded retention, Ratatui pre-windowing, terminal restoration, and offline default tests;
- the dedicated Codex home and full-access risk documentation; and
- RepoPrompt CE as optional, strictly read-only development guidance with no runtime or build dependency.

Codex app-server startup becomes provider-scoped rather than globally fatal so a configured OpenRouter session can remain usable. AgentHarness still attempts Codex resolution, version checking, spawn, and initialization on startup and keeps the long-lived process running and its events consumed while OpenRouter is active.

## 3. Scope

### In scope

- Codex and OpenRouter as the complete closed provider set.
- Independent provider authentication and availability state.
- A keyboard-driven /login modal listing both providers and their independent status.
- Codex browser/device-code login unchanged.
- Masked OpenRouter API-key entry, validation, transactional owner-only file replacement, and logout deletion through an injected CredentialStore.
- A fetched, searchable OpenRouter catalog and a persisted user-enabled subset.
- A searchable, scrollable, provider-labelled /model picker.
- Provider-tagged model, conversation, turn, transcript, reasoning, and context ownership.
- OpenRouter text-only streaming chat and locally owned durable conversations.
- Unified provider-labelled /resume and deletion behavior.
- Independent provider failure isolation and clean coordinated shutdown.
- Offline fake app-server, fake credential store, temporary file-store, and loopback HTTP/SSE tests.
- A real PreferencesV1 → PreferencesV2 migration.

### Out of scope

- Providers other than Codex and OpenRouter or any provider plugin/factory framework.
- Custom OpenRouter endpoints, proxy/custom-header UI, organization settings, or automatic endpoint fallback.
- OpenRouter tools, tool calls, MCP, web search, images, audio, or multimodal input.
- OpenRouter reasoning-effort controls or display/storage of provider reasoning fields.
- Cross-provider history transfer, automatic continuity, branching, multiple active conversations, parallel turns, or orchestration.
- Remote OpenRouter threads, cross-device local-history sync, spend/quota UI, or automatic model enablement.
- Encryption or secure erasure of the API-key file or local OpenRouter histories.
- macOS Keychain in this milestone.
- Windows support or a RepoPrompt dependency.

## 4. Locked product decisions

| Operation | Decision |
|---|---|
| Startup | Restore PreferencesV2 and local OpenRouter metadata first. Start Codex and inspect the OpenRouter credential independently. Restore only the active provider's valid automatic-resume pointer. Never silently switch provider or create a replacement conversation after resume failure. |
| Codex unavailable | Show a Codex-scoped actionable failure. OpenRouter remains usable. Core path/persistence failure remains globally fatal. |
| OpenRouter unavailable | Show a sanitized OpenRouter-scoped credential/network/catalog state. Codex remains usable. |
| /login | Open one modal with Codex and OpenRouter rows and independent statuses. Enter on signed-out Codex starts browser login; d starts device login. Enter on OpenRouter enters/replaces a key. c manages the catalog and r revalidates/refreshes when configured. |
| /login browser, /login device | Retain the existing direct Codex shortcuts. |
| OpenRouter key replacement | Transfer the candidate outside AppState, validate with GET /api/v1/key, atomically replace the file only after valid 2xx JSON, then fetch the catalog. Invalid credentials or transient failure preserve the prior file. |
| /logout | Open the provider modal in logout mode. Codex retains existing semantics. OpenRouter first interrupts and settles an active OpenRouter turn, then deletes the credential file. Failure to delete leaves auth configured and reports that the credential may still exist. Histories, catalog cache, enabled subset, and selected model remain. |
| OpenRouter catalog | Fetch authenticated GET /api/v1/models/user. Search edits a draft enabled subset; Space toggles, Enter commits, Escape discards. An empty enabled subset is valid and blocks OpenRouter send. Missing refreshed IDs remain persisted but unavailable so they can reappear later. |
| /model | Open one provider-labelled searchable/scrollable picker. Codex rows come from the live catalog; OpenRouter rows are enabled IDs present in live/cached catalog. Remove the ambiguous /model <id> shortcut. |
| Same-provider model change | Keep the active conversation and transcript. Revalidate Codex reasoning or keep OpenRouter reasoning unsupported. Clear context because its model owner changed. Do not alter automatic-resume identity. |
| Cross-provider model change | Reject while any turn is active. Preserve source history in its provider store, select the destination model/provider, set a blank destination conversation, clear transcript/reasoning/activity/context, clear both automatic-resume pointers, persist, and show that /resume restores old history. No confirmation and no automatic history load. |
| First send after a cross-provider model change | Lazily create a brand-new destination conversation and set only its automatic-resume pointer. Codex uses thread/start; OpenRouter persists a new local conversation before HTTP. |
| Exit before first send after provider switch | Restart into the selected provider/model with a blank conversation and no resume attempt. |
| /new | Eagerly create and activate a new conversation only for the active provider; preserve all inactive histories. Set that provider's automatic-resume pointer and clear the other pointer. |
| /resume | List registered/account-safe Codex threads and validated local OpenRouter conversations in provider sections. This is the only cross-provider operation that restores history. Successful selection activates/restores it, sets its pointer, and clears the other; failure preserves current provider, transcript, and pointers. |
| Delete | Preserve confirmation, partial-failure reporting, and active-conversation protection. Codex deletion remains remote; OpenRouter deletion removes validated local files/index entries. |
| /reasoning | Preserve Codex selection. For OpenRouter, state that reasoning effort is unsupported in this milestone. Never carry a Codex effort into OpenRouter. |
| /thinking | Preserve visibility preference. Codex shows only emitted summaries/text. OpenRouter shows that reasoning is not collected; reasoning-like SSE fields are ignored. Provider switches clear content but not panel visibility. |
| Context meter | Codex behavior is unchanged. OpenRouter computes remaining context only when usage and a validated positive catalog context length exist for the active tagged turn/model; otherwise Context --. |
| Escape during a turn | Preserve Codex interrupt. For OpenRouter, cancel the HTTP task, emit exactly one interrupted terminal event, persist the user record without partial assistant text, and wait for terminal reduction before another lifecycle operation. |
| OpenRouter stream failure | Keep bounded partial assistant text only in the current in-memory transcript, persist no partial assistant response, mark the local turn failed, and never automatically retry POST. |
| Offline/transient GET failure | Retain valid cached catalog/history and expose Unverified/refresh-failed status. GET-only retry is bounded. No hidden provider switch. |
| 401/403 from catalog/chat | Mark the stored credential Invalid but do not delete it or histories. Block sends until successful replacement/revalidation. |
| Shutdown | Stop input; settle/interrupt the active turn; cancel and join OpenRouter tasks; persist local terminal outcome/preferences; stop and reap Codex; then restore the terminal. Temporary secret values are dropped/zeroized. |

The model-picker footer and provider-switch notice must say: **Switching provider starts a new conversation; use /resume for history.**

## 5. Architecture and data ownership

This is a bounded two-provider refactor, not a general provider SDK.

### Closed provider identities

Add src/provider.rs with:

- ProviderId::{Codex, OpenRouter};
- ModelKey { provider, id };
- ConversationRef::{Codex { thread_id }, OpenRouter { conversation_id }};
- TurnRef::{Codex { thread_id, turn_id }, OpenRouter { conversation_id, turn_id }};
- validated OpenRouterConversationId serialized as or_<32 lowercase UUID hex>;
- validated OpenRouterTurnId serialized as ort_<32 lowercase UUID hex>; and
- provider-aware ModelChoice and reasoning capability.

Raw user/server strings must never be joined into a local path. Every reducer/backend operation that could address either provider must accept tagged identities.

### Authoritative application state

AppState gains ProviderStates, active_provider, one ConversationState, one tagged TurnState, one PopupState, provider-owned model/auth/catalog state, and tagged ContextState. TranscriptEntry and ThinkingEntry use TurnRef. No secret, authorization value, key metadata, key length, or key fragment may enter AppState, DomainEvent, Effect, notices, transcript, watch snapshots, preferences, diagnostics, environment, or process arguments.

Replace thread-specific application nouns with conversation-specific types where both providers participate. Keep Codex-specific account and protocol concepts inside the Codex state/service.

### Runtime/backend composition

BackendCoordinator owns an optional/live Codex runtime plus an OpenRouterService. BackendRuntimeEvent is Codex(SessionEvent) or OpenRouter(OpenRouterServiceEvent). Receiving an event and processing its reduction plus required persistence remains a non-cancellable unit before another UI intent.

OpenRouterService owns:

- an immutable bounded reqwest Client;
- Arc<dyn CredentialStore>;
- Arc<dyn OpenRouterConversationStore>;
- a bounded event channel;
- at most one validation/catalog control task;
- at most one chat task; and
- cancellation tokens and join handles.

Codex continues running and its notifications are consumed while OpenRouter is active. OpenRouter HTTP never enters codex::transport.

### Secret command path

Normal UI actions continue through Intent. Secret submission uses a separate non-clonable RuntimeCommand::Secret carrying SecretValue. If bounded channel admission fails, ownership returns to UiState without copying. Reducer-visible events contain only an operation ID and sanitized outcome.

SecretValue is zeroizing, non-Clone, non-Serialize, and redacted in Debug. The masked editor displays one fixed nonempty marker rather than key length. Paste goes directly to the secret buffer, never the composer. Candidate input trims outer paste whitespace and then requires 1–8192 printable ASCII bytes with no embedded whitespace/control characters. Escape and Ctrl-U zeroize.

## 6. Credential storage and technical debt

### Injected port

Add:

- CredentialAccount::OpenRouterApiKey;
- CredentialStore: Send + Sync with load, replace, and delete;
- FileCredentialStore for production; and
- FakeCredentialStore for deterministic tests.

The synchronous trait is called via spawn_blocking. Sanitized CredentialStoreError exposes only a closed CredentialFailureCategory such as Read, Write, Delete, Permissions, or Corrupt.

### Production layout

~~~text
<Application Support>/AgentHarness/
  runtime/
    openrouter-home/       mode 0700
      api-key              regular file, current user, exact mode 0600
~~~

The file contains normalized API-key bytes only: no JSON and no trailing newline. Maximum stored size is 8 KiB.

Reads fail closed on symlinks, non-regular files, wrong ownership, unexpected permission bits, empty/oversize content, invalid UTF-8, whitespace, or controls. Writes use a same-directory create_new mode-0600 temporary file, file sync, atomic rename, and parent-directory sync. Initialization removes only recognized orphan credential temporary names without reading/logging their contents. Delete unlinks api-key and syncs the parent directory.

This is plaintext organizational isolation only. It does not protect against malware, other same-user processes, backups, snapshots, disk recovery, or the shipped full-access Codex runtime. Deletion is not secure erasure. HTTP libraries may copy authorization bytes internally, so zeroization minimizes exposure but cannot guarantee removal of every process-memory copy.

### Mandatory future technical debt

A later explicitly approved milestone must migrate OpenRouter credentials to macOS Keychain through the same CredentialStore port. Migration must save and verify the Keychain item before deleting the plaintext api-key file, preserve the file on any migration failure, and never make reducer/HTTP consumers Keychain-specific. This milestone adds no Keychain dependency or fallback.

## 7. OpenRouter HTTP/SSE contract

### Requests

Production headers are Authorization: Bearer <key>, User-Agent: AgentHarness/<version>, X-Title: AgentHarness, appropriate Accept, and Content-Type for POST. Headers and arbitrary remote bodies are never logged.

GET /api/v1/key requires 2xx plus bounded valid JSON with documented top-level data. Returned account/key/usage metadata does not enter AppState.

GET /api/v1/models/user requires authentication and a top-level data array. Each retained model requires a nonempty ID; name and positive context length are optional. Unknown fields are ignored.

POST /api/v1/chat/completions sends only:

- selected model ID;
- text-only user and completed-assistant messages from the active OpenRouter conversation;
- stream: true; and
- stream_options.include_usage: true.

It sends no system/developer instructions, Codex history, tools, reasoning controls, temperature, max tokens, metadata, or partial failed/interrupted assistant output.

### Ceilings

- Connect timeout: 5 seconds.
- Key/catalog attempt timeout: 15 seconds.
- Chat response-header timeout: 30 seconds.
- SSE idle timeout: 60 seconds.
- Maximum chat duration: 6 hours.
- JSON error body read: 16 KiB.
- Catalog body/cache file: 4 MiB.
- Retained models: 10,000.
- Model ID: 512 bytes; display name: 1 KiB; retained catalog text: 8 MiB.
- Outbound chat JSON: 1 MiB.
- SSE event/frame buffer: 256 KiB.
- Completed assistant text per turn: 1 MiB.
- OpenRouter provider event queue: 64.
- Catalog search text: 256 UTF-8 bytes.

Duplicate catalog IDs keep the first. A structural/limit failure rejects refresh and preserves the previous cache. Do not invent pagination or dynamically fall back to /api/v1/models.

GET /api/v1/key and GET /api/v1/models/user have at most two total attempts for connect/timeout, 408, 429, 502, 503, or 504. Initial delay is 250 ms; Retry-After is honored only up to 2 seconds. POST has no automatic retry, even before the first delta. Cancellation never retries.

### SSE rules

Use a bounded incremental decoder without automatic reconnect:

- consume only data events;
- [DONE] completes the turn;
- non-null finish_reason marks a terminal choice, with continued reading for final usage;
- EOF after finish_reason is accepted; EOF without [DONE] or finish_reason fails;
- malformed JSON, oversize frames, content after terminal choice, or conflicting required identity fails;
- ignore provider reasoning, reasoning_details, tool calls, and unknown delta fields;
- accept usage only for the active tagged turn/model; and
- reconcile final state without duplicate streamed text.

## 8. OpenRouter local history

Store non-secret OpenRouter data separately from runtime/openrouter-home:

~~~text
<Application Support>/AgentHarness/openrouter/
  catalog.json
  conversations/
    index.json
    or_<uuid>.json
~~~

Directories are 0700 and files 0600, written using create_new temporary files, sync, atomic rename, and directory sync. Histories are owner-only plaintext, not encrypted, and remain reachable by same-user/full-access Codex commands.

OpenRouterConversationV1 contains version, ID, created/updated timestamps, sanitized title, and turn records. Each turn stores ID, model ID, user text, optional assistant text, and InProgress/Completed/Interrupted/Failed outcome.

Rules:

- persist InProgress plus user text before HTTP;
- persist completed assistant only after terminal success;
- on interrupt/failure persist the terminal outcome with no assistant;
- repair persisted InProgress to Interrupted at startup;
- restore user messages and completed assistant messages only;
- build request history from that same canonical store only;
- write conversation before index update and rebuild index by validated filename/contents on startup;
- retain corrupt files without overwriting; a saved corrupt active ID produces ResumeFailed; and
- allow confirmed deletion by a validated ID.

Ceilings:

- 50 conversations;
- 1 MiB per serialized conversation;
- 768 KiB canonical text per conversation;
- 1,024 turns per conversation;
- existing 128 KiB user/composer limit;
- 16 MiB aggregate conversation files;
- 256 KiB index.

If a record cannot fit, preserve the last valid file, fail visibly, and direct the user to /new or delete inactive conversations. Never silently truncate/evict canonical request history. Reducer transcript presentation may retain its existing independent bounds.

## 9. PreferencesV2 and migration

Use a version-dispatched loader.

~~~text
PreferencesV2
- version: 2
- active_provider: ProviderId
- codex: CodexPreferencesV2
- openrouter: OpenRouterPreferencesV2

CodexPreferencesV2
- account_scope
- auto_resume_thread_id
- model_id
- reasoning_effort
- thread_account_scopes

OpenRouterPreferencesV2
- auto_resume_conversation_id
- selected_model_id
- enabled_model_ids
~~~

Invariant: only the active provider may have an automatic-resume ID.

Migration maps every PreferencesV1 field without loss and maps v1 thread_id exactly to codex.auto_resume_thread_id. Initialize OpenRouter fields empty and active_provider as Codex. Persist migrated v2 atomically after successful local startup; if save fails, continue with the in-memory migration, warn, and retry at shutdown. Unknown versions remain non-overwritable.

Cross-provider /model clears both resume pointers. Successful first send, /new, or /resume sets the active provider pointer and clears the other. A same-provider model change preserves the pointer. A failed /resume preserves both current state and pointers.

Preferences never contain credentials, auth validity, catalog bodies, transcript, context, popup state, or pending operations. A v1 binary refuses to overwrite v2, so downgrade is non-destructive but does not restore settings until re-upgrade. Credential/history files are independent.

## 10. Popup and key contracts

Replace the application thread picker with one PopupState: Auth, Model, OpenRouterCatalog, or Conversation. UiState retains help/local errors and owns the secret editor. Input precedence is Ctrl-C, secret entry, help/error overlay, application popup, then composer/scroll/interrupt. Opening a popup does not discard the composer draft.

Common list keys: arrows or j/k, PageUp/PageDown, Home/End, Enter, Escape. Model/catalog search accepts printable input and Backspace. Catalog Space toggles. Conversation d requests selected deletion and D requests all inactive deletion, followed by Enter confirmation; Escape cancels.

The /model picker remains provider-labelled at narrow supported widths. Authentication does not hide configured models/history, but send remains auth-gated. If catalog changes disable the selected OpenRouter model, choose the first enabled available model sorted by display name then ID, or clear selection and block send. Keep the active conversation and clear context.

## 11. File-by-file implementation plan

### Phase 1 — approval documents

- docs/plans/openrouter-chat-provider-2026-07-22.md: this source-of-truth plan.
- AGENTS.md: planned/not-shipped approval, plaintext credential boundary, Keychain migration debt, fresh provider-switch rule.
- No Rust behavior changes; phase completion does not complete the milestone.

### Phase 2 — provider identities and preferences

- src/provider.rs: closed provider/model/conversation/turn identities and validation.
- src/lib.rs and src/app.rs: module exports.
- src/app/domain.rs, state.rs, turn.rs, transcript.rs, thinking.rs: provider-tagged state.
- src/app/actions.rs and reducer/{intent,event}.rs: tagged operations with Codex-only behavior initially.
- src/persistence.rs and src/persistence/tests.rs: PreferencesV2, exact v1 migration, invariant/unknown-version/no-secret tests.
- src/persistence/atomic.rs: extract reusable owner-only atomic replacement while preserving symlink/temp-file tests.
- src/platform.rs: add openrouter_dir, openrouter_home_dir = runtime_dir.join("openrouter-home"), and openrouter_credential_file = openrouter_home_dir.join("api-key").
- Preserve all focused reducer and persistence tests at a compiling Codex-only boundary.

### Phase 3 — generalized popups

- src/app/thread_events.rs → src/app/conversation_events.rs.
- src/app/thread_picker.rs → src/app/conversation_picker.rs.
- src/app/popup.rs: single popup state and bounded search/navigation.
- src/command.rs: modal /login, /logout, /model, /resume; retain explicit Codex login shortcuts; remove /model <id>.
- src/tui/thread_picker.rs → src/tui/popup/conversation.rs.
- Add src/tui/popup.rs plus popup/{auth,model,catalog}.rs.
- Modify src/tui/{state,layout,header_message,conversation,display}.rs and main.rs for provider labels and modal routing.
- Update app/TUI parser, keymap, narrow-layout, activity, and composer-preservation tests.

### Phase 4 — transient secrets and credential store

- src/credentials.rs, src/credentials/types.rs: SecretValue, CredentialAccount, CredentialStore, redacted errors.
- src/credentials/file.rs: FileCredentialStore validation, atomic replacement, orphan cleanup, deletion.
- src/credentials/tests.rs: regular-file/ownership/mode/symlink/size/content rejection; atomic preservation; deletion; temp cleanup; redaction.
- src/runtime/types.rs and scheduler.rs: ownership-preserving RuntimeCommand secret channel.
- src/tui/state.rs and main.rs: masked editor, paste isolation, zeroization, queue failure ownership return.
- src/runtime/build.rs: inject production FileCredentialStore; test constructors inject FakeCredentialStore.
- Tests assert a recognizable fake key is absent from every clonable/serialized/rendered/logged/process surface.

### Phase 5 — bounded OpenRouter layers

- Cargo.toml/Cargo.lock: add reqwest 0.12 without default features with json, stream, rustls-tls; bytes 1; zeroize 1; uuid 1 with v4/serde; required Tokio net and tokio-util rt features. Do not add security-framework, provider/OpenAI SDK, event-source reconnect library, async-trait, or RepoPrompt.
- src/openrouter.rs: narrow module exports.
- src/openrouter/types.rs: catalog, operation IDs, events, records, sanitized failures.
- src/openrouter/protocol.rs: tolerant /key, /models/user, chat/SSE/usage structures.
- src/openrouter/client.rs: fixed production URL, injected loopback URL, headers, redaction, timeouts/retries/bounds.
- src/openrouter/sse.rs: incremental bounded decoder and terminal rules.
- src/openrouter/store.rs: cache/conversation schemas, atomic storage, limits, recovery, injected store port.
- src/openrouter/service.rs: task ownership, per-operation credential load/drop, cancellation, event queue, persistence ordering.
- src/openrouter/tests/** and tests/support/openrouter.rs: focused unit and loopback fake tests for exact paths/headers, chunking, limits, retries, cancellation, and request counts.

### Phase 6 — independent runtimes and auth/catalog UI

- src/backend/types.rs: optional Codex state, OpenRouterService, BackendRuntimeEvent.
- src/backend/lifecycle.rs: local restore, independent provider startup/event selection/shutdown.
- src/backend/effects.rs: provider auth/catalog/model/conversation/send/interrupt/persist dispatch and CredentialStore operations.
- src/backend/protocol_events.rs → src/backend/codex_events.rs, preserving Codex mapping with tags.
- src/backend/openrouter_events.rs: bounded service-event mapping and required-before-publication persistence.
- src/backend/thread_ops.rs → src/backend/conversation_ops.rs: unified list/resume/delete with partial results.
- src/backend/helpers.rs: provider-scoped failures.
- src/runtime/build.rs, scheduler.rs, types.rs: recoverable Codex construction, both event streams, secret commands, cancellation boundary.
- Implement /login independent status, validation-before-replacement, catalog refresh/editor, and offline cached states.
- Add reducer/backend/runtime/integration tests before proceeding.

### Phase 7 — switching and OpenRouter turns/history

- reducer intent/state changes: synchronous hard blank cross-provider /model boundary; same-provider retention; no pending activation/history load.
- backend effects: no destination resume during model switching.
- OpenRouter service: lazy conversation creation before POST, canonical request construction, SSE reduction, usage/context, invalid credential, interruption, terminal durability.
- conversation picker/backend: /new, /resume-only history restoration, local/remote deletion, logout sequencing.
- src/app/tests/{selection_new_thread,resume,context_activity,account_auth}.rs: switching, pointer, scoping, status.
- tests/openrouter_vertical_slice/{startup_auth,catalog_model,turn_lifecycle,history,support}.rs.
- tests/provider_interop/{switching,failure_isolation,shutdown}.rs.
- tests/fixtures/openrouter/: bounded fake key/models/chat/usage/malformed/EOF fixtures with no real data.
- Captured requests prove no Codex history and no older OpenRouter history after model-based provider switch.

### Phase 8 — completion documentation and full regression

Only after implementation and all acceptance criteria pass:

- README.md: document shipped dual-provider workflows, plaintext credential/history risks, local resume, and retained Codex full-access warning.
- AGENTS.md: move planned contracts into shipped baseline/current UX; retain Keychain migration debt.
- LOC-Doc.md: regenerate actual line counts.
- Preserve all existing Codex fake-app-server, transport recovery, denial, full-access, thread, reducer, and Ratatui tests.

## 12. Offline acceptance matrix

| Area | Required acceptance |
|---|---|
| Codex regression | Existing startup/auth/model/reasoning/thread/resume/stream/tool/safety/resource/shutdown tests remain green with only necessary provider tags/modal entry updates. |
| Credentials | Production temp-dir tests prove 0700/0600, current-owner regular-file checks, symlink/non-file/wrong-mode/corrupt/oversize rejection, old-value preservation, cleanup, and no secure-erasure claim. Default tests never inspect the developer's real path. |
| Key validation | Fake server receives exact GET /api/v1/key with fake authorization; valid/invalid/transient/replacement/storage-failure outcomes are transactional and sanitized. |
| Catalog | Exact authenticated GET /api/v1/models/user; search/scroll/toggle/commit/discard, empty subset, stale IDs, duplicate/oversize/malformed response, cache retention, and exact no-fallback behavior. |
| SSE | Split chunks, multi-line data, [DONE], finish reason then EOF, usage, malformed JSON, oversize frame, idle timeout, cancel, late event, EOF failure, ignored reasoning/tool fields, and no POST retry. |
| History | Atomic create/update/index, startup InProgress repair, index rebuild, corrupt-file preservation, size/count ceilings, active protection, and canonical-history request construction. |
| Provider switching | Cross-provider /model immediately blanks transcript/reasoning/context, clears pointers, creates no conversation until send, resumes blank after pre-send exit, creates a fresh tagged ID, and retains old histories in /resume. |
| Same-provider selection | Keeps conversation/transcript/pointer, updates model/reasoning rules, and clears context. |
| /resume | Only successful /resume can switch provider and restore history. Failure preserves provider/transcript/pointers; Codex account scope remains enforced. |
| Failure isolation | Codex unavailable does not kill OpenRouter; credential/network/OpenRouter failure does not kill Codex; core persistence failure remains fatal. |
| Secret hygiene | Fake key absent from AppState/preferences/history/catalog/transcript/notices/Debug/diagnostics/TUI snapshots/process arguments/environment; queue rejection returns ownership. |
| Fairness/shutdown | Tool-heavy Codex traffic cannot starve conversation or fail-closed events; OpenRouter/control traffic remains bounded; active task settles once; both runtimes join before terminal restore. |
| Network policy | All normal tests use fakes/127.0.0.1 and perform no external DNS or authenticated request. |

## 13. Risks and mitigations

- **Plaintext credential exposure:** plainly document same-user, backup, snapshot, disk, and Codex full-access exposure; enforce file invariants and narrow lifetime; plan Keychain migration.
- **Cross-provider leakage:** tagged IDs everywhere; provider-owned history access only; captured-body switching tests.
- **Duplicate billing/output:** never retry chat POST; terminal and cancellation events are idempotently scoped.
- **Endpoint/schema drift:** implement the locked official paths and verify their current response schema immediately before the HTTP phase; update typed fixtures and compatibility notes rather than probing/falling back at runtime.
- **Local-store partial commit:** conversation-first/index-second atomic writes and startup index rebuild.
- **Unexpected history growth:** hard ceilings; no silent canonical truncation/eviction.
- **Codex failure isolation regression:** preserve Codex service behavior and split provider availability from true core fatal errors.
- **Preference downgrade:** old binary refuses v2 overwrite; document that settings reappear on upgrade.
- **Terminal secret paste:** dedicated editor, fixed mask, ownership channel, zeroization, and snapshot tests.
- **Dependency expansion:** use narrow maintained crates only; no general provider framework or RepoPrompt dependency.

There are no remaining owner decisions. Schema verification for the three locked endpoints is an implementation prerequisite, not a product question.

## 14. Completion and handoff checklist

The milestone is complete only when all phases and acceptance tests land together at coherent compiling/tested boundaries and shipped documentation is updated.

- [ ] The shipped Codex regression contract is preserved.
- [ ] OpenRouter is text-chat only over the three locked endpoints.
- [ ] CredentialStore and FileCredentialStore enforce the owner-only runtime/openrouter-home/api-key contract.
- [ ] Keychain migration debt is explicit; no Keychain dependency exists.
- [ ] Cross-provider /model is always a blank fresh boundary and /resume is the only history-restoring cross-provider action.
- [ ] PreferencesV1 migration and downgrade behavior are tested.
- [ ] All security and resource ceilings are enforced and tested.
- [ ] Normal tests are entirely offline and use no real credential.
- [ ] RepoPrompt CE was not modified and is not a dependency.
- [ ] README/AGENTS/LOC documentation is finalized only after shipping behavior exists.
- [ ] cargo fmt --check
- [ ] cargo clippy --all-targets --all-features -- -D warnings
- [ ] cargo test --all-targets
- [ ] Git diff contains no credential, generated cache, test artifact, or change under repoprompt-ce.
- [ ] No commit or push was performed.
