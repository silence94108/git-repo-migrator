# 发布检查清单（Windows）

本清单是 Gate 2 的门禁。**任何一项未通过都不得发布**——包括那些 CI 无法自动判定、
只能由人记录结果的项目。清单填好后附在 GitHub Draft Release 上，再把草稿转为正式发布。

版本：`v____`　负责人：`____`　日期：`____`

---

## 1. 自动门禁（由 `windows-ci.yml` 判定）

这些项由 CI 的 `Release gate` job 汇总；它对任何非 `success` 的上游 job 都会失败。

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `cargo test --workspace`（含真实裸仓库 Git 夹具、执行器与工作区安全测试）
- [ ] `npm run typecheck --prefix apps/desktop`
- [ ] `npm run test --prefix apps/desktop`
- [ ] `npm run build --prefix apps/desktop`
- [ ] `npm run e2e:webview`（主流程、恢复/限流、安全边界、100 仓库容量）
- [ ] `npm run e2e:desktop`（打包后的应用，经 WebView2 调试端口驱动真实后端）

> `e2e:desktop` 被 **skip** 不算通过。若 CI 上该 job 未真正执行，必须在第 3 节手工记录实机结果。

## 2. 构建与签名

- [ ] 标签与 `apps/desktop/src-tauri/tauri.conf.json` 中的 `version` 一致（CI 会校验）
- [ ] 凭据录入伴随程序 `git-repo-migrator-credential.exe` 已放入
      `apps/desktop/src-tauri/binaries/`，且打包时带上了
      `--config src-tauri/tauri.bundle.conf.json`（基础配置里**不**声明该资源，
      否则 `cargo build` / `cargo test` 会因为资源缺失而失败）
- [ ] 安装后 `git-repo-migrator-credential.exe` 与主程序位于同一目录
      （`AppState::authorize_connection` 只在该目录查找）
- [ ] 所有 `.msi` / `.exe` 的 `Get-AuthenticodeSignature` 状态为 `Valid`（CI 强制）
- [ ] 签名证书与密码**只**来自 CI secret store；工作树、日志与构建参数中没有出现
- [ ] `SHA256SUMS.txt` 已生成，并与上传的产物逐一对应
- [ ] 更新签名（`TAURI_SIGNING_PRIVATE_KEY`）密钥托管方与轮换责任人已确认：`____`

## 3. Windows 实机验证

CI 只跑 `windows-latest`。以下必须在真实机器上各跑一次并记录结果。

| 项目 | Windows 10 | Windows 11 |
|---|---|---|
| 打包应用 E2E（`npm run e2e:desktop`，4 项全绿） | ☐ | ☑ 2026-08-30，开发机 Windows 11 Home 10.0.26200 |
| 安装 / 卸载 / 重装 | ☐ | ☐ |
| 首次启动（WebView2 Runtime 缺失时的提示）| ☐ | ☐ |
| 连接页「录入令牌」打开控制台窗口并成功写入凭据 | ☐ | ☐ |
| Generic Git 主流程：空仓复用 → 推送 → 校验 → 报告导出 | ☐ | ☐ |
| SSH 认证路径 | ☐ | ☐ |
| 自签名证书 + 指纹固定 | ☐ | ☐ |
| HTTP(S) 代理环境 | ☐ | ☐ |
| 100 仓库批次：暂停 / 恢复 / 取消 | ☐ | ☐ |
| 迁移中强制结束进程后重启，未完成批次可恢复且无重复创建 | ☐ | ☐ |

机器信息：Win10 `____`　Win11 `____`　WebView2 Runtime 版本 `____`

## 4. 安全复核

- [ ] `tests/e2e/security-boundary.spec.ts` 全绿（命令白名单、无 secret 载荷、无外部请求）
- [ ] 生产 bundle 中不含 `__migrationBridge`（E2E 注入点已被编译移除，CI 断言）
- [ ] 从安装包中随机抽取一次迁移的日志与导出文件，人工确认不含令牌、密码、认证头
- [ ] Windows 凭据管理器中的条目命名符合 `git-repo-migrator` / `credential/windows/*`，
      且删除连接后引用失效
- [ ] `Tauri` capabilities（`apps/desktop/src-tauri/capabilities/default.json`）未新增
      shell、fs、http 等能力
- [ ] 崩溃报告 / 诊断日志中不含 `SecretGuard` 内容（`Debug` 输出为 `[REDACTED]`）

## 5. 第三方分发与许可

产品调用用户机器上的 Git；**如果安装包内附带**这些程序，必须先解决许可与分发义务。

**已定决策（2026-08-30，决策人：silence108 委托工程侧决定）：不随包分发
`git.exe` 与 `git-lfs.exe`。** 理由与现状：

- 安装包只含本产品二进制与 WebView2 依赖，不含任何 Git 组件，GPLv2 的
  源码提供义务因此不触发，安装包也显著更小。
- `GitRunner` 从系统 PATH 解析 Git（可执行文件白名单校验）。缺失时批次失败
  为不可重试的 `git.missing`，错误信息直接指引「请安装 Git for Windows 并
  确认 git.exe 在 PATH 中」——不静默、不误报。
- `git-lfs.exe` 缺失时 LFS 模块自动降级为「未迁移」并在报告中如实标注，
  主流程（Git 历史、分支、Tag）不受影响。
- 因此下表中两项「随包分发」均勾「否」，GPLv2 源码渠道与 MIT 许可全文
  两项义务条目不适用（N/A）。

- [x] 是否随包分发 `git.exe`？**☑ 否（依赖用户自行安装）**　☐ 是
- [x] 是否随包分发 `git-lfs.exe`？**☑ 否（缺失时自动降级并如实报告）**　☐ 是
- [x] Git（GPLv2）源码获取渠道已随包提供，或已在关于页给出书面提供承诺
      ——N/A：不分发 Git 二进制，义务不触发
- [x] Git LFS（MIT）版权与许可全文已随包提供——N/A：不分发 git-lfs 二进制
- [ ] WebView2 Runtime 的分发方式已确认（Evergreen Bootstrapper / Fixed Version）
      并符合 Microsoft 分发条款
- [ ] 第三方许可清单（Rust crates 与 npm 依赖）已生成并随包提供
- [ ] 法务/负责人签字：`____`

## 6. 回滚

- [ ] 上一版本安装包与 `SHA256SUMS.txt` 仍可获取，路径：`____`
- [ ] 已验证从本版本降级到上一版本后，`%LOCALAPPDATA%` 中的 `migration-state.sqlite3`
      仍可被旧版本打开，或已提供导出/清理指引
- [ ] 已确认本版本没有引入破坏性的 SQLite schema 变更；若有，回滚步骤为：`____`
- [ ] 发布公告中包含回滚指引与已知问题

## 7. 已知未完成项（发布前必须为空或有明确豁免）

在这一节列出本版本仍未闭合的缺口。**列表非空且无书面豁免时，不得发布。**

| # | 缺口 | 影响 | 豁免人 / 计划 |
|---|---|---|---|
| | | | |

---

## 附：本地复跑门禁

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace

npm ci --prefix apps/desktop
npm run typecheck --prefix apps/desktop
npm run test --prefix apps/desktop
npm run build --prefix apps/desktop

npm ci
npx playwright install chromium
npm run e2e:webview

# 打包后的应用（--no-bundle 只产出可执行文件）
npm run tauri build --prefix apps/desktop -- --no-bundle
cargo build --release -p git-repo-migrator-credential-store --bin git-repo-migrator-credential
$env:E2E_TAURI_BINARY = "target\release\git-repo-migrator-desktop.exe"
npm run e2e:desktop

# 打包（带凭据录入伴随程序）
New-Item -ItemType Directory -Force -Path apps/desktop/src-tauri/binaries | Out-Null
Copy-Item target/release/git-repo-migrator-credential.exe `
  apps/desktop/src-tauri/binaries/git-repo-migrator-credential.exe -Force
npm run tauri build --prefix apps/desktop -- --config src-tauri/tauri.bundle.conf.json
```
