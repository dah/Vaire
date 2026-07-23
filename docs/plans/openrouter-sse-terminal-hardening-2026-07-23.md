# OpenRouter SSE terminal hardening

**Status:** Completed 2026-07-23

## Objective

Make OpenRouter streaming terminal behavior deterministic, bounded, secret-free, and compatible
with provider-resolved models without changing conversation schema V2 or the established
`Completed` versus `FailedIncomplete` persistence contract.

## Durable protocol contract

- Each bounded SSE `data:` event is parsed first as a JSON value and must have an object root.
- A non-null top-level provider `error` object is authoritative and is classified before
  `choices`, completion fields, identity, or usage siblings. A valid numeric or numeric-string
  status is authoritative for HTTP-category mapping and cannot be overridden by conflicting
  symbolic code or `metadata.error_type`; symbolic fields are fallback classification only.
  Remote messages and arbitrary metadata are never projected into failures or diagnostics.
- Missing, null, or empty `choices` represents metadata only. Metadata-only events never establish
  or compare response IDs or models.
- A nonempty `choices` array is semantic and must contain exactly one index-zero choice with a
  structurally valid delta and optional string finish reason.
- Semantic response IDs are optional. The first present nonempty ID establishes the stream ID and
  later present IDs must match it.
- Semantic server models are optional. The first present nonempty server-reported model establishes
  the stream model and later present semantic models must match that server value.
- **The response model is never compared with the requested model ID.** A requested alias may
  legitimately resolve to a different semantic server model.
- Usage is parsed independently and atomically. Missing/null usage is absent; a malformed usage
  object is dropped in full, never clears earlier valid usage, and never converts a valid terminal
  answer into a failed turn.
- After a terminal choice, repeated empty non-error semantic finish markers are idempotent,
  including when their finish reasons differ. Nonempty post-terminal content remains invalid, while
  a bare `finish_reason: "error"` remains a remote failure. Top-level provider-error precedence is
  unchanged. EOF succeeds only after `[DONE]` or a non-null finish reason. Assistant and SSE frame
  bounds remain enforced.
- Parser failures expose only a closed `OpenRouterStreamStage`; stages carry no response payload,
  prompt, reply, ID, model, header, remote message, or secret-derived value.

## Implementation

- `src/openrouter/protocol.rs` is the semantic choice-bearing projection.
- `src/openrouter/sse.rs` performs bounded envelope-first parsing, authoritative provider-error
  classification, semantic identity/model reduction, independent usage handling, and terminal
  validation.
- `src/openrouter/types.rs` defines the closed stream-stage enum and staged
  `OpenRouterFailure` accessor/formatting.
- `src/openrouter/client.rs` assigns content-type framing stages and forwards only normal stream
  events.
- No dependency, retry, cancellation, task, queue, or persistence-schema contract changed.
  The existing failure UI now appends a static stream stage when one is available; no new UI
  workflow was introduced.

## Persistence contract

- A genuine provider/parser failure after nonempty text remains `Failed` with that text only in
  `incomplete_assistant_text`; it restores as `FailedIncomplete` and is excluded from canonical
  history and outbound requests.
- Valid text plus finish plus malformed optional usage remains `Completed` with normal
  `assistant_text`.
- Choice-empty resolved identity metadata remains compatible.
- A later conflicting choice-bearing server model is a failed stream.

## Diagnostics decision

The existing diagnostic sink performs synchronous mutex-protected file I/O. It is not safe to call
from the Tokio chat parsing path for successful optional-metadata drops without broader plumbing.
Therefore no diagnostics integration was added. Staged failures remain available through
`OpenRouterFailure::stage()`, `Debug`, and `Display`. A failure stage is also carried as the
closed enum through the transient service event and application domain event, then appended to the
existing turn-failure message. It is not written to conversation persistence. `UsageDropped`
remains a once-per-stream internal compatibility result covered by tests. None of these paths
carries remote payload data.

## Offline regression coverage

Parser, loopback client, and service/store tests cover:

- fragmented LF/CRLF SSE, comments, event names, reasoning-only chunks, finish, usage, and
  `[DONE]`;
- provider-error precedence over null/malformed completion and usage siblings, including
  numeric and numeric-string 429 statuses that remain `RateLimited` despite conflicting
  authentication metadata;
- missing/null/empty metadata choices and ignored metadata identity;
- malformed usage counts and representations, whole-object dropping, and preservation of earlier
  valid usage;
- a requested alias resolving to a different semantic server model;
- rejection of a later conflicting semantic server model;
- response ID, choice cardinality/index, completion shape, forbidden post-terminal content,
  idempotent same/different repeated non-error finish markers, bare error finishes, after-done,
  UTF-8, frame, content-type, assistant-limit, and premature-EOF stages;
- reopen after valid answer + finish + malformed usage as `Completed`;
- reopen after partial text + authoritative provider error with malformed siblings as display-only
  failed incomplete text, excluded from canonical history; and
- typed parser-stage propagation through service/application reduction and the existing sanitized
  turn-failure UI, without durable stage persistence.

All fixtures are loopback-only and use synthetic credentials and content.
