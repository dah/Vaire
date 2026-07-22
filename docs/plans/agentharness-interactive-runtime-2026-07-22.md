# AgentHarness interactive runtime milestone

## Status

Approved for implementation. This is the first post-MVP milestone and supersedes the completed MVP plan only as the active roadmap; the MVP plan remains historical evidence.

## Goal

Evolve the working single-thread chat client into a more capable interactive Codex harness without weakening its state, transport, persistence, or testing foundations.

## User-facing scope

1. `/new` starts a fresh Codex thread and makes it the single active persisted thread without deleting older threads.
2. `/resume` opens a TUI thread picker. The picker resumes the selected thread and offers deliberate, confirmed actions to delete the selected inactive thread or clear all inactive threads. Deleting the active thread must require switching away from it or creating a replacement explicitly; never silently replace it.
3. A slash command toggles a right-side Thinking panel. It displays only reasoning summaries or thinking text delivered by the installed app-server protocol, with turn scoping and bounded rendering.
4. Codex built-in command-line and file tools are enabled with full local access, as explicitly authorized by the project owner. AgentHarness does not add approval cards in this milestone. Use the app-server policy that permits execution without interactive approvals, and continue to deny any unexpected server request that still arrives.
5. The header shows the authenticated account identity instead of the generic “signed in” label.
6. While a turn is running but no assistant text has arrived, the transcript shows a small animated squiggle. Remove it on the first assistant-text delta and on completion, failure, or cancellation.
7. The top-right cyan header shows the percentage of the model context window remaining whenever app-server token-usage events provide a usable context-window size. Show an honest unknown state rather than inventing a percentage.

## Protocol and safety decisions

- The installed Codex CLI schema remains authoritative. Regenerate it in a temporary directory before implementing thread listing/deletion, reasoning events, tool policy, account identity, or token usage.
- Keep one long-lived app-server connection and exactly one active thread in application state.
- Full tool access is an intentional product-policy change. Keep the dedicated AgentHarness Codex home and dedicated working directory, but do not claim the filesystem or command environment is sandboxed from the user’s machine.
- Do not render hidden chain-of-thought. “Thinking” means only summaries or text explicitly surfaced by app-server.
- Thread deletion is destructive. Require an in-TUI confirmation, identify the exact scope, and preserve the active saved ID unless its deletion was explicitly confirmed as part of a visible replacement flow.
- Continue sanitizing rendered text and diagnostics. Do not log prompts, replies, raw reasoning content, command output, or account secrets.

## Acceptance tests

- Slash parsing and reducer tests for `/new`, the `/resume` picker, selection, cancellation, deletion confirmation, delete failure, clear-all behavior, and saved-ID preservation.
- Fake app-server integration tests for paginated thread listing, resume selection, new-thread creation, single deletion, clear-all sequencing, malformed responses, and partial deletion failure.
- Protocol and reducer tests for reasoning deltas scoped by thread, turn, and item; panel toggling; unknown reasoning events; and terminal turn cleanup.
- Policy tests proving command/file tools are enabled with the intended full-access sandbox and command environment, while unexpected server requests still fail closed.
- Header rendering tests for signed-out and authenticated account identity states.
- Deterministic animation tests proving the squiggle appears before assistant text, advances frames, and disappears on first text or terminal events.
- Token-usage parsing and reducer tests for normal, missing, zero, stale-thread, over-limit, and model-change cases; rendering tests for the top-right percentage and unknown state.
- Cross-feature Ratatui `TestBackend` tests at narrow and normal terminal widths, with and without the Thinking panel.
- All default tests remain offline. The normal format, Clippy, and test checks must pass after every merge and for the final integrated branch.

## Integration order

Develop each numbered capability in its own worktree. Merge thread management, thinking panel, tool enablement, account identity, thinking animation, and context percentage one by one. Resolve reducer, protocol, and TUI conflicts centrally, then run the full suite and an installed-CLI initialization smoke test.
