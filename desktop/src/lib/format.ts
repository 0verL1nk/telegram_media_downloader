import type { DownloadTask, TaskAction } from "./api";

export function formatBytes(value?: number | null): string {
  if (value == null || !Number.isFinite(value)) return "—";
  if (value < 1024) return `${Math.max(0, Math.round(value))} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let size = value / 1024;
  let unit = 0;
  while (size >= 1024 && unit < units.length - 1) {
    size /= 1024;
    unit += 1;
  }
  return `${size.toFixed(size >= 100 ? 0 : 1)} ${units[unit]}`;
}

export function formatSpeed(value?: number | null): string {
  if (!value) return "0 B/s";
  return `${formatBytes(value)}/s`;
}

export function formatDate(value?: string | null): string {
  if (!value) return "—";
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return new Intl.DateTimeFormat("zh-CN", { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" }).format(date);
}

export function getPercent(task: DownloadTask): number {
  if (task.totalBytes && task.totalBytes > 0) return Math.min(100, Math.max(0, (task.downloadedBytes / task.totalBytes) * 100));
  return Math.min(100, Math.max(0, task.progress));
}

export function statusLabel(status: string): string {
  const labels: Record<string, string> = { downloading: "下载中", processing: "处理中", queued: "排队中", paused: "已暂停", completed: "已完成", failed: "失败", cancelled: "已取消" };
  return labels[status.toLowerCase()] ?? status;
}

export function statusTone(status: string): "blue" | "muted" | "amber" | "green" | "red" {
  const tones: Record<string, "blue" | "muted" | "amber" | "green" | "red"> = {
    downloading: "blue", processing: "blue", queued: "muted", paused: "amber", completed: "green", failed: "red", cancelled: "muted",
  };
  return tones[status.toLowerCase()] ?? "muted";
}

const VIDEO_EXTENSIONS = new Set(["3g2", "3gp", "asf", "avi", "divx", "f4v", "flv", "m2ts", "m2v", "m4v", "mkv", "mov", "mp4", "mpe", "mpeg", "mpg", "mts", "mxf", "ogv", "ogg", "qt", "rm", "rmvb", "ts", "vob", "webm", "wmv"]);

export function isVideoTask(task: DownloadTask): boolean {
  const mediaType = task.mediaType?.toLowerCase() ?? "";
  if (mediaType === "video" || mediaType === "animation" || mediaType.startsWith("video/")) return true;
  const extension = task.fileName?.split(".").pop()?.toLowerCase();
  return Boolean(extension && VIDEO_EXTENSIONS.has(extension));
}

/** 任务状态 → 行首图标块(26×26 圆角 8,见 DESIGN.md §3)。 */
export function taskGlyph(task: DownloadTask): { glyph: string; tone: "dl" | "ok" | "bad" } {
  const status = task.status.toLowerCase();
  if (status === "completed") return { glyph: "✓", tone: "ok" };
  if (status === "failed" || status === "cancelled") return { glyph: "✕", tone: "bad" };
  return { glyph: "↓", tone: "dl" };
}

export function getTaskActions(task: DownloadTask): TaskAction[] {
  switch (task.status.toLowerCase()) {
    case "downloading": return ["pause", "cancel"];
    case "paused": return ["resume", "cancel"];
    case "queued": return ["cancel"];
    case "failed":
    case "cancelled": return ["retry"];
    default: return [];
  }
}

export function canProcessVideo(task: DownloadTask): boolean {
  if (task.status.toLowerCase() !== "completed") return false;
  if (["video", "animation"].includes((task.mediaType ?? "").toLowerCase())) return true;
  return /\.(mp4|m4v|mov|mkv|webm|avi)$/i.test(task.fileName ?? task.outputPath ?? "");
}

export function actionLabel(action: TaskAction): string {
  return ({ pause: "暂停", resume: "继续", cancel: "取消", retry: "重试" })[action];
}

/** 剩余时间估算(仅活动任务;数据来自后端 stats 时以任务自身剩余字节估算)。 */
export function remainingLabel(task: DownloadTask): string | null {
  const remaining = task.remainingBytes;
  const speed = task.speedBytesPerSecond;
  if (task.status.toLowerCase() !== "downloading" || !remaining || !speed) return null;
  const seconds = Math.ceil(remaining / speed);
  if (!Number.isFinite(seconds) || seconds <= 0 || seconds > 99 * 3600) return null;
  const minutes = Math.floor(seconds / 60);
  const rest = seconds % 60;
  return minutes > 0 ? `剩余 ${minutes}:${String(rest).padStart(2, "0")}` : `剩余 ${rest}s`;
}

/** 面板行副标题:进度百分比 + 速度,或"完成 · 大小"、失败原因。 */
export function panelSubtitle(task: DownloadTask): { text: string; failed: boolean } {
  const status = task.status.toLowerCase();
  if (status === "downloading") {
    return { text: `${Math.round(getPercent(task))}% · ${formatSpeed(task.speedBytesPerSecond)}`, failed: false };
  }
  if (status === "processing") return { text: "AV1 视频处理中", failed: false };
  if (status === "queued") return { text: "排队中", failed: false };
  if (status === "paused") return { text: `已暂停 · ${Math.round(getPercent(task))}%`, failed: false };
  if (status === "completed") {
    if (task.error) return { text: `完成 · ${task.error}`, failed: true };
    return { text: `完成 · ${formatBytes(task.totalBytes ?? task.downloadedBytes)}`, failed: false };
  }
  return { text: task.error || statusLabel(status), failed: true };
}

/** 全览表格副标题:来源与大小,或失败原因。 */
export function tableSubtitle(task: DownloadTask): { text: string; failed: boolean } {
  const status = task.status.toLowerCase();
  const source = task.chatTitle || (task.fileName ? "" : "Telegram Web");
  if (status === "failed" || status === "cancelled") {
    return { text: task.error || "任务失败,可重试", failed: true };
  }
  if (status === "completed" && task.error) {
    return { text: `已完成 · ${task.error}`, failed: true };
  }
  const size = task.totalBytes ? formatBytes(task.totalBytes) : formatBytes(task.downloadedBytes);
  const parts = [source, size].filter(Boolean);
  return { text: parts.join(" · "), failed: false };
}
