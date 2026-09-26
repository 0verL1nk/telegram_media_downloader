import { useRef } from "react";
import { TaskContextMenu, TaskOps } from "./TaskOps";
import { Progress } from "../ui/progress";
import type { DownloadTask, TaskAction } from "../../lib/api";
import { formatBytes, formatDate, formatSpeed, getPercent, remainingLabel, statusLabel, tableSubtitle, taskGlyph } from "../../lib/format";

export type TaskFilter = "all" | "downloading" | "queued" | "paused" | "completed" | "failed";

const FILTERS: { id: TaskFilter; label: string }[] = [
  { id: "all", label: "全部" },
  { id: "downloading", label: "下载中" },
  { id: "queued", label: "队列" },
  { id: "paused", label: "已暂停" },
  { id: "completed", label: "已完成" },
  { id: "failed", label: "失败" },
];

/** 任务全览页(DESIGN.md §1 状态②):54px 行高表格 + 键盘操作。 */
export function TaskList({
  tasks,
  counts,
  filter,
  onFilterChange,
  selectedId,
  onSelect,
  onAction,
  onOpenFolder,
  onCopyName,
}: {
  tasks: DownloadTask[];
  counts: Record<TaskFilter, number>;
  filter: TaskFilter;
  onFilterChange: (filter: TaskFilter) => void;
  selectedId?: string | null;
  onSelect: (task: DownloadTask | null) => void;
  onAction: (task: DownloadTask, action: TaskAction) => void;
  onOpenFolder: (task: DownloadTask) => void;
  onCopyName: (task: DownloadTask) => void;
}) {
  const listRef = useRef<HTMLDivElement | null>(null);

  function handleKeyDown(event: React.KeyboardEvent<HTMLDivElement>) {
    const index = tasks.findIndex((task) => task.taskId === selectedId);
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      const delta = event.key === "ArrowDown" ? 1 : -1;
      const next = tasks[Math.min(tasks.length - 1, Math.max(0, index + delta))] ?? tasks[0];
      if (next) onSelect(next);
    } else if (event.key === "Enter" && index >= 0) {
      event.preventDefault();
      const task = tasks[index];
      if (task.status.toLowerCase() === "completed") onOpenFolder(task);
    } else if (event.key === "Delete" && index >= 0) {
      event.preventDefault();
      const task = tasks[index];
      if (task.status.toLowerCase() === "downloading" || task.status.toLowerCase() === "paused" || task.status.toLowerCase() === "queued") {
        onAction(task, "cancel");
      }
    } else if (event.key === "Escape") {
      onSelect(null);
    }
  }

  return (
    <main className="full">
      <div className="full-head">
        <div className="head-row">
          <h1>任务</h1>
        </div>
        <div className="tabs" role="tablist" aria-label="任务筛选">
          {FILTERS.map((entry) => (
            <button
              key={entry.id}
              type="button"
              role="tab"
              aria-selected={filter === entry.id}
              className={"tab" + (filter === entry.id ? " on" : "")}
              onClick={() => onFilterChange(entry.id)}
            >
              {entry.label} <span className="n">{counts[entry.id] ?? 0}</span>
            </button>
          ))}
        </div>
      </div>
      <div className="list-wrap">
        <div className="list-head" aria-hidden>
          <span />
          <span>文件</span>
          <span>进度</span>
          <span>大小</span>
          <span>速度</span>
          <span style={{ textAlign: "right" }}>操作</span>
        </div>
        <div className="list-body" ref={listRef} tabIndex={0} role="grid" aria-label="下载任务" onKeyDown={handleKeyDown}>
          {tasks.length === 0 ? (
            <div className="empty">
              <span className="empty-ic">↓</span>
              <span>这里还没有任务<br />打开 Telegram 媒体并点击下载按钮就会出现在这里</span>
            </div>
          ) : tasks.map((task) => {
            const status = task.status.toLowerCase();
            const glyph = taskGlyph(task);
            const subtitle = tableSubtitle(task);
            const percent = Math.round(getPercent(task));
            const remaining = remainingLabel(task);
            return (
              <TaskContextMenu key={task.taskId} task={task} onAction={onAction} onOpenFolder={onOpenFolder} onCopyName={onCopyName}>
                <div
                  className={"row" + (selectedId === task.taskId ? " selected" : "")}
                  role="row"
                  tabIndex={-1}
                  aria-selected={selectedId === task.taskId}
                  onClick={() => onSelect(task)}
                >
                  <span className={"st " + glyph.tone}>{glyph.glyph}</span>
                  <span className="fname">
                    <b title={task.fileName ?? ""}>{task.fileName || "未命名媒体"}</b>
                    <span className={subtitle.failed ? "err" : undefined}>{subtitle.text}</span>
                  </span>
                  <span className="prog-cell">
                    <Progress value={percent} tone={glyph.tone === "ok" ? "success" : glyph.tone === "bad" ? "danger" : "default"} />
                    <span className={"pct" + (glyph.tone === "bad" ? " bad" : "") + (status === "completed" ? " ok" : "")}>
                      {status === "completed" ? "完成" : `${percent}%`}
                    </span>
                  </span>
                  <span className="size">{task.totalBytes ? formatBytes(task.totalBytes) : formatBytes(task.downloadedBytes)}</span>
                  <span className="speed">
                    {status === "downloading" ? formatSpeed(task.speedBytesPerSecond) : status === "completed" ? "—" : statusLabel(task.status)}
                    {remaining ? <small>{remaining}</small> : null}
                    {status !== "downloading" && status !== "completed" && task.updatedAt ? <small>{formatDate(task.updatedAt)}</small> : null}
                  </span>
                  <TaskOps task={task} onAction={onAction} onOpenFolder={onOpenFolder} />
                </div>
              </TaskContextMenu>
            );
          })}
        </div>
      </div>
    </main>
  );
}
