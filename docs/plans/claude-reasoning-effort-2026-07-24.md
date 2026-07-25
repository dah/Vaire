# Claude Code reasoning effort milestone

Date: 2026-07-24
Status: complete

## Goal

Add Claude Code reasoning-effort selection to Vairë using the installed CLI's documented `--effort` contract while preserving the shipped subscription-auth, session, safety, provider-isolation, and one-active-conversation behavior.

The evidence and rationale are recorded in `docs/investigations/claude-reasoning-level-2026-07-24.md`.

## Completion and supersession

This milestone supersedes the earlier Claude provider and CLI-compatibility plans only where they
recorded Claude effort as unsupported or 2.1.178 as the current minimum. It does not rewrite that
history or change the established auth, session, direct-spawn, strict empty MCP, permission, tool,
environment, failure, or storage boundaries.

The UI and CLI spelling is exactly `xhigh`. Preferences V4 uses Serde's internal snake-case enum
token `x_high`; that persistence spelling is never accepted as a command value or emitted to the
Claude CLI. Vairë stores and displays the current requested override. It does not discover the
effective effort, and Claude Code may use a provider/model default when the flag is absent or clamp
a configured request. Effort remains provider-wide launch configuration: changing it preserves
the active session, resume does not restore a historical effort, and `ClaudeSessionV1` plus turn
records remain unchanged.

## User experience

- Claude `/reasoning` reports the current effort and the choices `default`, `low`, `medium`, `high`, `xhigh`, and `max`.
- Claude `/reasoning default` clears Vairë's override; the CLI receives no `--effort` argument.
- Claude `/reasoning <level>` accepts only the five exact lowercase installed values.
- Changing effort preserves the active Claude conversation, confirms the selection applies to the next turn, and avoids a persistence write when the selected value is already current.
- Effort changes are blocked while a Claude turn or eager Claude session creation is active.
- The Claude header shows the selected override as `effort default` or `effort <level>`.
- OpenRouter reasoning remains unsupported. Codex reasoning remains derived from its server catalog.
- `/thinking` remains separate and does not collect Claude reasoning output.

## Protocol and execution contract

- Raise the supported Claude Code baseline from 2.1.178 to 2.1.218.
- Add a typed `ClaudeEffort` with `Low`, `Medium`, `High`, `XHigh`, and `Max`.
- Keep Claude effort separate from Codex's dynamic reasoning string.
- Snapshot `Option<ClaudeEffort>` into `Effect::SendClaudeMessage` when the user sends.
- Preserve that snapshot through native subscription revalidation, lazy UUID registration, pointer persistence, and recursive send requeue.
- Carry effort through `ClaudeService::prepare_turn`, `PreparedClaudeTurn`, and both `ClaudeInvocation` variants.
- Emit exactly one `--effort <value>` pair for configured fresh and resumed children; emit neither argument for provider default.
- Keep direct no-shell spawning, prompt-on-stdin, environment scrubbing, strict empty MCP configuration, permission bypass, and all existing bounds unchanged.
- Never use `CLAUDE_CODE_EFFORT_LEVEL`, private control messages, private Claude storage, or RepoPrompt runtime code.

## State and persistence

- Store one provider-wide `selected_effort: Option<ClaudeEffort>` in Claude preferences.
- Introduce Preferences V4. Migrate V1, V2, and V3 to V4 with no Claude override.
- Make V4 reject unknown same-version fields/tokens and preserve future versions byte-for-byte through the existing no-overwrite gate.
- Preserve Claude effort across provider switches, alias blank boundaries, explicit resume, failed resume/new, and creation uncertainty.
- Keep `ClaudeSessionV1` unchanged. Historical per-turn effort display is outside this milestone; a later milestone must add a versioned session migration if that feature is approved.

## Safety and failure behavior

- Native subscription auth remains authoritative and is revalidated before every Claude turn.
- Effort is non-secret and must not alter credential handling or environment scrub rules.
- Auth, store, spawn, protocol, interruption, and persistence failures retain existing pointer/session guarantees.
- Stale Claude events cannot mutate the selected effort.
- Model-dependent CLI clamping is provider behavior. Vairë displays the requested override and never claims it is the effective effort.

## Acceptance tests

- All five values round-trip through parsing, display, Serde, preferences, effects, service preparation, and argv.
- `default` maps only to `None`; `ultracode`, `x-high`, uppercase, empty, and unknown values are rejected locally.
- New and resumed configured invocations each contain exactly one effort pair; default contains none.
- The captured effort survives auth awaits and lazy session creation without rereading mutable UI state.
- Effort changes preserve conversation state and are blocked during active/pending Claude work.
- Alias/provider/resume/failure/uncertainty transitions preserve the provider preference without leaking Codex reasoning.
- Preferences V1/V2/V3 migrate to V4; future and corrupt files remain protected from overwrite.
- Header/help text is provider-specific and `/thinking` behavior is unchanged.
- Ignored installed-CLI smoke coverage checks only version/help for the flag and five values, never auth or private sessions.
- Normal tests stay offline.

## Out of scope

- Historical per-turn effort display or Claude session-schema changes.
- Effective-effort discovery or model-specific capability tables.
- `ultracode` until supported executable capability discovery is designed.
- Interactive effort popup.
- Claude reasoning summaries or hidden chain-of-thought.
- RepoPrompt dependencies, environment transport, live-control messages, MCP, plugins, or multi-agent runtime behavior.

## Validation

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets
git diff --check
```
