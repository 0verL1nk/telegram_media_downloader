import { useRef } from "react";
import { TaskContextMenu, TaskOps } from "./TaskOps";
import { VideoTaskThumbnail } from "./VideoTaskThumbnail";
import { Button } from "../ui/button";
import { Checkbox } from "../ui/checkbox";
import { Progress } from "../ui/progress";
import type { DownloadTask, TaskAction } from "../../lib/api";
import { formatBytes, formatDate, formatSpeed, getPercent, remainingLabel, statusLabel, tableSubtitle, taskGlyph } from "../../lib/format";

export type TaskFilter = "all" | "downloading" | "processing" | "queued" | "paused" | "completed" | "failed";

const FILTERS: { id: TaskFilter; label: string }[] = [
  { id: "all", label: "全部" },
  { id: "downloading", label: "下载中" },
  { id: "processing", label: "视频处理中" },
  { id: "queued", label: "队列" },
  { id: "paused", label: "已暂停" },
  { id: "completed", label: "已完成" },
  { id: "failed", label: "失败" },
];

/** 任务全览页(DESIGN.md §1 状态②):54px 行高表格 + 多选 + 键盘操作。 */
export function TaskList({
  tasks,
  counts,
  filter,
  onFilterChange,
  showCovers,
  onToggleCovers,
  selectedId,
  selectedIds,
  onSelect,
  onToggleSelect,
  onToggleSelectAll,
  onAction,
  onOpenFolder,
  onSetCover,
  onProcessVideo,
  onCopyName,
  onDelete,
  onClearFinished,
  onBulkAction,
  onBulkDelete,
  onClearSelection,
}: {
  tasks: DownloadTask[];
  counts: Record<TaskFilter, number>;
  filter: TaskFilter;
  onFilterChange: (filter: TaskFilter) => void;
  showCovers: boolean;
  onToggleCovers: () => void;
  selectedId?: string | null;
  selectedIds: Set<string>;
  onSelect: (task: DownloadTask | null) => void;
  onToggleSelect: (task: DownloadTask) => void;
  onToggleSelectAll: (checked: boolean) => void;
  onAction: (task: DownloadTask, action: TaskAction) => void;
  onOpenFolder: (task: DownloadTask) => void;
  onSetCover: (task: DownloadTask) => void;
  onProcessVideo: (task: DownloadTask) => void;
  onCopyName: (task: DownloadTask) => void;
  onDelete: (task: DownloadTask) => void;
  onClearFinished: () => void;
  onBulkAction: (action: "pause" | "resume" | "retry") => void;
  onBulkDelete: () => void;
  onClearSelection: () => void;
}) {
  const listRef = useRef<HTMLDivElement | null>(null);
  const allSelected = tasks.length > 0 && tasks.every((task) => selectedIds.has(task.taskId));
  const selectedVisible = tasks.filter((task) => selectedIds.has(task.taskId));

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
      if (["downloading", "paused", "queued"].includes(task.status.toLowerCase())) {
        onAction(task, "cancel");
      }
    } else if (event.key === "Escape") {
      onSelect(null);
      onClearSelection();
    }
  }

  return (
    <main className="full">
      <div className="full-head">
        <div className="head-row">
          <h1>任务</h1>
          <Button
            variant="secondary"
            aria-pressed={showCovers}
            onClick={onToggleCovers}
            title={showCovers ? "隐藏视频封面" : "显示视频封面"}
          >
            {showCovers ? "隐藏封面" : "显示封面"}
          </Button>
          <Button
            variant="secondary"
            onClick={onClearFinished}
            disabled={counts.completed + counts.failed === 0}
            title="移除已完成与已取消的任务记录(不删除已下载的文件)"
          >
            清除已完成
          </Button>
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

      {selectedIds.size > 0 ? (
        <div className="bulk-bar">
          <span className="bulk-count">已选 {selectedIds.size} 项</span>
          <span className="bulk-actions">
            {selectedVisible.some((task) => ["downloading", "queued"].includes(task.status.toLowerCase())) ? (
              <Button variant="secondary" onClick={() => onBulkAction("pause")}>暂停</Button>
            ) : null}
            {selectedVisible.some((task) => task.status.toLowerCase() === "paused") ? (
              <Button variant="secondary" onClick={() => onBulkAction("resume")}>继续</Button>
            ) : null}
            {selectedVisible.some((task) => ["failed", "cancelled"].includes(task.status.toLowerCase())) ? (
              <Button variant="secondary" onClick={() => onBulkAction("retry")}>重试</Button>
            ) : null}
            <Button variant="destructive" onClick={onBulkDelete}>删除</Button>
            <Button variant="ghost" onClick={onClearSelection}>取消选择</Button>
          </span>
        </div>
      ) : null}

      <div className="list-wrap">
        <div className="list-head">
          <span className="row-check">
            <Checkbox
              checked={allSelected}
              onCheckedChange={(checked) => onToggleSelectAll(checked === true)}
              aria-label="全选任务"
            />
          </span>
          <span aria-hidden />
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
            const checked = selectedIds.has(task.taskId);
            return (
              <TaskContextMenu key={task.taskId} task={task} onAction={onAction} onOpenFolder={onOpenFolder} onSetCover={onSetCover} onProcessVideo={onProcessVideo} onCopyName={onCopyName} onDelete={onDelete}>
                <div
                  className={"row" + (selectedId === task.taskId ? " selected" : "") + (checked ? " checked" : "")}
                  role="row"
                  tabIndex={-1}
                  aria-selected={selectedId === task.taskId}
                  onClick={() => onSelect(task)}
                >
                  <span className="row-check" onClick={(event) => event.stopPropagation()}>
                    <Checkbox
                      checked={checked}
                      onCheckedChange={() => onToggleSelect(task)}
                      aria-label={`选择 ${task.fileName || "未命名媒体"}`}
                    />
                  </span>
                  <VideoTaskThumbnail task={task} fallback={glyph.glyph} tone={glyph.tone} showCover={showCovers} />
                  <span className="fname">
                    <b title={task.fileName ?? ""}>{task.fileName || "未命名媒体"}</b>
                    <span className={subtitle.failed ? "err" : undefined}>{subtitle.text}</span>
                  </span>
                  <span className="prog-cell">
                    <Progress value={percent} tone={glyph.tone === "ok" ? "success" : glyph.tone === "bad" ? "danger" : "default"} />
                    <span className={"pct" + (glyph.tone === "bad" ? " bad" : "") + (status === "completed" ? " ok" : "")}>
                      {status === "completed" ? "完成" : status === "processing" ? "转码中" : `${percent}%`}
                    </span>
                  </span>
                  <span className="size">{task.totalBytes ? formatBytes(task.totalBytes) : formatBytes(task.downloadedBytes)}</span>
                  <span className="speed">
                    {status === "downloading" ? formatSpeed(task.speedBytesPerSecond) : status === "completed" ? "—" : statusLabel(task.status)}
                    {remaining ? <small>{remaining}</small> : null}
                    {status !== "downloading" && status !== "completed" && task.updatedAt ? <small>{formatDate(task.updatedAt)}</small> : null}
                  </span>
                  <TaskOps task={task} onAction={onAction} onOpenFolder={onOpenFolder} onSetCover={onSetCover} onProcessVideo={onProcessVideo} onDelete={onDelete} />
                </div>
              </TaskContextMenu>
            );
          })}
        </div>
      </div>
    </main>
  );
}
