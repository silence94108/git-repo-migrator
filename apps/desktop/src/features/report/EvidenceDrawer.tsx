/**
 * Evidence drawer for one report row.
 *
 * This is where "why is this only partial?" gets answered: ref counts, LFS
 * counts, which refs were deliberately excluded, which platform fields could not
 * be mapped, and where the read-only archive was written.
 */

import { Alert, Badge, Drawer, FidelityBadge, ResultBadge } from "../../components/primitives";
import type { ReportRowSnapshot } from "../../state/ipcTypes";

export function EvidenceDrawer({
  row,
  onClose,
  onRetry,
}: {
  row: ReportRowSnapshot | null;
  onClose: () => void;
  onRetry: () => void;
}) {
  return (
    <Drawer
      open={row !== null}
      title={row ? `证据详情：${row.target_url}` : "证据详情"}
      onClose={onClose}
    >
      {row ? (
        <>
          <div className="button-row">
            <ResultBadge status={row.status} />
            {row.error_code ? <Badge tone="error" label={row.error_code} /> : null}
          </div>

          <h3>Git 与 LFS 校验</h3>
          <dl className="definition-list">
            <dt>源地址</dt>
            <dd className="mono">{row.source_url}</dd>
            <dt>目标地址</dt>
            <dd className="mono">{row.target_url}</dd>
            <dt>引用比对</dt>
            <dd>
              已校验 {row.evidence.refs_checked} 个，缺失 {row.evidence.refs_missing} 个
              {row.git_verified ? "" : "（Git 校验未通过）"}
            </dd>
            <dt>LFS 对象</dt>
            <dd>
              已校验 {row.evidence.lfs_checked} 个，缺失 {row.evidence.lfs_missing} 个
              {row.lfs_verified ? "" : "（LFS 校验未通过）"}
            </dd>
            <dt>元数据</dt>
            <dd>{row.metadata_verified ? "已校验一致" : "未校验或不一致"}</dd>
          </dl>

          <h3>按策略排除的引用</h3>
          {row.evidence.excluded_refs.length === 0 ? (
            <p className="caption">没有被排除的引用。</p>
          ) : (
            <ul className="log-list">
              {row.evidence.excluded_refs.map((ref) => (
                <li className="log-entry mono" key={ref}>
                  {ref}
                </li>
              ))}
            </ul>
          )}

          <h3>平台模块保真度</h3>
          {row.modules.length === 0 ? (
            <p className="caption">本次只迁移 Git 数据，未选择平台数据模块。</p>
          ) : (
            <ul className="log-list">
              {row.modules.map((module) => (
                <li className="log-entry" key={module.module}>
                  <span className="button-row">
                    <span>{module.module}</span>
                    <FidelityBadge fidelity={module.fidelity} reason={module.reason} />
                  </span>
                  {module.reason ? <span>{module.reason}</span> : null}
                </li>
              ))}
            </ul>
          )}

          {row.archive_path ? (
            <Alert
              tone="warning"
              title="该仓库的平台数据保存为只读归档"
              action="归档保存在本机，不会在目标平台呈现为可交互的 Issue 或 PR。"
            >
              <p className="mono">{row.archive_path}</p>
            </Alert>
          ) : null}

          {row.unmapped_fields.length > 0 ? (
            <Alert
              tone="warning"
              title={`${row.unmapped_fields.length} 个字段无法映射到目标平台`}
              action="这些字段保留在归档和报告中，未写入目标。"
            >
              <p className="mono">{row.unmapped_fields.join(" · ")}</p>
            </Alert>
          ) : null}

          {row.source_links.length > 0 ? (
            <>
              <h3>源链接</h3>
              <ul className="log-list">
                {row.source_links.map((link) => (
                  <li className="log-entry mono" key={link}>
                    {link}
                  </li>
                ))}
              </ul>
            </>
          ) : null}

          {row.next_action ? (
            <Alert tone="info" title="建议动作" action={row.next_action} />
          ) : null}

          {row.status === "retryable_failed" ? (
            <button type="button" className="button button-primary" onClick={onRetry}>
              回到队列重试该仓库
            </button>
          ) : null}
        </>
      ) : null}
    </Drawer>
  );
}
