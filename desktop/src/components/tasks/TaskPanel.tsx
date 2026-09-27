import type { Ref } from "react";
import { TaskContextMenu, TaskOps } from "./TaskOps";
import { Progress } from "../ui/progress";
import type { DownloadTask, TaskAction } from "../../lib/api";
import { getPercent, panelSubtitle, taskGlyph } from "../../lib/format";

export type PanelFilter = "active" | "completed" | "failed";

/** 分栏右侧任务面板 360px(DESIGN.md §1/§3):图标 + 文件名 + 行内操作 + 进度行。 */
export function TaskPanel({
  tasks,
  filter,
  onFilterChange,
  onSelect,
  onAction,
  onOpenFolder,
  onSetCover,
  onCopyName,
  onDelete,
  onShowAll,
  counts,
  selectedId,
  width,
  ref,
}: {
  tasks: DownloadTask[];
  filter: PanelFilter;
  onFilterChange: (filter: PanelFilter) => void;
  onSelect: (task: DownloadTask) => void;
  onAction: (task: DownloadTask, action: TaskAction) => void;
  onOpenFolder: (task: DownloadTask) => void;
  onSetCover: (task: DownloadTask) => void;
  onCopyName: (task: DownloadTask) => void;
  onDelete: (task: DownloadTask) => void;
  onShowAll: () => void;
  counts: { active: number; completed: number; failed: number };
  selectedId?: string | null;
  width?: number;
  ref?: Ref<HTMLElement>;
}) {
  return (
    <aside className="panel" aria-label="任务面板" ref={ref} style={width ? { width } : undefined}>
      <div className="panel-head">
        <div className="panel-title"><h2>任务</h2></div>
        <div className="panel-tabs" role="tablist" aria-label="任务筛选">
          {([
            ["active", "下载中", counts.active],
            ["completed", "已完成", counts.completed],
            ["failed", "失败", counts.failed],
          ] as const).map(([id, label, count]) => (
            <button
              key={id}
              type="button"
              role="tab"
              aria-selected={filter === id}
              className={"ptab" + (filter === id ? " on" : "")}
              onClick={() => onFilterChange(id)}
            >
              {label} <span className="n">{count}</span>
            </button>
          ))}
        </div>
      </div>
      <div className="panel-list">
        {tasks.length === 0 ? (
          <div className="empty">
            <span className="empty-ic">↓</span>
            <span>在 Telegram 里打开媒体并点亮下载按钮<br />任务会出现在这里</span>
          </div>
        ) : tasks.map((task) => {
          const glyph = taskGlyph(task);
          const subtitle = panelSubtitle(task);
          return (
            <TaskContextMenu key={task.taskId} task={task} onAction={onAction} onOpenFolder={onOpenFolder} onSetCover={onSetCover} onCopyName={onCopyName} onDelete={onDelete}>
              <div
                className={"pr" + (selectedId === task.taskId ? " selected" : "")}
                role="button"
                tabIndex={0}
                aria-label={`${task.fileName || "未命名媒体"},${subtitle.text}`}
                onClick={() => onSelect(task)}
                onKeyDown={(event) => {
                  if (event.key === "Enter" || event.key === " ") {
                    event.preventDefault();
                    onSelect(task);
                  }
                }}
              >
                <span className="pr-top">
                  <span className={"pr-ic " + glyph.tone}>{glyph.glyph}</span>
                  <span className="pr-name" title={task.fileName ?? ""}>{task.fileName || "未命名媒体"}</span>
                  <TaskOps task={task} onAction={onAction} onOpenFolder={onOpenFolder} onSetCover={onSetCover} onDelete={onDelete} compact />
                </span>
                <span className="pr-meta">
                  <Progress
                    size="sm"
                    tone={glyph.tone === "ok" ? "success" : glyph.tone === "bad" ? "danger" : "default"}
                    value={Math.round(getPercent(task))}
                  />
                  <span className={"pr-sub" + (subtitle.failed ? " fail" : "")}>{subtitle.text}</span>
                </span>
              </div>
            </TaskContextMenu>
          );
        })}
      </div>
      <div className="panel-foot">
        <span>{counts.active > 0 ? `${counts.active} 项进行中` : "空闲中"}</span>
        <button type="button" className="link" onClick={onShowAll}>查看全部 ↗</button>
      </div>
    </aside>
  );
}
