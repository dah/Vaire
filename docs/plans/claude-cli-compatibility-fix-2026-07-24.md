# Claude CLI compatibility fix

Date: 2026-07-24  
Status: complete

## Goal

Make the first real Claude Code turn establish reliably after native subscription sign-in, expose
the current documented `fable` model alias, and make both `/reasoning` forms report Claude's
unsupported effort control consistently.

## Background

- A fresh Vairë registration becomes `CreationPending` before the CLI is spawned and becomes
  established only after a matching `system/init` is accepted. The reported `Protocol/Protocol`
  failure initially suggested a pre-init parser incompatibility.
- The exact installed Claude Code 2.1.178 invocation instead exited with status 1 before stdout
  emitted any JSON envelope. Bounded one-variable probes isolated the inline
  `--mcp-config {}` argument: the CLI rejected the bare object as invalid MCP configuration.
- Replacing only that value with the accepted zero-server shape `{"mcpServers":{}}` keeps
  `--strict-mcp-config` enabled and configures no MCP servers. The resulting sanitized stream
  began with a matching `system/init` and was already accepted by the existing strict parser.
- Installed Claude Code 2.1.178 advertises the additive `fable` model alias.
- With Claude or OpenRouter active, `/reasoning` already reported unsupported, while
  `/reasoning <value>` incorrectly fell through to a Codex-only model lookup.

## Approach

Keep the parser and its establishment boundary strict. Change only the empty MCP argument from the
rejected `{}` value to `{"mcpServers":{}}`, retaining both `--strict-mcp-config` and zero
configured servers. Assert the exact argument pair offline, then reproduce the sanitized successful
envelope classes through the fake CLI/service path so a fresh registration reaches `Established`
and completes.

Do not add a pre-init buffer or relax envelope classification: the installed CLI emitted no stdout
for the failing invocation, and the corrected invocation emitted matching `system/init` first.
All existing correlation, duplicate/terminal, contradiction, EOF, byte, event, and text limits
therefore remain unchanged.

Add `ClaudeModelAlias::Fable` through the existing enum-driven catalog, CLI argument, picker, and
persistence paths without removing established aliases or changing the schema. Route both
`/reasoning` forms through the same provider-specific unsupported notice for Claude and
OpenRouter, with no provider work or state mutation.

## Work Item 1 sanitized finding

The exact installed Claude Code 2.1.178 invocation exited with status 1 before stdout emitted any
JSON envelope. Its two stderr lines were discarded after sanitizing as non-JSON shape only. Bounded
one-variable probes and installed `--help` comparison isolated the inline `--mcp-config {}` value:
the CLI classified that value as invalid MCP input. Replacing only that value with the valid empty
configuration shape `{"mcpServers":{}}` preserved strict MCP mode and zero configured servers.

With that one value changed, the fresh turn exited successfully and emitted this sanitized,
fully matching-session structure:

1. `system/init`
2. `system/status`
3. `rate_limit_event`
4. three `stream_event` envelopes
5. `assistant`
6. three `stream_event` envelopes
7. top-level `result/success`

There was no parser divergence: matching `system/init` was first, and the existing parser already
accepted the subsequent correlated metadata, semantic envelopes, and top-level terminal result.
Therefore `src/claude/protocol.rs` remains unchanged; relaxing its init gate would contradict the
observed evidence. No raw prompt, reply, tool, provider error, credential, or private provider file
content was emitted or retained by the diagnostic capture.

Add `ClaudeModelAlias::Fable` and expose the catalog in the additive order `default`, `fable`,
`opus`, `sonnet`, `haiku`. Do not remove established aliases or accept arbitrary full model IDs
without an authoritative catalog. The existing catalog projection, blank-boundary selection, CLI
argv, session records, and preferences should reuse the enum without a schema version change.

For reasoning, preserve the completed milestone's unsupported contract. Route both `/reasoning`
and `/reasoning <value>` through the same provider-specific notice for Claude and OpenRouter; do not
add an undocumented CLI effort flag or reasoning-panel collection.

## Work Items

- [x] **1. Capture the structural failure safely.** Reproduce one fresh turn with the exact Vairë
  CLI contract and a fixed harmless prompt. Record only sanitized process/envelope structure,
  identify whether the failure reaches parsing, and add that structure to this plan. Never commit or
  retain raw output.
- [x] **2. Add failing-first regressions and the narrow compatibility fix.** The capture proved that
  no envelope reached the parser: installed Claude rejected `--mcp-config {}` before streaming.
  `src/claude/tests.rs` now requires the valid strict-empty `{"mcpServers":{}}` value and reproduces
  the successful sanitized envelope classes through the offline fake CLI/service. `src/claude/config.rs`
  supplies that value. `src/claude/protocol.rs` remains unchanged because matching `system/init`
  was first and all existing establishment, correlation, terminal, and resource invariants apply.
- [x] **3. Add the `fable` alias end to end.** Update `src/provider.rs` and
  `src/claude/config.rs` atomically with the completed milestone's documented model list. Cover
  enum/string/serde behavior, five-entry catalog order, and one `--model fable` argv assertion; rely
  on the existing enum-driven picker and persistence paths rather than duplicating their full suites.
- [x] **4. Correct provider-specific reasoning feedback.** Update the reducer so both reasoning
  command forms report that Claude/OpenRouter reasoning effort is unsupported and emit no effect.
  Add focused reducer tests proving neither form opens a picker, mutates the selected model, or
  launches a turn.
- [x] **5. Update the durable record and validate.** Reconcile this plan, the completed Claude
  milestone, and `README.md` with the captured compatibility rule and additive `fable` catalog.
  Run targeted offline Claude/app/persistence tests, then `cargo fmt --all -- --check`,
  `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test --all-targets`, and
  `git diff --check`.

## Acceptance Criteria

- The durable record identifies the observed pre-stream rejection of `--mcp-config {}`; production
  and offline argv coverage require `--strict-mcp-config --mcp-config '{"mcpServers":{}}'`.
- The corrected fake-CLI stream begins with the matching `system/init`, reproduces the sanitized
  envelope classes, establishes the fresh registration, and completes the turn.
- Mismatched or malformed correlation, unknown unclassified pre-init envelopes, duplicate or
  post-terminal events, contradiction, resource exhaustion, and premature EOF still fail closed
  with the original UUID preserved.
- `/model` exposes `fable`; selecting it passes `--model fable`, starts a blank Claude
  conversation, and round-trips through enum-driven preferences/session storage without a schema
  migration.
- `/reasoning` and `/reasoning <value>` consistently explain that Claude and OpenRouter reasoning
  effort is unsupported and never start provider work or mutate selection state.
- Normal tests remain offline and do not inspect real auth, Keychain, private Claude storage, or
  RepoPrompt. RepoPrompt remains read-only reference material with no runtime/build dependency.
- All required Rust gates pass.

## Evidence Limits

The diagnostic evidence is structural and deliberately bounded. With `--mcp-config {}`, the exact
invocation exited with status 1 before emitting stdout; stderr content was discarded after
classification as non-JSON shape. One-variable probing identified the rejected argument. With only
the value changed to `{"mcpServers":{}}`, the sanitizer retained top-level `type`, `subtype`, and
session-correlation form only. No prompt, reply, tool payload, raw provider error, credential, or
private provider file content was displayed, persisted, or committed.

## Validation

Completed on 2026-07-24:

- `cargo test --lib claude::`: 29 passed;
- `cargo test --lib app::tests::reasoning::`: 2 passed;
- `cargo test --lib persistence::`: 11 passed;
- `cargo fmt --all -- --check`: passed;
- `cargo clippy --all-targets --all-features -- -D warnings`: passed;
- `cargo test --all-targets`: 377 passed, 3 explicitly ignored installed-CLI smoke tests;
- `git diff --check`: passed.

## Open Questions

None. Work item 1 resolved diagnosis before production changes: the CLI rejected its MCP
argument before streaming. Alias support is additive, and Claude effort remains unsupported until
an approved CLI contract and milestone add it.

## References

- Existing milestone: `docs/plans/claude-code-provider-2026-07-24.md`
- Claude Code CLI reference: <https://code.claude.com/docs/en/cli-reference>
- Claude Code headless mode: <https://code.claude.com/docs/en/headless>
- Read-only reference root: `/Users/danhancu/Developer/RepoPrompt/repoprompt-ce`
