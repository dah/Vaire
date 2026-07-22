# AgentHarness interactive runtime milestone

## Status

Complete as of 2026-07-22. This is a historical implementation record alongside the completed MVP plan, not the live roadmap.

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
- Full tool access is an intentional product-policy change. The implemented boundaries are `danger-full-access`, `approval_policy="never"`, and `shell_environment_policy.inherit="all"`; command/file operations execute without an approval prompt.
- The app-server inherits the launch environment after inherited `CODEX_*` variables are removed and the dedicated `CODEX_HOME` is supplied. Codex's default variable-name filtering for `KEY`, `SECRET`, and `TOKEN` is incomplete: values such as `DATABASE_URL` and `SSH_AUTH_SOCK`, macOS Keychain, credential/config files, and authenticated CLIs may remain available to full-access commands.
- The persistent conversation directory is only a starting cwd, and the dedicated Codex home is organizational isolation only. Commands can leave the cwd, access other same-user paths and the network, and potentially reach Codex-owned authentication state.
- Do not render hidden chain-of-thought. “Thinking” means only summaries or text explicitly surfaced by app-server.
- Thread deletion is destructive. Require an in-TUI confirmation, identify the exact scope, and preserve the active saved ID unless its deletion was explicitly confirmed as part of a visible replacement flow.
- Continue sanitizing rendered text and diagnostics. Do not log prompts, replies, raw reasoning content, command output, or account secrets.

## Acceptance tests

The following coverage was implemented and is maintained in the offline default suite:

- Slash parsing and reducer tests for `/new`, the `/resume` picker, selection, cancellation, deletion confirmation, delete failure, clear-all behavior, and saved-ID preservation.
- Fake app-server integration tests for paginated thread listing, resume selection, new-thread creation, single deletion, clear-all sequencing, malformed responses, and partial deletion failure.
- Protocol and reducer tests for reasoning deltas scoped by thread, turn, and item; panel toggling; unknown reasoning events; and terminal turn cleanup.
- Policy tests proving command/file tools are enabled with the intended full-access sandbox and command environment, while unexpected server requests still fail closed.
- Header rendering tests for signed-out and authenticated account identity states.
- Deterministic animation tests proving the squiggle appears before assistant text, advances frames, and disappears on first text or terminal events.
- Token-usage parsing and reducer tests for normal, missing, zero, stale-thread, over-limit, and model-change cases; rendering tests for the top-right percentage and unknown state.
- Cross-feature Ratatui `TestBackend` tests at narrow and normal terminal widths, with and without the Thinking panel.
- All default tests remain offline. Format, strict Clippy, the full test suite, and the explicit installed-CLI initialization smoke were run on the final integrated branch.

## Integration record

Thread management, the Thinking panel, tool enablement, account identity, thinking animation, and the context percentage were developed in six dedicated worktrees. They were merged one by one in that order. Reducer, protocol, TUI, and test conflicts were resolved centrally, followed by cross-feature state and transport regressions.

## Final validation

The required offline checks and explicit installed-CLI initialization smoke are the release gate:

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets
cargo test --test installed_cli_smoke installed_cli_initializes_with_full_access_policy -- --ignored --nocapture
```

Final result on 2026-07-22: format and strict Clippy passed; the offline suite passed with 115 tests and 2 intentionally ignored live smokes; the targeted installed-CLI initialization smoke passed against the installed Codex runtime.
