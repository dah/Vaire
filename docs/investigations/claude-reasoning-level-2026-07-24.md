# Investigation: Claude Code Reasoning Level Support

## Summary
Investigation complete. Vairë should add a typed optional Claude effort preference with exactly `low`, `medium`, `high`, `xhigh`, and `max`; `None` means provider default and emits no flag. Snapshot that value into each Claude send effect, preserve it through auth and lazy session creation, and pass `--effort` exactly once on every configured fresh or resumed child. Store the current selection in a deliberate Preferences V4 migration, keep it isolated from Codex reasoning, and raise the proven Claude Code baseline to 2.1.218.

Historical per-turn effort display is explicitly outside this milestone, so `ClaudeSessionV1` remains unchanged. RepoPrompt CE corroborates the typed-value and start/resume concepts, but its per-model defaults, encoded model IDs, environment transport, private live controls, and resume fallback are not appropriate for Vairë.

## Symptoms
- Vairë currently rejects both `/reasoning` and `/reasoning <level>` for Claude because reasoning selection is Codex-only.
- Claude model selection and turns work after the prior MCP compatibility fix, but there is no Claude reasoning-level state, picker, persistence, or CLI argument.
- The implementation must preserve provider-scoped state and existing Claude new/resume/session safety contracts.

## Background / Prior Research
- Installed Claude Code `2.1.218` advertises `--effort <level>` as “Effort level for the current session” with exactly `low`, `medium`, `high`, `xhigh`, and `max`. Top-level help does not state a default or whether resumed sessions retain a historical value.
- Anthropic's [CLI reference](https://code.claude.com/docs/en/cli-reference#cli-flags) documents `--effort` as a launch-time/current-session override, so Vairë can pass it on every fresh and resumed non-interactive Claude invocation without mutating Claude-owned settings.
- Anthropic's [effort configuration](https://code.claude.com/docs/en/model-config#adjust-effort-level) documents model-dependent defaults and graceful clamping of unsupported levels. Current web documentation additionally mentions `ultracode`, but the installed supported CLI help does not advertise it; the implementation should use the executable-advertised intersection unless explicit version/capability discovery is added.
- `--resume <id> --effort <level>` is the documented launch composition. Neither help nor documentation makes effort an immutable conversation property.
- Both probes were read-only: one used only official Anthropic pages; the other used only `claude --version` and `claude --help`, without reading auth state, credentials, or private sessions.

## Investigator Findings

### Executive conclusion

The builder direction is sound with two qualifications:

1. `--effort` must be emitted exactly once on every **configured** fresh or resumed Claude turn. For provider default (`None`), omit the flag because the installed CLI advertises no `default` value.
2. The provider-wide typed preference should use a preferences V4 migration. Extending V3 in place would let an older V3 binary ignore and later erase the field, bypassing the existing future-version no-overwrite protection.

No Claude session-record migration is needed: effort is a launch preference, not Claude-owned context or bounded display-history data.

### Hypothesis resolution

| # | Verdict |
|---|---|
| 1 | **Proved.** Add Claude-specific `ClaudeEffort::{Low, Medium, High, XHigh, Max}` and use `Option<ClaudeEffort>::None` for provider default. Never reuse Codex's dynamic string. |
| 2 | **Proved with qualification.** Both New and Resume get exactly one `--effort <value>` pair for `Some`; `None` emits no effort argument. Existing stdin and environment boundaries stay unchanged. |
| 3 | **Provider-wide scope.** Preserve effort across resume, alias/provider changes, failed resume, and creation uncertainty. Block changes during an active Claude turn or pending eager creation. |
| 4 | **Preferences V4; Claude session V1 unchanged.** V1/V2/V3 preferences migrate with `selected_effort: None`; future versions remain non-overwritable. |
| 5 | **Trace proved.** Snapshot effort in `Effect::SendClaudeMessage`, preserve it through auth and lazy-create requeue, then carry it through service preparation and `ClaudeInvocation` to argv. |
| 6 | **Reference is conceptual only.** RepoPrompt supports a closed enum and separate UI, but its per-model preferences, environment/private live controls, MCP, and resume fallback must not be copied. |
| 7 | **Raise to 2.1.218 on current evidence.** Retaining 2.1.178 requires a new capability probe or authoritative proof that it supports `--effort`. |
| 8 | **Reuse `/reasoning`; add no picker.** Support `/reasoning default`, show Claude effort in the header/help, and keep `/thinking` unsupported for Claude output collection. |

### Vairë evidence and scope

- `AppState.selected_reasoning` is an untyped `Option<String>` (`src/app/state.rs:12-15`), Codex persistence is dynamic (`src/persistence/domain.rs:36-44`), load hydrates it only for Codex (`src/app/account.rs:20-39`), and Claude startup clears it (`src/app/reducer/event/claude.rs:8-23`). This is not a reusable cross-provider type.
- Put the closed snake-case serde/CLI enum in the Claude provider boundary, exported with the existing types at `src/claude.rs:21-25`. Do not add a `Default` enum case or accept `ultracode`; omission is the only evidenced provider-default representation.
- Claude aliases are typed but are not authoritative resolved model IDs (`src/provider.rs:58-99`, `src/claude/types.rs:10-16`); resolution arrives later and the header may show alias -> resolved model (`src/tui/header_message.rs:186-196`). Per-alias/model filtering would guess capabilities.
- `ClaudeSessionV1` stores alias, resolved display metadata, lifecycle, and bounded turns (`src/claude/types.rs:36-80`); the module defines this as Vairë registration/display history while Claude state remains opaque (`src/claude.rs:1-4`). Do not store effort per session or turn.

#### Lifecycle semantics

| Transition | Required behavior |
|---|---|
| `/resume` | Keep current provider-wide effort; restore saved alias/session/history only (`src/app/reducer/event/claude.rs:167-240`). |
| Alias change | Keep effort, but retain the blank-conversation and pointer-clear boundary (`src/app/reducer/intent.rs:210-227`). |
| Provider switch | Keep each provider sub-preference; switching remains a blank-conversation boundary (`src/app/reducer/intent.rs:166-198`). |
| Active Claude turn | Reject with `wait for or interrupt the active turn`; argv is already fixed and Vairë has no live update. Active sends are already guarded at `src/app/account/guards.rs:84-100`. |
| Pending eager creation | Reject until it settles, matching the existing pending-create model guard (`src/app/reducer/intent.rs:156-161`). |
| Resume failure | Preserve pointer and effort; a later explicit `/resume` or `/new` uses the current effort (`src/app/reducer/event/claude.rs:261-275`). |
| Creation uncertainty | Preserve UUID and effort; sends remain blocked until explicit resolution. Recovery maps to Resume (`src/app/reducer/event/claude.rs:140-165`; `src/claude/service.rs:115-141`). |
| Explicit-new failure | Preserve prior conversation and effort (`src/app/tests/claude_integration.rs:175-213`). |

### Exact reducer-to-argv trace

1. Sending sets `TurnState::Starting`, appends the transcript entry, and emits `Effect::SendClaudeMessage { text }` (`src/app/reducer/intent.rs:392-424`). Change it to `{ text, effort: Option<ClaudeEffort> }`, snapshotted at Enter. Update the definition (`src/app/actions.rs:49-59`) and dispatch (`src/backend/effects.rs:53-63`).
2. Backend revalidates native subscription auth first (`src/backend/effects/claude.rs:250-284`). Carry the snapshot; do not re-read UI state after the await.
3. Lazy first send durably creates the UUID/record and pointer, emits `ClaudeSessionStarted`, then requeues the send effect (`src/backend/effects/claude.rs:286-355`). Preserve the same effort in that requeue.
4. The ready path calls `prepare_turn` and `launch_prepared_turn` (`src/backend/effects/claude.rs:375-407`). Add the typed effort argument to `prepare_turn`.
5. Preparation maps Fresh to New (`src/claude/service.rs:94-125`), Established to Resume (`src/claude/service.rs:127-133`), and CreationUncertain to Resume/recovery (`src/claude/service.rs:134-141`). Add effort to both `ClaudeInvocation` variants; `PreparedClaudeTurn` owns the invocation (`src/claude/service.rs:20-27`).
6. `run_turn` passes the prepared invocation and prompt to `ClaudeChild::spawn` (`src/claude/service.rs:342-384`).
7. The invocation enum and New/Resume argv arms are `src/claude/config.rs:38-48,78-117`; common turn policy is `src/claude/config.rs:120-132`. Append one `--effort <as_str>` for `Some` in their common path. Omit it for `None`; keep `--session-id` for New and `--resume` for Resume.
8. Preserve safety: spawn builds argv and pipes stdio (`src/claude/process.rs:560-582`); the prompt plus newline is written only to stdin (`src/claude/process.rs:645-660`); environment setup strips inherited `ANTHROPIC_*` and names beginning `CLAUDE`, then sets only `CLAUDE_CONFIG_DIR` and `NO_COLOR` (`src/claude/config.rs:400-409`). Do not introduce `CLAUDE_CODE_EFFORT_LEVEL`.

### Persistence and migration

#### Preferences V4

Current preferences are V3 (`src/persistence/domain.rs:11-14`); Claude V3 has only pointer and alias (`src/persistence/domain.rs:54-67`). Introduce `ClaudePreferencesV4 { auto_resume_session_id, selected_model_alias, selected_effort: Option<ClaudeEffort> }` and `PreferencesV4` with version 4. Retain a deserializable V3 shape, add `V3_PREFERENCES_VERSION` and `LoadNotice::MigratedV3`, and convert V1/V2/V3 to V4 with effort `None`.

Update public exports (`src/persistence.rs:5-8`), `PreferencesPort` (`src/persistence/domain.rs:193-203`), `AppState.preferences` (`src/app/state.rs:21`), and `Effect::Persist` (`src/app/actions.rs:72`). Add a V3 migration arm to the version dispatch (`src/persistence/file.rs:99-150`) and make 4 current. Preserve unknown-version `may_overwrite: false` (`src/persistence/file.rs:144-149`), already tested byte-for-byte through backend writes (`src/backend/tests.rs:137-157`). Unknown V4 effort tokens must be corrupt/non-overwritable.

**Why not additive V3:** these V3 structs do not deny unknown fields (`src/persistence/domain.rs:36-67`). An older V3 binary could ignore the new field, still consider the file writable, and erase it on save. V4 forces that binary through its existing unsupported-version gate.

#### Claude sessions remain V1

Keep `SESSION_VERSION = 1` (`src/claude/store.rs:18`) and `ClaudeSessionV1` unchanged. Non-V1 files are rejected before decode (`src/claude/store.rs:274-293`), and session/turn structs deny unknown fields (`src/claude/types.rs:36-60`). A session V2 would add migration cost and false historical ownership without serving launch/restore.

### RepoPrompt CE: concepts only

Useful corroboration:

- It has a Claude-specific closed enum with the exact five values and explicit conversions (`repoprompt-ce/Sources/RepoPrompt/Infrastructure/AI/Providers/ClaudeCode/ClaudeAgentToolPreferences.swift:494-538`).
- Its tests separate model from effort and expose five UI values (`repoprompt-ce/Tests/RepoPromptTests/AI/ModelPickerStringOrderingTests.swift:57-104`).
- It supplies effort to normal start/resume and fresh fallback (`repoprompt-ce/Sources/RepoPrompt/Features/AgentMode/Runtime/Claude/ClaudeAgentModeCoordinator.swift:545-565,578-625`), corroborating that Resume is not exempt.

Do **not** copy:

- Missing/invalid effort defaults to High and preferences combine global plus per-agent-kind/per-model storage (`repoprompt-ce/Sources/RepoPrompt/Infrastructure/AI/Providers/ClaudeCode/ClaudeAgentToolPreferences.swift:350-423`). Vairë needs explicit `None` and no guessed model table.
- Effort is encoded into model IDs as `base:effort` (`repoprompt-ce/Sources/RepoPrompt/Features/AgentMode/Models/ModelSelection/ClaudeModelSpecifier.swift:3-66`). Keep Vairë aliases and effort separate.
- The one-shot path uses `CLAUDE_CODE_EFFORT_LEVEL`, not `--effort` (`repoprompt-ce/Sources/RepoPrompt/Infrastructure/AI/Providers/ClaudeCodeProvider.swift:20-49,145-170`). This conflicts with Vairë's scrub policy and is not argv evidence.
- The native runtime uses private-looking `apply_flag_settings`/`effortLevel` control messages (`repoprompt-ce/Sources/RepoPrompt/Infrastructure/AI/Providers/ClaudeCode/SDK/ClaudeNativeProcessSessionController.swift:672-762`) and launch argv has no effort flag (`repoprompt-ce/Sources/RepoPrompt/Infrastructure/AI/Providers/ClaudeCode/SDK/ClaudeNativeProcessSessionController.swift:1971-2008`). Vairë must not add a long-lived/live-update transport.
- Resume errors clear the provider session and retry fresh (`repoprompt-ce/Sources/RepoPrompt/Features/AgentMode/Runtime/Claude/ClaudeAgentModeCoordinator.swift:573-625`), conflicting with Vairë's preserve-and-block contract.
- MCP/runtime servers, plugins/compatible providers, tool cards, approvals, multi-session coordination, injected auth environments, model whitelists, and live updates are all out of Vairë scope.

### Minimum Claude version

Read-only local probes on 2026-07-24 returned `2.1.218 (Claude Code)` and help advertised `--effort <level>` for the current session with exactly `low, medium, high, xhigh, max`. The code accepts every version at least 2.1.178 (`src/claude/config.rs:20,242-250`), but no evidence proves 2.1.178 accepts the flag.

Raise `TESTED_CLAUDE_VERSION` to 2.1.218 for static support. Update the outdated fixture from 2.1.177 to 2.1.217 (`src/runtime/types.rs:312-329`), README (`README.md:19`), and durable guide (`AGENTS.md:24,93`). Extend the ignored smoke test, which checks version/help without auth state (`tests/installed_cli_smoke.rs:15-60`), to assert top-level `--effort` and five tokens.

The safe alternative is a bounded, cancellable `claude --help` capability probe. That is more state, UX, and testing than the static proven gate, so it is not the smallest milestone.

### Smallest coherent UX

- Keep parsing unchanged: `/reasoning <value>` already becomes `Intent::SelectReasoning(String)` (`src/command.rs:30-70`). Validate by active provider in the reducer.
- Codex keeps its server-derived list and validation (`src/app/reducer/intent.rs:49-60,271-289`). OpenRouter remains unsupported.
- Claude `/reasoning` shows `Claude effort: <current>; choices: default, low, medium, high, xhigh, max`. `/reasoning default` stores `None`; exact lowercase concrete values store `Some`. Invalid values produce `unsupported Claude effort <value>; use /reasoning` without mutation. Block during active turn/pending creation.
- Change Claude header `{model}/reasoning n/a` (`src/tui/header_message.rs:180-214`) to `{model}/effort default` or `{model}/effort <value>`.
- Change Codex-only help (`src/command.rs:3-4`) to “List or select Codex reasoning / Claude effort”; explain that `default` uses the provider default.
- `/thinking` remains emitted-content UI, not effort. Preserve non-Codex panel behavior (`src/tui/conversation.rs:112-130`) and the Claude assertion that reasoning is not collected (`src/tui/tests/claude_ui.rs:123-126`). Add no picker or reasoning-output support.

### Eliminated alternatives

1. Reusing Codex `Option<String>` loses type safety and conflates a dynamic catalog with a closed CLI contract.
2. `Some(Default)` or `--effort default` is not executable-advertised.
3. Per-alias/model state guesses unresolved capabilities; per-session/turn state pollutes display history and makes resume override the current preference.
4. Passing effort only on New misses every resumed direct process and uncertainty recovery.
5. Reading preference late creates ambiguity across auth/lazy creation; snapshot it in the effect.
6. Live-updating the active child has no supported Vairë transport; block instead.
7. Extending V3 permits downgrade data loss; V4 preserves forward protection.
8. Copying RepoPrompt env/private control/MCP machinery violates scope.
9. Keeping minimum 2.1.178 without probing advertises support to an unproved CLI and may make every turn fail.

### Focused test matrix

| Layer | Tests |
|---|---|
| Type | Five serde/parse/display/argv values; reject unknown, Codex-only, `x-high`, `ultracode`; `None` is default. |
| Preferences | V4 round-trip for None/all values; V1/V2/V3 -> V4+None; invalid token corrupt/no-overwrite; V5 byte-preserved. Extend `src/persistence/tests.rs:33-150,236-264` and `src/backend/tests.rs:137-157`. |
| Reducer | Claude list/select/default/invalid, active/pending blocks and Persist; Codex unchanged; OpenRouter rejected. Resume, alias change, provider away/back, failed resume, new failure, and uncertainty preserve effort. |
| UI | Header default/concrete effort, help wording, `/thinking` still says Claude reasoning is not collected. |
| Backend | Snapshot survives auth and lazy-create requeue (`src/backend/tests.rs:280-351`); auth failure launches no child. |
| Argv | New+XHigh and Resume+Max have exactly one pair; None has none; prompt/auth absent. Extend `src/claude/tests.rs:115-153`. |
| Process | Prompt stdin and environment scrubbing remain unchanged while argv carries effort (`src/claude/tests.rs:680-723`). |
| Service | Fresh uses `--session-id`, Established and CreationUncertain use `--resume`; all configured paths carry effort; session V1 unchanged. |
| Version | 2.1.218 accepted, 2.1.217 rejected, ignored installed smoke verifies the five-value help without auth/session access. |

## Investigation Log

### Initial assessment - likely integration seams
**Hypothesis:** Claude Code exposes a documented effort control that must be mapped through Vairë's provider-tagged domain, UI, persistence, backend effect, and direct CLI argv.
**Findings:** Confirmed, with provider-default represented by flag absence. The effort must be captured in `Effect::SendClaudeMessage`, carried through auth/lazy creation, and added to both invocation variants.
**Evidence:** `src/app/reducer/intent.rs:392-424`; `src/app/actions.rs:49-59`; `src/backend/effects/claude.rs:250-407`; `src/claude/service.rs:94-141`; `src/claude/config.rs:38-132`.
**Conclusion:** Add a typed Claude-only value and preserve the existing direct-process, prompt-on-stdin, and credential-scrubbing boundaries.

### Initial assessment - RepoPrompt reference
**Hypothesis:** RepoPrompt CE already implements Claude effort selection and can clarify accepted values, default behavior, invocation scope, and UI/persistence semantics without becoming a dependency.
**Findings:** Confirmed as conceptual reference only. RepoPrompt has a closed five-value enum and supplies effort to start/resume paths, but also uses per-model preferences, `CLAUDE_CODE_EFFORT_LEVEL`, encoded `model:effort` identifiers, and private-looking live control messages.
**Evidence:** `repoprompt-ce/Sources/RepoPrompt/Infrastructure/AI/Providers/ClaudeCode/ClaudeAgentToolPreferences.swift:350-423,494-538`; `repoprompt-ce/Sources/RepoPrompt/Features/AgentMode/Runtime/Claude/ClaudeAgentModeCoordinator.swift:545-625`; `repoprompt-ce/Sources/RepoPrompt/Infrastructure/AI/Providers/ClaudeCodeProvider.swift:20-49,145-170`; `repoprompt-ce/Sources/RepoPrompt/Infrastructure/AI/Providers/ClaudeCode/SDK/ClaudeNativeProcessSessionController.swift:672-762`.
**Conclusion:** Reuse the type and lifecycle concepts, not RepoPrompt's transport, persistence topology, or failure semantics.

## Root Cause
Claude effort is absent by deliberate prior milestone scope: the app reducer rejects non-Codex reasoning, Claude preferences have no effort field, Claude sends carry only text/model/session data, and direct argv omits `--effort`. The missing prerequisite was an authoritative supported CLI contract. Installed Claude Code 2.1.218 now supplies that contract, while the existing 2.1.178 minimum is not proven to accept the flag.

## Recommendations
1. Establish this as a bounded Claude effort milestone and update the durable project guide.
2. Add strict `ClaudeEffort` typing for the five installed values; use `Option::None` for provider default.
3. Persist one provider-wide selection in Preferences V4, migrating V1/V2/V3 to no override and preserving future-version files.
4. Keep `ClaudeSessionV1` unchanged because historical requested-effort display is not part of this milestone.
5. Support Claude `/reasoning`, `/reasoning default`, and exact lowercase values; keep OpenRouter unsupported and Codex catalog behavior unchanged.
6. Snapshot effort in the send effect and carry it unchanged through every fresh, resumed, lazy-create, and uncertainty-recovery path.
7. Emit `--effort <value>` exactly once for configured New and Resume invocations, and no effort arguments for provider default.
8. Preserve effort across provider/model/resume transitions while blocking changes during active Claude work or pending eager creation.
9. Update header/help wording while leaving Claude `/thinking` output collection unsupported.
10. Raise the tested Claude Code minimum to 2.1.218 and extend only the ignored, non-authenticated version/help smoke test.

## Preventive Measures
- Keep Codex dynamic reasoning strings and Claude fixed effort values separated by types and persistence fields.
- Freeze turn configuration before asynchronous auth or persistence work; never re-read mutable UI state after an await.
- Assert exact argv independently for fresh, resumed, and provider-default invocations.
- Version preference schemas instead of silently extending a writable older version.
- Treat installed CLI help as the executable compatibility contract and keep optional web-only values out until capability discovery exists.
- Record only requested effort if a later milestone adds historical display; never infer effective effort from model aliases or private Claude storage.
- Keep RepoPrompt a read-only reference with no runtime/build dependency.
