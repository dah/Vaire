# Investigation: OpenRouter assistant history missing after resume

**Status:** Implemented on 2026-07-23. OpenRouter conversation schema V2 now retains nonempty
failed-stream assistant partials as explicitly marked display-only history, restores them through
startup and `/resume`, and excludes them from canonical model context. The analysis below remains
the historical root-cause record; delta-queue redesign, empty-completion policy, and observability
expansion remain out of scope. The related SSE terminal parser was subsequently hardened
with envelope-first provider-error precedence, authoritative numeric-status classification,
independently tolerant usage, server-established semantic model consistency, and secret-free
failure stages carried into transient turn-failure messages without persistence. See
`docs/plans/openrouter-failed-partial-history-2026-07-23.md` and
`docs/plans/openrouter-sse-terminal-hardening-2026-07-23.md`.

## Summary

Source investigation complete. Live OpenRouter deltas can remain visible even when the turn later fails, but failed/interrupted turns deliberately persist no assistant text. Both resume routes reconstruct completed-only durable history; no additional store or resume defect explains the five observed `Failed + null` turns. If visible failed-turn output is expected to survive relaunch, the recommended product correction is an explicit persisted incomplete/display-only assistant field that is never reused as model context.

## Symptoms

- An OpenRouter conversation automatically resumes after relaunch.
- Restored history contains the user's messages but not the model's replies.
- Switching to a Codex conversation restores its history correctly.
- Returning to the OpenRouter conversation still omits the model's replies.

## Background / Prior Research

A read-only metadata probe inspected the one indexed OpenRouter conversation without reading or printing the API key, prompts, replies, or title:

- Conversation `or_7b783641c113488282b26b5724f8b6e9` contains five turns, all using `moonshotai/kimi-k3`.
- Every turn has outcome `failed`, `assistant_text: null`, and zero durable assistant bytes.
- There are no completed turns, so there is no evidence of a completed turn losing a stored assistant reply.
- The conversation was last updated at `2026-07-23T11:54:59.862Z`.
- Store permissions remain owner-only: directories `0700`, JSON files `0600`.

This proves that the resume path cannot reconstruct the missing replies from the current durable records. It does not yet establish whether all five turns predate the stream-parser fix or whether a post-fix turn still terminates as failed.

## Investigator Findings

### Proven stream, persistence, and restore path

1. ChatAccumulator appends every nonempty content delta to its final-response buffer before emitting the matching TextDelta, and finish() returns that buffer only after a terminal choice or [DONE] (vaire/src/openrouter/sse.rs:151-179). The client forwards deltas immediately and emits one final snapshot only after successful stream completion (vaire/src/openrouter/client.rs:226-272).
2. Before network streaming starts, the service appends and atomically saves an InProgress turn with assistant_text: None (vaire/src/openrouter/service.rs:438-469). This explains why an interrupted process always has a durable pre-turn record.
3. During streaming, deltas are independently delivered to the reducer and appended to the live transcript (vaire/src/openrouter/service.rs:521-535; vaire/src/app/reducer/event.rs:308-318). Therefore text can be visible live even though no assistant text has yet been committed to the conversation file.
4. After the client returns, the service maps success to Completed, cancellation to Interrupted, and every other client error to Failed. It retains final_text only for Completed; failed and interrupted records are deliberately written with assistant_text: None (vaire/src/openrouter/service.rs:543-569). The final store write is awaited before TurnFinished is sent (vaire/src/openrouter/service.rs:567-589).
5. Store validation requires every Completed turn to have Some(assistant_text) and forbids assistant text on InProgress, Interrupted, or Failed turns (vaire/src/openrouter/store.rs:503-520). Normal loading validates the record before returning it (vaire/src/openrouter/store.rs:358-377).
6. The source conversation is serialized and atomically replaced before index maintenance (vaire/src/openrouter/store.rs:389-418). The writer creates a unique 0600 temporary file, writes and syncs it, renames it over the target, then syncs the directory or verifies the exact target bytes (vaire/src/openrouter/store.rs:617-659). A stale or failed index update cannot erase assistant text from a valid source conversation.
7. Startup auto-resume loads the saved conversation and passes it through openrouter_history (vaire/src/backend/lifecycle.rs:499-536). Manual unified-picker resume, including Codex → OpenRouter, loads the same store record and uses the same conversion (vaire/src/app/thread_picker.rs:88-162; vaire/src/backend/effects.rs:201-229). The reducer accepts only the correlated request, switches provider state, and replaces the transcript (vaire/src/app/reducer/event.rs:190-242).
8. openrouter_history always emits the user entry but emits an assistant entry only for Completed + Some(text) (vaire/src/backend/lifecycle.rs:738-763). This matches canonical_messages(), which also excludes failed/interrupted assistant output from future model context (vaire/src/openrouter/types.rs:247-263).

### What the five durable Failed turns prove

The combination of a visibly streamed reply and a durable Failed record with assistant_text: null has one direct source path: the client delivered one or more deltas, later returned an error, and the service successfully saved the terminal Failed state with the partial text deliberately discarded (vaire/src/openrouter/service.rs:521-569).

Two other ways to end without durable assistant text do not fit the observed records:

- If the final save itself fails, the service reports a failed terminal event but does not re-save that rewritten failure; the prior durable state remains InProgress (vaire/src/openrouter/service.rs:567-585). On the next store open, startup repair changes InProgress to Interrupted, not Failed (vaire/src/openrouter/store.rs:213-225). A final-save failure therefore cannot produce the five durable Failed records.
- abandon_prepared_turn can save Failed + None when the conversation pointer cannot be persisted, but that happens before launch_prepared_turn, so it cannot produce a visible streamed reply (vaire/src/backend/effects.rs:271-303; vaire/src/openrouter/service.rs:484-499).

The missing-after-resume behavior is therefore not a second loss in the store or either resume route. The assistant bytes were never retained in the terminal failed records, and both restore routes correctly apply the completed-only durable-history policy.

### Additional source-proven service hazards

- **Bounded delta queue can turn presentation pressure into durable loss.** The chat-event channel holds 64 entries (vaire/src/openrouter/service.rs:18,124-125). Each delta uses try_send; a full or closed queue cancels the request (vaire/src/openrouter/service.rs:521-535). If the client observes that cancellation before completing, the service persists Interrupted + None, even though a partial reply may already have been rendered. This is an independent service defect because UI delivery pressure should not determine durable completion. It does **not** explain the current five records: the resulting persisted outcome is Interrupted, not Failed.
- **Empty success is accepted.** A terminal stream with no content can make finish() return an empty string (vaire/src/openrouter/sse.rs:151-179), and store validation accepts Completed + Some("") (vaire/src/openrouter/store.rs:503-511). This can render as a missing reply but does not match the current Failed records.
- **Transcript bounds are presentation-only.** replace_transcript sanitizes and reapplies entry/byte/newline/display-width limits, which can drop old entries or trim the first retained entry (vaire/src/app/transcript.rs:91-174). It does not mutate the OpenRouter store and cannot explain assistant_text: null.

### Eliminated hypotheses

- **A valid completed assistant reply is dropped during load or history conversion:** disproved by load validation and openrouter_history (vaire/src/openrouter/store.rs:358-377,503-520; vaire/src/backend/lifecycle.rs:738-763).
- **Codex → OpenRouter resume keeps or clears the wrong provider transcript:** disproved by the destination load and explicit replace_transcript (vaire/src/backend/effects.rs:201-229; vaire/src/app/reducer/event.rs:214-242).
- **A stale/duplicate restore overwrites newer state:** disproved by exact automatic/picker correlation (vaire/src/app/reducer/event.rs:190-212) and reducer coverage (vaire/src/app/tests/openrouter_integration.rs:640-658).
- **Startup repair clears completed text:** disproved; repair touches only InProgress turns (vaire/src/openrouter/store.rs:213-225).
- **Index maintenance precedes or replaces source persistence:** disproved by source-first save ordering (vaire/src/openrouter/store.rs:389-418,617-659).
- **A terminal event races ahead of persistence:** disproved; the service awaits the final store operation before sending TurnFinished (vaire/src/openrouter/service.rs:567-589).
- **The five durable failures were caused by event-queue cancellation or a final-save error:** disproved by outcome shape. Those paths durably produce Interrupted or the prior InProgress, not Failed.

### Test coverage audit

No test covers the complete regression: a service-produced completed OpenRouter turn, reconstruction of the store/service/backend, and restoration of its assistant text through startup or a Codex → OpenRouter unified-picker selection.

- The vertical-slice test completes a real streamed turn and verifies both live and stored assistant text, then shuts down without reopening or resuming (vaire/tests/openrouter_vertical_slice.rs:522-593).
- Store tests round-trip a synthetic completed conversation through the same store instance (vaire/src/openrouter/store.rs:722-786). The explicit reopen test changes that turn to InProgress and verifies repair to Interrupted; it does not reopen a completed assistant turn (vaire/src/openrouter/store.rs:802-825).
- The Codex → OpenRouter picker test injects synthetic restored history directly into the reducer; it does not exercise FileOpenRouterStore, OpenRouterService::load_conversation, or the backend effect (vaire/src/app/tests/openrouter_integration.rs:581-658; production load at vaire/src/backend/effects.rs:201-229).
- Automatic-resume reducer coverage preserves the saved model and failed-resume pointer, but does not load completed history (vaire/src/app/tests/openrouter_integration.rs:658-697).
- The saturated-chat-queue test proves shutdown does not deadlock but never asserts the saved terminal outcome or assistant text (vaire/src/openrouter/service.rs:895-963).
- The resolved-model parser test calls OpenRouterClient::chat and asserts only its final event (vaire/src/openrouter/tests.rs:295-328). No production resolved_model persistence field exists, so this coverage does not reach service outcome mapping, the final store write, reopening, or resume. It proves the current parser accepts the terminal usage metadata; it does not establish that the five observed turns ran after that fix.

## Investigation Log

### Initial triage - persistence versus rendering

**Hypothesis:** The missing replies are absent from the durable OpenRouter turn records, or the resume conversion deliberately excludes their terminal outcome.

**Findings:** The symptom survives both startup auto-resume and explicit provider switching, while Codex restoration works. This points to the OpenRouter durable-record or restore path rather than generic transcript rendering.

**Evidence:** User report on 2026-07-23. The immediately preceding parser defect produced visible text followed by `InvalidResponse`, making failed-turn persistence a specific lead to verify.

**Conclusion:** Confirmed by the service/store/resume trace above: the durable failed-turn policy, not a second resume-time loss, explains the missing assistant entries.

## Root Cause

**Immediate root cause:** the live transcript consumes OpenRouter deltas before terminal success is known, while the durable model intentionally retains assistant text only for successfully completed turns. Each observed stream later terminated as a client failure; the service saved Failed + assistant_text: null; startup and manual resume then correctly rebuilt completed-only history, leaving only user messages.

**Underlying cause of the five client failures:** unresolved from source and sanitized metadata alone. The previously fixed terminal usage/resolved-model parser defect is consistent with “visible partial text, then InvalidResponse,” and current parser coverage demonstrates acceptance at the client layer (vaire/src/openrouter/tests.rs:295-328). However, neither the runtime version/commit that produced the five records nor a post-fix completed turn has been established. Do not claim the parser defect caused all five without that chronology.

**Additional-defect conclusion:** no independent store, auto-resume, unified-picker, or transcript-replacement defect explains the five Failed + null records. The delta-queue cancellation hazard is real but would produce Interrupted, so it is preventive follow-up rather than the cause of this incident.

**Recovery conclusion:** the five missing replies are not recoverable from Vairë's current durable data. No assistant bytes were stored. Reissuing the prompts would create new responses rather than recover the originals.

**Product implication:** the completed-only policy is internally safe but does not meet the expectation created when substantial provisional output remains visible after a failed terminal event. If that expectation is accepted as a requirement, preserve failed-turn output in a separate schema field, label it incomplete when restored, and continue excluding it from `canonical_messages()` and all future model requests. Do not weaken the meaning of `assistant_text` or `Completed`.

## Recommendations

1. **Persist failed partial output explicitly if the user expectation is adopted.** Introduce a new conversation schema version with a separate field such as `incomplete_assistant_text`. Initially allow it only for `Failed` turns after at least one nonempty delta. Restore it as a visibly labelled incomplete response, but never include it in `canonical_messages()` or outbound OpenRouter requests. Keep `Completed => assistant_text` unchanged, and do not infer missing V1 partial text during migration.
2. **Add a completed-turn restart regression.** Drive a real loopback SSE turn through OpenRouterService, assert Completed + full assistant_text, drop the service/store, reopen the same FileOpenRouterStore, and assert exact turn outcome/text and canonical ordering.
3. **Add both restoration regressions.**
   - Startup: reconstruct a backend with OpenRouter active and the saved pointer/model, run automatic resume, and assert the restored user and assistant entries exactly once.
   - Provider roundtrip: start on Codex, open the unified picker, select the persisted OpenRouter conversation, execute the real backend effect, and assert provider/model/conversation selection plus exact nonduplicated assistant history.
4. **Extend the parser regression through persistence.** Feed the resolved-model terminal usage sequence through OpenRouterService, wait for TurnFinished, reopen the store, and assert the requested conversation model plus Completed assistant text. This closes the gap between client-parser acceptance and durable behavior.
5. **Fix delta-queue coupling.** Do not cancel a valid network response solely because a presentation-delta queue is full. Use awaited backpressure, bounded delta coalescing, or a separate lossless terminal/final-snapshot path. Add a test with more than 64 small deltas while consumption is delayed and require a completed durable final snapshot.
6. **Improve sanitized failure observability.** Record build identity, terminal failure category/status, delta count/bytes, and static SSE validation stage without recording keys, prompts, replies, authorization headers, titles, remote messages, or response bodies. This is needed to distinguish a post-fix parser rejection from a genuine provider failure.
7. **Reject or explicitly represent empty completions.** Add a terminal-with-no-content test and either classify it as InvalidResponse or render a deliberate empty-response state.

## Preventive Measures

- Require every OpenRouter parser bug fix involving terminal chunks to include a service-level persistence test and a reopen/resume assertion, not only a client callback assertion.
- Preserve the source-first atomic-save ordering and completed-turn validation invariants.
- Add outcome-shape assertions to failure tests: client failure → durable Failed + None; cancellation → durable Interrupted + None; successful completion → durable Completed + Some(full_text).
- Keep both automatic and unified-picker restore paths covered against the same persisted completed fixture so future conversion changes cannot diverge.
