# git-repo-migrator - Boss State (fallback mode)

## Meta

- mode: fallback
- created: 2026-08-26
- roles: core
- lang: zh
- project_root: D:\Desktop\chang\www\git-repo-migrator
- cli: unavailable (`boss --version` not found)

## Stage Status

| stage | status | updated |
|---|---|---|
| 1 planning | completed | 2026-08-26 |
| 2 review | completed | 2026-08-26 |
| 3 dev+qa | completed | 2026-08-29 |
| 4 deploy | blocked | 2026-08-29 |

## Artifacts

| artifact | stage | status | path |
|---|---|---|---|
| design-brief.md | 1 | done | .boss/git-repo-migrator/design-brief.md |
| prd.md | 1 | done | .boss/git-repo-migrator/prd.md |
| architecture.md | 1 | done | .boss/git-repo-migrator/architecture.md |
| ui-spec.md | 1 | done | .boss/git-repo-migrator/ui-spec.md |
| ui-design.json | 1 | done | .boss/git-repo-migrator/ui-design.json |
| tech-review.md | 2 | done | .boss/git-repo-migrator/tech-review.md |
| tasks.md | 2 | done | .boss/git-repo-migrator/tasks.md |
| waves.json | 2 | done | .boss/git-repo-migrator/waves.json |
| release-checklist.md | 4 | done | docs/release-checklist.md |

## Gates

| gate | when | result | notes |
|---|---|---|---|
| Gate 0 | after dev | passed | cargo workspace tests, strict clippy, TypeScript check, Vite build; real bare-repository Git fixture passed |
| Gate 1 | after QA | passed | 200 Rust tests, 85 renderer tests, 23 browser E2E tests; executor drives real `git.exe` end to end |
| Gate 2 | before release | blocked | 见 Open Gaps：打包应用 E2E、Windows 10/11 实机、签名与分发许可均未执行 |

## Event Log (append-only)

- 2026-08-26 stage-1 started
- 2026-08-26 design-brief.md recorded
- 2026-08-26 Boss CLI unavailable; fallback mode enabled
- 2026-08-26 prd.md recorded
- 2026-08-26 architecture.md recorded; B-1/B-2/B-3 revisions completed
- 2026-08-26 ui-spec.md and ui-design.json recorded; JSON contract validated
- 2026-08-26 stage-1 planning completed
- 2026-08-26 stage-2 review completed; conditional pass, zero critical blockers
- 2026-08-26 tasks.md and waves.json recorded; code dispatch awaits explicit user confirmation
- 2026-08-26 Wave 3 started; platform-core, platform-generic and application artifacts implemented
- 2026-08-26 Wave 3 evidence passed: Generic Git URL/script safety, immutable planning, queue/recovery/report contracts, bare-repository ref migration
- 2026-08-26 Gate 0 passed; Windows bundle icon assets generated, signing/release checks remain in Wave 6
- 2026-08-27 Wave 4 completed; Windows Credential Manager boundary, GitHub/GitLab/Gitea/Forgejo/Gitee adapters, generated IPC contract, fidelity archive and failed-item retry implemented
- 2026-08-27 Wave 4 evidence passed: secret boundary, adapter pagination/permissions/rate limit/version/private refs, IPC drift, platform fidelity, workspace tests, strict clippy, TypeScript check and Vite production build
- 2026-08-28 Wave 5 completed; Tauri command whitelist, SQLite-backed snapshot state source, typed event envelopes and the six-step GUI implemented
- 2026-08-28 Wave 5 evidence passed: 42 desktop crate tests, 104 workspace tests, 82 renderer tests, Rust→TypeScript contract drift guard, strict clippy, rustfmt, typecheck and Vite production build
- 2026-08-28 Wave 5 open gaps recorded (block Wave 6): no stage executor wired to git-runner, no HttpTransport implementation, no in-app path to write a Windows credential, and three test files named in the Wave 2/3 gates were never created
- 2026-08-29 Wave 5 gaps G-1..G-4 closed; see "Gap closure" below
- 2026-08-29 Wave 6 completed; Playwright harness, four E2E specs, Windows CI/release workflows and the release checklist added
- 2026-08-29 Wave 6 evidence passed: rustfmt, strict clippy, 200 Rust tests, typecheck, 85 renderer tests, Vite production build, 23 browser E2E tests
- 2026-08-29 Gate 1 passed; Gate 2 blocked on the items in Open Gaps

## Gap closure (2026-08-29)

| # | Gap | What was built | Evidence |
|---|---|---|---|
| G-1 | No stage executor | `crates/application/src/executor.rs` drives prepare_target → clone --mirror → allowlisted push → LFS/metadata/platform modules → verify → cleanup behind `StageRecorder`/`TargetGateway`/`ModuleGateway` ports; `apps/desktop/src-tauri/src/runner.rs` runs it in a worker pool that `batch_start`/`batch_resume`/`task_retry` start and `batch_cancel` signals | `tests/integration/generic_migration_flow.rs` (13), `runner_tests.rs` (4, real bare repositories reach `succeeded`), `the_queue_commands_drive_the_worker_pool` |
| G-2 | No `HttpTransport` | `crates/http-transport` on reqwest/rustls: resolves `X-Credential-Ref` into a real auth header inside the transport, pins a self-signed certificate without disabling verification, retries only 429/5xx and honours `Retry-After`; wired into API discovery via `apps/desktop/src-tauri/src/discovery.rs` | 14 unit + 6 socket-level tests (`live_transport.rs`), 3 discovery tests |
| G-3 | No in-app credential entry | `git-repo-migrator-credential.exe` console companion reads the token (no echo, typed twice) and writes it straight to Windows Credential Manager; `connection_authorize` passes only a *name* and returns only a reference | `crates/credential-store/src/prompt.rs` (7), `flow_tests.rs` credential-boundary tests (3), `ConnectionView.test.tsx` (3), `security-boundary.spec.ts` |
| G-4 | Missing gate test files | `crates/workspace/tests/workspace_safety.rs` (13), `crates/application/tests/orchestrator_faults.rs` (16), `tests/integration/generic_migration_flow.rs` (13) | all green in `cargo test --workspace` |

Defects found and fixed while closing the gaps:

- `Report::to_json` did not redact; only the CSV path did. Both formats now share one redaction step (CM-004).
- Git rejected `\\?\`-prefixed workspace paths passed as arguments; they are stripped before argv.
- The Git error classifier matched bare `401`/`403`, so a nanosecond timestamp in a temp path turned a disk error into a permanent auth failure. It now matches phrases.
- `Retry-After` was clamped down to the backoff ceiling, i.e. retried *earlier* than the server asked. It is now a floor, bounded by a separate 120 s cap.
- `Orchestrator::retry` could not reopen a completed batch, unlike the SQLite queue it mirrors.

## Open Gaps (blocking Gate 2 / release)

| # | Gap | Evidence | Impact |
|---|---|---|---|
| R-1 | The packaged-application E2E project has never been executed | `tests/e2e/windows-application.desktop.spec.ts` skips without `E2E_TAURI_BINARY`; no Tauri bundle has been built in this environment | The real backend + WebView2 path is unverified. The spec and the CI job exist but are unproven. |
| R-2 | No Windows 10 / Windows 11 hardware run | `docs/release-checklist.md` §3 is empty | SSH, self-signed certificates, proxies and the credential-entry console window are untested on real machines |
| R-3 | Neither workflow has run on GitHub | `.github/workflows/*.yml` added but never triggered | Signing, checksum and artifact steps are unverified; the `release` environment does not exist yet |
| R-4 | Git / Git LFS distribution licensing undecided | `docs/release-checklist.md` §5 unchecked | If the installer ever bundles `git.exe`, GPLv2 source-offer obligations apply |
| R-5 | Platform modules do not execute against a real API | `StageExecutor` `ModuleGateway` defaults to `NoPlatformApi`; only discovery is wired to the adapters | Issues/PR/Wiki/Release migration reports `unsupported` rather than migrating. Honest, but not the PRD's full scope. |
| R-6 | Target creation is not wired to a platform API | `runner::GitTargetGateway::create` returns an actionable refusal | An operator must pre-create an empty target repository; `create` plan rows fail with a next step instead of migrating |
