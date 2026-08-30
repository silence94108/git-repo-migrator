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
- 2026-08-30 Open Gaps R-4..R-10 closed (see "Gap closure 2" below); Gate 2 remains blocked only on R-1/R-2/R-3 (packaged-app E2E run, physical Windows hardware run, first GitHub workflow run)
- 2026-08-30 licensing decision recorded in docs/release-checklist.md §5: the installer bundles neither git.exe nor git-lfs.exe (missing Git fails with an actionable `git.missing`, missing git-lfs degrades the LFS module honestly), so GPLv2 source-offer obligations are not triggered
- 2026-08-30 R-1 closed: the packaged application was built (`tauri build --no-bundle --config src-tauri/tauri.e2e.conf.json`, which adds the WebView2 debugging port) and the desktop E2E project ran for the first time — 4/4 passed on this Windows 11 machine (startup + snapshot, renderer capability surface, command whitelist rejection, SQLite persistence across restart)
- 2026-08-30 P0 defect found by that first run and fixed: the production IPC bridge double-wrapped every payload (`{ input: { input: {...} } }`), so **every** payload-carrying command in the packaged app failed against the commands' `deny_unknown_fields` structs while the whole jsdom suite stayed green (the in-memory test double never exercised the wrapping). Guard test added (`ipcClient.test.ts`); the E2E spec also gained real user-data-folder isolation (`WEBVIEW2_USER_DATA_FOLDER`) and serial execution (one CDP port), and the CI workflow's desktop job was corrected (missing `e2e:desktop` script, wrong binary path, missing E2E window config)
- 2026-08-30 R-3 (CI half) closed: `windows-ci.yml` runs green end to end on GitHub — all five jobs pass, including the packaged-application E2E on the runner. Two more defects it flushed out and that were fixed: the E2E binary path was relative to `apps/desktop` while cargo puts the workspace binary in the repo root's `target/`; and `tauri.bundle.conf.json` shipped the credential companion via `resources` with a name that never existed on disk (the staged file carries the target triple), so **the installer build had never succeeded anywhere** — now `externalBin`, verified by extracting the MSI and finding `git-repo-migrator-credential.exe` next to the app. The CI now also uploads `windows-installer-unsigned` (MSI + NSIS) as an artifact for manual hardware testing; Playwright failures are surfaced as GitHub annotations (`annotate-failures.mjs`) so the reason is readable without downloading artifacts. Remaining for R-3: `windows-release.yml` still needs its first tag run (requires the signing secrets and the `release` environment to exist)

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

## Gap closure 2 (2026-08-30)

| # | Gap | What was built | Evidence |
|---|---|---|---|
| R-5 | Platform modules never ran a real API | `platform_gateway.rs` `ApiModuleGateway` drives the real adapters with source+target contexts inside the worker pool; issues/metadata migrate natively where the capability matrix says so, same-platform → native rebuild, cross-platform → archive | `cargo test --workspace` green; runner wiring exercised by desktop crate tests |
| R-6 | Target creation not wired to an API | `ApiTargetGateway::create` calls the target platform adapter's `create_repository` (visibility Private, initialize false, idempotency = task id) whenever the target session has an adapter; generic/unknown targets keep the honest ls-remote probe + actionable refusal | desktop crate tests; `runner_tests.rs` |
| R-7 | `connection_test` made no network call | `ApiConnectionTester` probes the real endpoint through the platform adapter with the stored credential; wrong token / unreachable instance / insufficient scope surface at test time | discovery tests |
| R-8 | `workspace_policy` validated but never read | persisted on the batch row (migration 0002, schema v2), read back by the worker per batch and injected via `with_workspace_policy`; renderer exposes the radio group on the mapping page and shows the live value on the preflight summary | `flow_tests.rs` validation tests, `PlanningViews.test.tsx` (selection, summary, `batch_start` payload) |
| R-9 | No LFS success-path test | real end-to-end test with git-lfs 3.7.1: fixture pushes one LFS object, executor runs `git-lfs fetch --all` + `push --all`, verification clone restores the original bytes | `tests/integration/generic_migration_flow.rs` `lfs_objects_travel_to_the_target_when_the_tool_is_present` |
| R-10 | Archive/cleanup states were dead types | executor produces `ArchiveDocument`s and persists them under `archives/<batch>/<task>/`; `AppRecorder` records the real archive path, unmapped fields and retained-temp-directory outcomes | `generic_migration_flow.rs` archive assertions |
| R-4 | Git / Git LFS licensing undecided | decision recorded in `docs/release-checklist.md` §5: bundle neither binary | checklist §5 filled |

Defects found and fixed while closing the gaps:

- `lfs_stage` passed a `lfs` subcommand prefix to the **git-lfs executable** (`run_lfs` invokes `git-lfs` directly, not `git lfs`) — every LFS push had been failing with exit 127. LFS push never worked before this fix.
- Local absolute-path targets must be converted to `file://` URLs for git-lfs ("no valid file:// URLs found" otherwise).
- `persist_archives` handed a full file path to `workspace.child()`; `create_dir_all` turned `issues.json` into a *directory* and the subsequent write failed with os error 5.
- `ArchiveDocument` moved from `application` to `platform-core` so adapters can produce archive documents without depending on the application layer.

## Open Gaps (blocking Gate 2 / release)

| # | Gap | Evidence | Impact |
|---|---|---|---|
| R-2 | No Windows 10 / Windows 11 hardware run | `docs/release-checklist.md` §3 is empty | SSH, self-signed certificates, proxies and the credential-entry console window are untested on real machines |
| R-3 | Neither workflow has run on GitHub | `.github/workflows/*.yml` added but never triggered | Signing, checksum and artifact steps are unverified; the `release` environment does not exist yet |
