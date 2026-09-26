import { Button } from "../ui/button";
import { Segmented } from "../ui/segmented";
import type { LogEntry } from "../../lib/api";

/** 日志页(DESIGN.md §3 空状态:无插画,中性)。 */
export function LogsPage({
  logs,
  level,
  onLevelChange,
  onRefresh,
  loading,
}: {
  logs: LogEntry[];
  level: string;
  onLevelChange: (level: string) => void;
  onRefresh: () => void;
  loading: boolean;
}) {
  return (
    <main className="logs-page">
      <div className="logs-head">
        <div>
          <h1>日志</h1>
          <div className="tabs">
            <Segmented
              ariaLabel="日志级别"
              value={level}
              onChange={onLevelChange}
              options={[
                { value: "all", label: "全部" },
                { value: "info", label: "信息" },
                { value: "warn", label: "警告" },
                { value: "error", label: "错误" },
              ]}
            />
          </div>
        </div>
        <Button variant="secondary" onClick={onRefresh} disabled={loading}>
          {loading ? "正在刷新…" : "刷新日志"}
        </Button>
      </div>
      <div className="logs-body">
        {logs.length === 0 ? (
          <div className="empty">
            <span className="empty-ic">≡</span>
            <span>{loading ? "正在读取日志…" : "当前级别下没有日志记录"}</span>
          </div>
        ) : logs.map((entry, index) => {
          const levelName = (entry.level ?? "info").toLowerCase();
          return (
            <div className="log-line" key={`${entry.timestamp}-${index}`}>
              <span className="target">{entry.timestamp?.slice(11, 19) ?? "—"}</span>
              <span className={"lvl " + levelName}>{levelName.toUpperCase()}</span>
              <span className="target">{entry.target || "app"}</span>
              <span className="msg">{entry.message}</span>
            </div>
          );
        })}
      </div>
    </main>
  );
}
