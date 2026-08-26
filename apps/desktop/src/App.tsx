import { useState } from "react";
import { ArrowRight, CheckCircle2, Circle, GitBranch, HardDrive, LockKeyhole, Server, ShieldCheck, XCircle } from "lucide-react";

type ConnectionStatus = "idle" | "testing" | "success" | "error";
type ConnectionResult = { status: ConnectionStatus; message: string };

const steps = [
  ["连接", "配置源平台和目标平台"],
  ["选择仓库", "筛选、全选并排除例外"],
  ["映射与策略", "命名、模块和冲突规则"],
  ["预检", "权限、目标状态和磁盘检查"],
  ["迁移队列", "暂停、恢复和失败重试"],
  ["报告", "校验结果和本地导出"],
] as const;

const platforms = ["GitHub", "GitLab", "Gitee", "Gitea / Forgejo", "通用 Git"];
const initialResult: ConnectionResult = { status: "idle", message: "" };

function validateEndpoint(value: string): string | null {
  const endpoint = value.trim();
  if (!endpoint) return "请输入服务地址";
  try {
    const url = new URL(endpoint);
    if (!["http:", "https:", "ssh:"].includes(url.protocol)) return "仅支持 HTTP、HTTPS 或 SSH 地址";
    if (!url.hostname) return "服务地址缺少主机名";
    return null;
  } catch {
    return "服务地址格式不正确，例如 https://gitlab.example.com";
  }
}

function ConnectionCard({ kind, defaultPlatform, defaultEndpoint, result, onTest }: {
  kind: "source" | "target";
  defaultPlatform: string;
  defaultEndpoint: string;
  result: ConnectionResult;
  onTest: (platform: string, endpoint: string) => void;
}) {
  const [platform, setPlatform] = useState(defaultPlatform);
  const [endpoint, setEndpoint] = useState(defaultEndpoint);
  const isSource = kind === "source";

  return (
    <section className={`connection-panel ${isSource ? "" : "target"}`}>
      <div className="section-heading">
        <div><h2>{isSource ? "源平台" : "目标平台"}</h2><p>{isSource ? "先连接需要迁出的 Git 服务" : "目标仓库可在预检后自动创建"}</p></div>
        {isSource ? <Server size={20} /> : <HardDrive size={20} />}
      </div>
      <label>平台类型<select value={platform} onChange={(event) => setPlatform(event.target.value)}>{platforms.map((item) => <option key={item}>{item}</option>)}</select></label>
      <label>服务地址<input value={endpoint} onChange={(event) => setEndpoint(event.target.value)} placeholder="https://git.example.com" /></label>
      <label>凭据<select defaultValue="new"><option value="new">添加新凭据...</option></select></label>
      <div className={`hint ${isSource ? "" : "neutral"}`}>
        {isSource ? <LockKeyhole size={15} /> : <CheckCircle2 size={15} />}
        <span>{isSource ? "令牌将保存到 Windows Credential Manager，不写入项目或日志。" : "默认跳过非空目标仓库，不会自动覆盖已有内容。"}</span>
      </div>
      <button className="secondary" onClick={() => onTest(platform, endpoint)} disabled={result.status === "testing"}>{result.status === "testing" ? "测试中..." : "测试连接"}</button>
      {result.status !== "idle" && (
        <div className={`connection-status ${result.status}`} role="status" aria-live="polite">
          {result.status === "success" && <CheckCircle2 size={16} />}
          {result.status === "error" && <XCircle size={16} />}
          {result.status === "testing" && <Circle className="status-spinner" size={16} />}
          <span>{result.message}</span>
        </div>
      )}
    </section>
  );
}

export function App() {
  const [sourceResult, setSourceResult] = useState<ConnectionResult>(initialResult);
  const [targetResult, setTargetResult] = useState<ConnectionResult>(initialResult);

  const testConnection = (side: "source" | "target", platform: string, endpoint: string) => {
    const setResult = side === "source" ? setSourceResult : setTargetResult;
    const error = validateEndpoint(endpoint);
    if (error) {
      setResult({ status: "error", message: error });
      return;
    }
    setResult({ status: "testing", message: `正在检查 ${platform} 地址...` });
    window.setTimeout(() => setResult({ status: "success", message: "地址格式检查通过；真实平台权限探测将在后端接入后执行。" }), 450);
  };

  const canContinue = sourceResult.status === "success" && targetResult.status === "success";

  return (
    <main className="app-shell">
      <aside className="sidebar">
        <div className="brand"><GitBranch size={22} /><span>Git Repo Migrator</span></div>
        <nav aria-label="迁移步骤">{steps.map(([title, description], index) => (
          <button className={index === 0 ? "step active" : "step"} key={title} disabled={index > 0}>
            <span className="step-index">{index === 0 ? <Circle size={11} fill="currentColor" /> : index + 1}</span>
            <span><strong>{title}</strong><small>{description}</small></span>
          </button>
        ))}</nav>
        <div className="privacy"><LockKeyhole size={16} /><span>代码与令牌仅在本机处理</span></div>
      </aside>
      <section className="workspace">
        <header className="topbar"><div><p className="eyebrow">新建迁移</p><h1>连接 Git 平台</h1></div><span className="local-badge"><ShieldCheck size={15} /> 本地模式</span></header>
        <div className="content-grid">
          <ConnectionCard kind="source" defaultPlatform="通用 Git" defaultEndpoint="http://git.zihai.cn" result={sourceResult} onTest={(platform, endpoint) => testConnection("source", platform, endpoint)} />
          <ConnectionCard kind="target" defaultPlatform="Gitee" defaultEndpoint="https://github.com" result={targetResult} onTest={(platform, endpoint) => testConnection("target", platform, endpoint)} />
        </div>
        <section className="support-row"><div><h3>当前支持</h3><p>{platforms.join(" · ")}</p></div><button className="primary" disabled={!canContinue}>继续选择仓库 <ArrowRight size={16} /></button></section>
      </section>
    </main>
  );
}
