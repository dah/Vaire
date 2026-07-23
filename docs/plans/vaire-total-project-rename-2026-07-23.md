# Vairë total-project rename

**Status:** Completed — 2026-07-23

**Implementation progress:** Complete. Identity, migration, historical Codex cwd compatibility,
documentation, residue cleanup, RepoPrompt review, and clean final validation all passed. Only the
user-managed checkout-directory rename remains.

## Goal

Rename the complete product from its legacy identity to **Vairë** in human-facing branding and
`vaire` in every technical identifier, while preserving Codex authentication and threads,
OpenRouter credentials and conversations, preferences, diagnostics, and the persistent tool
working directory. The user will rename the checkout directory from `AgentHarness` to `vaire`
manually after this work is complete; this implementation must leave that as the only remaining
rename step.

## Canonical identity

| Concern | Value |
|---|---|
| Human product name | `Vairë` |
| Cargo package, crate, and executable | `vaire` |
| Application Support child | `vaire` |
| Environment prefix | `VAIRE_` |
| Codex client name/title | `vaire` / `Vairë` |
| OpenRouter user agent/title | `vaire/<version>` / `Vairë` |
| Active diagnostics file | `vaire.log` |
| Eventual checkout directory | `/Users/danhancu/Developer/vaire` |

The branded spelling is NFC and uses the precomposed character `ë` (`U+00EB`). OpenRouter receives
the exact UTF-8 bytes `56 61 69 72 C3 AB` for `X-Title`; loopback tests compare bytes rather than
calling an ASCII-only header conversion.

Do not retain a legacy environment-variable alias. Old-name literals may remain only where needed
to find and test the legacy Application Support root, or in documentation that explains migration
and rollback. Generic uses of “harness” may remain only when they are not product branding.

## Background

- `Cargo.toml`, `src/main.rs`, and integration tests currently derive or import the legacy Cargo
  package, crate, and executable identity.
- `src/platform.rs::AppPaths` supplies the common support/runtime roots; `IsolationPaths::prepare`
  derives the Codex home and conversation cwd beneath that runtime root. Changing the support path
  without migration would make an existing user appear signed out with no saved conversations.
- `src/runtime/types.rs::RuntimeConfig::discover()` runs before signal installation, terminal entry,
  backend construction, diagnostics, stores, or provider processes. It is the required migration
  boundary.
- Codex thread discovery validates an exact conversation cwd. Existing inactive threads retain the
  old absolute cwd in app-server metadata, so `/resume` must query both the current and historical
  dedicated cwd while preserving account registration, source filtering, and global resource
  limits.
- The tracked tree contains 176 old-name references across 36 files. There is no configured Git
  remote, release URL, package registry, or CI identity requiring an external mutation.

## Support-root migration contract

Before any component opens or creates application state, inspect these same-parent entries without
following symlinks:

- legacy: `~/Library/Application Support/AgentHarness`
- current: `~/Library/Application Support/vaire`

The closed behavior is:

1. Neither exists: clean first run; create nothing during migration.
2. Only a valid current-user-owned `0700` current root exists: idempotent success.
3. Only a valid current-user-owned `0700` legacy root exists: atomically rename that entire root to
   `vaire` with an exclusive Darwin rename that cannot replace a destination, then sync the parent.
4. Both entries exist in any form: fail closed without changing either.
5. A lone symlink, non-directory, wrong-owner, or non-`0700` root: fail closed without mutation.

The migration must never enumerate, deserialize, copy, log, chmod recursively, or otherwise inspect
descendants. Codex-owned files and persistent working-directory contents are opaque and may contain
arbitrary objects. A directory-sync failure after a verified atomic rename is committed but
durability-unverified; never reverse the rename automatically. The moved historical
`diagnostics/agentharness.log`, if present, stays untouched while new runs write `vaire.log`.

Implement the migration as a focused private platform module using existing storage durability
seams and the existing `libc` dependency. Provide injected home, rename, metadata, and directory-sync
seams where needed so normal tests never mutate real user state or global `HOME`.

This migration is macOS-only, matching the shipped platform contract. The dormant Unix/XDG branch
uses `vaire` for clean future installs but receives no legacy-XDG migration until Linux becomes an
approved platform milestone. Parent durability uses the existing `DirectorySync` contract, which
opens and synchronizes the directory path.

## Approach

1. Add and exhaustively test pre-startup support-root migration in the platform/runtime boundary.
2. Change `AppPaths` to the `vaire` root and permanently configure the historical Codex conversation
   cwd as discovery-only metadata on macOS, independent of whether migration happened this launch;
   never create it or use it for new starts/resumes/turns.
3. Extend Codex thread listing across current and historical cwd filters with shared page/item/text
   limits, exact per-query cwd validation, deduplication, and conflict rejection.
4. Rename Cargo identity, Rust imports, environment variables, Codex/OpenRouter attribution,
   diagnostics, UI copy, fixtures, and tests atomically.
5. Rewrite current and historical documentation so Vairë/`vaire` is the current identity, while
   retaining only explicit migration-source references to the old name.
6. Audit all text, filenames, Cargo metadata, and generated artifacts; review with RepoPrompt; then
   validate from a freshly cleaned Cargo target.

## Work items

### 1. Cargo and identity foundation

- Rename the Cargo package, crate, and implicit binary in `Cargo.toml`; regenerate `Cargo.lock` and
  update `src/main.rs` plus every legacy integration-test import to `vaire::` before adding
  new integration tests, avoiding immediate test-name churn.
- Add package identity coverage for the package, library target, binary target, and
  `CARGO_BIN_EXE_vaire`.
- Pin branded protocol/UI/header assertions to NFC `Vairë` with precomposed `U+00EB`.

### 2. Migration and runtime paths

- Add `src/platform/migration.rs` with no-follow root classification, exclusive same-parent rename,
  post-commit verification, parent sync, closed outcomes/errors, and test injection seams.
- Update `src/platform.rs` to derive `Application Support/vaire`, expose the historical conversation
  cwd internally, and cover all derived paths.
- Update `src/runtime/types.rs` so discovery performs migration before reading `VAIRE_CODEX_BIN` and
  before returning `RuntimeConfig`; preserve plain pre-terminal startup failures.
- Update `src/runtime/build.rs`, `src/runtime/tests/`, `src/storage.rs`, and `src/codex/safety.rs` only
  as needed to attach an optional historical cwd field after `IsolationPaths::prepare()` creates the
  current directories. The migration outcome itself has no backend consumer and is not carried.
- Cover first run, current-only, legacy-only, idempotence, both-root collision, symlink,
  non-directory, owner/mode rejection, rename failure, concurrent winner, sync failure, and a
  small nested opaque sentinel fixture proving descendants are neither inspected nor rewritten.

### 3. Saved Codex thread compatibility

- Update `src/codex/session/threads.rs` to list current and optional historical cwd filters.
- Share the established 256-page, retained-item, retained-text, cursor, and result ceilings across
  the whole operation; query current cwd first, then historical cwd, and fail the complete listing
  explicitly if the shared budget is exhausted rather than returning a silently partial list.
  Scope cursor-cycle detection to each filter.
- Require returned cwd to match the filter used, deduplicate identical IDs, and reject the same ID
  under conflicting cwds.
- Keep account registration and allowed `appServer`/legacy `vscode` source filtering unchanged.
- Add fake-server coverage under `tests/thread_management/` proving registered historical threads
  remain resumable and unregistered or conflicting results remain rejected.

### 4. Remaining technical and provider identity

- Rename the legacy `InitializeParams` constructor and all call sites to
  `InitializeParams::vaire()`, with exact Codex `clientInfo` name `vaire` and title `Vairë` tests.
- Send OpenRouter `User-Agent: vaire/<version>` and raw NFC UTF-8 `X-Title: Vairë` through
  `HeaderValue::from_bytes`; assert exact raw bytes in loopback tests without weakening credential
  redaction or relying on `HeaderValue::to_str()`.
- Replace the legacy Codex-binary override, test inheritance variables, fixture paths, and comments
  with the `VAIRE` forms. Do not accept deprecated aliases.
- Change active diagnostics to `vaire.log`, leaving any migrated historical log opaque.

### 5. Human-facing product and documentation

- Replace all current UI, help, picker, restart, safety, error, and terminal branding with `Vairë`
  while preserving behavior and provider names.
- Update representative rendering/parser assertions for the diacritic.
- Update `README.md`, `AGENTS.md`, completed plans, and the OpenRouter investigation. Preserve all
  full-access warnings, plaintext-credential disclosures, Keychain debt, failure semantics, and
  historical conclusions.
- Prepare repository-qualified documentation paths for the eventual `vaire/` checkout name, even
  though the user will perform that final directory rename manually.
- Explicitly accept the short handoff window in which those prepared documentation paths point at
  the future checkout name while review and validation still run from `AgentHarness`.
- Regenerate `LOC-Doc.md` only from measured final source counts if the new migration module changes
  its documented inventory.

### 6. Residue, review, and validation

- Audit tracked text and filenames for every legacy brand spelling and capitalization. Allow only
  the migration source-root literal, narrowly scoped migration tests, and migration/rollback docs.
- Confirm no old crate import, binary, environment alias, attribution header, active log, UI text,
  package metadata, or current repository-qualified documentation path remains.
- Run RepoPrompt review while the current checkout root is still loaded and fix all must-fix issues.
- Confirm `target/` is ignored, run `cargo clean`, and rebuild generated artifacts rather than
  renaming them.
- Run:

  ```bash
  cargo fmt --check
  cargo clippy --all-targets --all-features -- -D warnings
  cargo test --all-targets
  cargo build --bin vaire
  cargo metadata --no-deps --format-version 1
  git diff --check
  ```

- Verify `target/debug/vaire` exists and is executable, with no old binary or old-package
  fingerprint remaining after the clean rebuild.
- Mark this plan completed only after review and every check passes. Do not commit, stage, push, or
  rename the checkout directory unless the user separately requests it.

## Manual final step

After this implementation is handed back, the user will stop tools that hold the old cwd, verify
that `/Users/danhancu/Developer/vaire` does not exist (including as a dangling symlink), and rename
the checkout directory from `AgentHarness` to `vaire` as a same-parent move. No copy, merge, or
overwrite is safe. The repository content will already expect the final `vaire/` root name.

If both Application Support roots ever exist, the startup error and README direct the operator to
stop all versions and move one complete root aside for inspection. Vairë never chooses, deletes, or
merges roots automatically.

## Open questions

None.

## References

- `Cargo.toml`
- `src/main.rs`
- `src/platform.rs`
- `src/runtime/types.rs`
- `src/runtime/build.rs`
- `src/codex/safety.rs`
- `src/codex/session/threads.rs`
- `src/codex/protocol/initialize_auth.rs`
- `src/openrouter/client.rs`
- `src/storage.rs`
- `README.md`
- `AGENTS.md`
