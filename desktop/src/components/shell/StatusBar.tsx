import type { AppState } from "../../lib/api";
import { formatSpeed } from "../../lib/format";

/** 状态栏 30px(DESIGN.md §1):下载概况 | 托盘与版本。 */
export function StatusBar({ stats, version }: { stats: AppState["stats"] | null; version?: string }) {
  const active = stats?.activeDownloads ?? 0;
  const queued = stats?.queuedDownloads ?? 0;
  const completed = stats?.completedDownloads ?? 0;
  const failed = stats?.failedDownloads ?? 0;
  return (
    <footer className="statusbar">
      <span className="sb-group">
        <span>{active} 项下载中</span>
        <span>·</span>
        <span><b>{formatSpeed(stats?.currentSpeedBytesPerSecond)}</b></span>
        <span>·</span>
        <span>排队 {queued}</span>
        <span>·</span>
        <span>已完成 {completed}</span>
        {failed > 0 ? <><span>·</span><span style={{ color: "var(--danger)" }}>失败 {failed}</span></> : null}
      </span>
      <span className="sb-group">
        <span>托盘运行中 · 关闭窗口下载继续</span>
        <span>·</span>
        <span>v{version ?? "—"}</span>
      </span>
    </footer>
  );
}
