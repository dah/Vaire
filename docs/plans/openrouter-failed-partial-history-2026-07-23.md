# OpenRouter Failed Partial History — Implementation Record

**Status:** Implemented on 2026-07-23  
**Scope:** OpenRouter failed-stream history only

This record documents the implemented correction for nonempty OpenRouter assistant text that was
streamed to the transcript before the turn terminated as `Failed`. It supersedes only the older
failed-stream retention policy in
`docs/plans/openrouter-chat-provider-2026-07-22.md`; every other shipped OpenRouter contract remains
in force.

## Outcome

A failed OpenRouter stream may now retain its nonempty assistant partial in a dedicated durable
field and restore it for display after relaunch or `/resume`. Restored partials carry explicit domain
metadata and Ratatui renders the exact label:

```text
Agent (incomplete; turn failed):
```

The retained text is display-only. It is never canonical conversation history and is never sent to
a model on a later request.

## Durable data contract

OpenRouter conversation schema V2 adds `incomplete_assistant_text` to each turn record while
preserving `assistant_text` as completed-output storage:

- `Completed` requires `assistant_text` and forbids `incomplete_assistant_text`.
- `Failed` permits a nonempty `incomplete_assistant_text` only when assistant text actually streamed
  before the terminal failure; `assistant_text` remains absent.
- `Interrupted` and `InProgress` permit neither assistant field. They are not partial-output
  checkpoints.
- `canonical_messages()` emits all user messages and only `Completed` assistant text. Failed partials
  are excluded from future OpenRouter POST bodies and model context.
- Valid V1 conversations migrate losslessly to V2. V1 failed turns gain no fabricated partial text.
- Corrupt, unsupported older, and future schema versions fail safely and are not destructively
  rewritten.

The existing source-first atomic write order, owner-only directory/file modes, startup
`InProgress`-to-`Interrupted` repair, text and aggregate limits, index maintenance, and active
conversation/turn invariants are unchanged.

## Runtime and restoration flow

The OpenRouter service accumulates the same accepted nonempty deltas that it emits to the live
transcript. On a non-cancellation client failure it persists the accumulated snapshot in
`incomplete_assistant_text` before publishing the terminal event. Successful turns continue to
persist only the completed final snapshot; cancellation continues to produce `Interrupted` without
a partial checkpoint.

Both restoration paths use the shared OpenRouter history conversion:

1. startup automatic resume of the saved active OpenRouter conversation; and
2. the real unified picker/effect route, including Codex-to-OpenRouter `/resume`.

A restored failed partial becomes an assistant `TranscriptEntry` with
`TranscriptEntryStatus::FailedIncomplete`. Normal user and completed assistant entries retain
`TranscriptEntryStatus::Normal`. The explicit status survives transcript sanitization, bounds,
trimming, and scrolling; the text or item identity is not overloaded as a marker.

Ratatui renders normal entries with the existing `You:` and `Agent:` labels. A
`FailedIncomplete` assistant entry uses the exact accessible label
`Agent (incomplete; turn failed):` in yellow bold, with the retained body rendered normally.

## Context and data-exposure boundaries

Failed partial text is excluded from:

- `canonical_messages()` and every later OpenRouter request;
- titles, notices, diagnostics, and preferences;
- credentials and credential validation; and
- all other non-display paths.

Local OpenRouter histories, including failed partials, remain owner-only plaintext files. This is
organizational isolation, not encryption or protection from same-user processes, backups, disk
recovery, or full-access Codex tools. Migration of the OpenRouter API key to macOS Keychain remains
mandatory technical debt for a later approved milestone. That migration must save and verify the
Keychain item through the credential-store port before deleting the plaintext key file and must
preserve the file on any failure.

## Regression coverage

Offline tests use temporary stores, fake credentials, and loopback HTTP/SSE only. Coverage includes:

- V2 validation, limits, round trips, safe V1 migration, and refusal to overwrite invalid/future
  data;
- service persistence of a failed nonempty partial and no checkpoint for interrupted or empty
  failures;
- terminal and restored transcript propagation with explicit `FailedIncomplete` status;
- normal and narrow Ratatui rendering of the exact incomplete/failed label without changing
  transcript windowing;
- service/store reconstruction followed by startup automatic resume, restoring exactly one marked
  failed partial;
- the actual Codex-to-OpenRouter unified picker/effect route restoring the same stored entry;
- a later captured OpenRouter POST excluding the restored failed partial from canonical context;
- resolved-model terminal metadata flowing through successful service persistence and store reopen;
  and
- completed assistant output surviving restart as normal canonical history.

Required handoff validation remains:

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets
git diff --check
```

## Explicitly unchanged and out of scope

This correction does not redesign delta-queue pressure handling, change empty-completion semantics,
expand observability, migrate credentials to Keychain, add providers, add OpenRouter tools, or add
multimodal input. It does not change Codex protocol/session behavior, provider switching semantics,
or the single active provider/conversation/turn model.
