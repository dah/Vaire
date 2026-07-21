# Milestone 0 protocol and safety evidence

Date: 2026-07-21

## Scope decision

Milestone 0 now establishes a conversation-focused safety foundation. It does **not**
require proof that every Codex built-in tool is unavailable. Retained built-in tool
capability is acceptable; AgentHarness still has no tool-card or approval UX.

The tested defaults are defense in depth:

- dedicated owner-only `CODEX_HOME`;
- dedicated empty non-project conversation `cwd`;
- strict, explicit disable overrides for recognized tool-bearing optional features;
- empty MCP configuration where practical;
- read-only sandboxing and disabled sandbox/tool network access;
- `approvalPolicy: "never"`; and
- fail-closed denial of every server-initiated request.

## Installed runtime and generated schema

- Executable: `/opt/homebrew/bin/codex`
- Version: `codex-cli 0.144.6`
- Generation command:
  `codex app-server generate-json-schema --out /private/tmp/agentharness-codex-schema-20260721-impl`
- Generated files: 267
- The schema was generated outside the repository and is not committed.

Selected SHA-256 evidence:

| Schema | SHA-256 |
|---|---|
| `ServerRequest.json` | `7c8a2c6fe03d6afdf8a83f91fa5eb55e7ae630fe2e43a167691ab917dccc9556` |
| `v1/InitializeParams.json` | `4f576f99e285beb28f71f48a72b887c1f517dada86fee348fe2af0a35511de23` |
| `v2/ThreadStartParams.json` | `0f85a05fc8a49b7cf38187506e0a2ea9cd49422fafcec33ab610e0932d2ef145` |
| `v2/TurnStartParams.json` | `c338689f1dac297102114d1ee2a5f45219e751b041479b4cc64ebeb8bd926230` |
| `codex_app_server_protocol.v2.schemas.json` | `2b8d4ffc7b48d0234f74a34cde2163946d4eae672351d3f8f59537e71cfb286a` |

## Protocol findings used by the crate

The stable initialization shape supports client metadata and capabilities. AgentHarness
sets `experimentalApi`, `mcpServerOpenaiFormElicitation`, and
`requestAttestation` to `false`.

`thread/start` exposes `approvalPolicy`, `config`, `cwd`, and `sandbox`.
`turn/start` exposes `approvalPolicy`, `cwd`, and a `sandboxPolicy` whose
read-only variant carries `networkAccess: false`.

The generated `ServerRequest` union contains exactly these ten methods:

1. `item/commandExecution/requestApproval`
2. `item/fileChange/requestApproval`
3. `item/tool/requestUserInput`
4. `mcpServer/elicitation/request`
5. `item/permissions/requestApproval`
6. `item/tool/call`
7. `account/chatgptAuthTokens/refresh`
8. `attestation/generate`
9. `applyPatchApproval`
10. `execCommandApproval`

Safety profile: `conversation-safety/codex-0.144.6-v1`.

| Method | Denial result |
|---|---|
| `item/commandExecution/requestApproval` | `{"result":{"decision":"cancel"}}` |
| `item/fileChange/requestApproval` | `{"result":{"decision":"cancel"}}` |
| `item/tool/requestUserInput` | JSON-RPC error `-32080` |
| `mcpServer/elicitation/request` | `{"result":{"action":"cancel"}}` |
| `item/permissions/requestApproval` | JSON-RPC error `-32080` |
| `item/tool/call` | JSON-RPC error `-32080` |
| `account/chatgptAuthTokens/refresh` | JSON-RPC error `-32080` |
| `attestation/generate` | JSON-RPC error `-32080` |
| `applyPatchApproval` | `{"result":{"decision":"abort"}}` |
| `execCommandApproval` | `{"result":{"decision":"abort"}}` |
| unknown future request | JSON-RPC error `-32601` |

The original string or numeric request ID is preserved in each response. Every
server request marks the connection unusable until restart.

## Runtime validation

The ignored installed-runtime smoke test was run explicitly against
`/opt/homebrew/bin/codex`. It used a temporary dedicated Codex home and empty cwd,
initialized through the production transport, sent `initialized`, called
`config/read`, and confirmed:

- `approval_policy = "never"`
- `sandbox_mode = "read-only"`
- `web_search = "disabled"`

The strict feature and configuration arguments were accepted by `codex-cli 0.144.6`.

The offline fake-child integration test injected all ten generated server requests
and an unknown future request through the production transport. It verified that no
response was accepting/approved, the schema-defined negative responses were used
where available, in-flight work failed, and the connection rejected later client
requests.

Commands run for handoff:

- `cargo fmt --check`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo test --all-targets`
- `AGENTHARNESS_CODEX_BIN=/opt/homebrew/bin/codex cargo test --test installed_cli_smoke -- --ignored --nocapture`

## Determination

**PASS for the revised Milestone 0 protocol and safety foundation on
`codex-cli 0.144.6`.**

This is a tested-version statement, not yet a long-term minimum-version guarantee.
It does not claim that all built-in tool capability is absent, and no authenticated
adversarial conformance proof is required by the revised MVP.

Protocol sources:

- [Official Codex app-server documentation](https://learn.chatgpt.com/docs/app-server)
- [Official Codex configuration reference](https://learn.chatgpt.com/docs/config-file/config-reference)
- Installed generated schema listed above (source of truth for this result)
