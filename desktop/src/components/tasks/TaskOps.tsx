import type { ReactNode } from "react";
import { Icon } from "../Icon";
import { ContextMenu, ContextMenuContent, ContextMenuItem, ContextMenuSeparator, ContextMenuTrigger } from "../ui/context-menu";
import type { DownloadTask, TaskAction } from "../../lib/api";
import { actionLabel, getTaskActions, isVideoTask, statusLabel } from "../../lib/format";

const ACTION_ICON: Record<TaskAction, string> = { pause: "pause", resume: "play", cancel: "close", retry: "refresh" };

function iconName(action: TaskAction): string {
  return ACTION_ICON[action];
}

/** 行内操作按钮组:悬停显现(DESIGN.md §3/§4),点击不透传到行选择。 */
export function TaskOps({
  task,
  onAction,
  onOpenFolder,
  onSetCover,
  onDelete,
  compact = false,
}: {
  task: DownloadTask;
  onAction: (task: DownloadTask, action: TaskAction) => void;
  onOpenFolder: (task: DownloadTask) => void;
  onSetCover: (task: DownloadTask) => void;
  onDelete: (task: DownloadTask) => void;
  compact?: boolean;
}) {
  const status = task.status.toLowerCase();
  const actions = getTaskActions(task);
  const done = status === "completed";
  const canSetCover = isVideoTask(task) && ["completed", "downloading", "queued", "paused", "failed", "cancelled"].includes(status);
  const className = compact ? "pr-x" : "op";
  const run = (event: { stopPropagation: () => void }, fn: () => void) => {
    event.stopPropagation();
    fn();
  };
  return (
    <span className="ops" style={compact ? { opacity: 1, gap: 2 } : undefined} onClick={(event) => event.stopPropagation()}>
      {done ? (
        <button type="button" className={className} title="打开文件位置" aria-label="打开文件位置" onClick={(event) => run(event, () => onOpenFolder(task))}>
          <Icon name="folder" size={compact ? 13 : 15} />
        </button>
      ) : null}
      {canSetCover ? (
        <button type="button" className={className} title="选取视频帧作为封面" aria-label={`选取视频帧作为封面${task.fileName ? " " + task.fileName : ""}`} onClick={(event) => run(event, () => onSetCover(task))}>
          <Icon name="image" size={compact ? 13 : 15} />
        </button>
      ) : null}
      {actions.map((action) => (
        <button
          key={action}
          type="button"
          className={className + (action === "cancel" ? " danger" : "")}
          title={actionLabel(action)}
          aria-label={`${actionLabel(action)}${task.fileName ? " " + task.fileName : ""}`}
          onClick={(event) => run(event, () => onAction(task, action))}
        >
          <Icon name={iconName(action)} size={compact ? 13 : 15} />
        </button>
      ))}
      <button
        type="button"
        className={className + " danger"}
        title="删除任务"
        aria-label={`删除任务${task.fileName ? " " + task.fileName : ""}`}
        onClick={(event) => run(event, () => onDelete(task))}
      >
        <Icon name="trash" size={compact ? 13 : 15} />
      </button>
    </span>
  );
}

/** 任务行右键菜单(DESIGN.md §5:暂停/继续/取消/重试/打开位置/复制文件名/删除)。 */
export function TaskContextMenu({
  task,
  onAction,
  onOpenFolder,
  onSetCover,
  onCopyName,
  onDelete,
  children,
}: {
  task: DownloadTask;
  onAction: (task: DownloadTask, action: TaskAction) => void;
  onOpenFolder: (task: DownloadTask) => void;
  onSetCover: (task: DownloadTask) => void;
  onCopyName: (task: DownloadTask) => void;
  onDelete: (task: DownloadTask) => void;
  children: ReactNode;
}) {
  const actions = getTaskActions(task);
  const status = task.status.toLowerCase();
  return (
    <ContextMenu>
      <ContextMenuTrigger asChild>{children}</ContextMenuTrigger>
      <ContextMenuContent>
        <ContextMenuItem disabled>{statusLabel(status)}</ContextMenuItem>
        <ContextMenuSeparator />
        {actions.map((action) => (
          <ContextMenuItem key={action} onSelect={() => onAction(task, action)}>
            <Icon name={iconName(action)} size={14} />{actionLabel(action)}
          </ContextMenuItem>
        ))}
        {status === "completed" ? (
          <ContextMenuItem onSelect={() => onOpenFolder(task)}>
            <Icon name="folder" size={14} />打开文件位置
          </ContextMenuItem>
        ) : null}
        {isVideoTask(task) && ["completed", "downloading", "queued", "paused", "failed", "cancelled"].includes(status) ? (
          <ContextMenuItem onSelect={() => onSetCover(task)}>
            <Icon name="image" size={14} />选取视频帧作为封面
          </ContextMenuItem>
        ) : null}
        <ContextMenuItem onSelect={() => onCopyName(task)}>
          <Icon name="file" size={14} />复制文件名
        </ContextMenuItem>
        <ContextMenuSeparator />
        <ContextMenuItem className="danger" onSelect={() => onDelete(task)}>
          <Icon name="trash" size={14} />删除任务记录
        </ContextMenuItem>
      </ContextMenuContent>
    </ContextMenu>
  );
}
