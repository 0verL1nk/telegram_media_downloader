import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { Event } from "@tauri-apps/api/event";

export type TaskStatus = "queued" | "downloading" | "paused" | "completed" | "failed" | "cancelled" | string;
export type TaskAction = "pause" | "resume" | "cancel" | "retry";

/** Mirrors `ConcurrencySettings` in the Rust backend. */
export interface ConcurrencySettings {
  maxFiles: number;
  perFileChunks: number;
  adaptive: boolean;
  /** 任务失败后自动重试(指数退避;永久性失败不重试)。 */
  autoRetry: boolean;
  chunkSizeKib: number;
  requestTimeoutSeconds: number;
  retries: number;
  maxBandwidthKib: number;
  /** 后端学习值:实测单路峰值速率(B/s),用于 BDP 分块估算;表单回传不覆盖。 */
  learnedPerStreamBytesPerSecond: number;
}

/** Mirrors `Settings` in the Rust backend: local paths, naming rules and queue limits only. */
export interface Settings {
  dataRoot: string;
  downloadRoot: string;
  pathTemplate: string;
  fileNameTemplate: string;
  dateFormat: string;
  duplicatePolicy: "skip" | "rename" | "overwrite" | string;
  concurrency: ConcurrencySettings;
  language: string;
  preservePartialFiles: boolean;
}

/** Mirrors `StorageOption` in the Rust backend. */
export interface StorageOption {
  path: string;
  label?: string | null;
  availableBytes: number;
  totalBytes?: number | null;
  isSystem: boolean;
}

export interface StorageChangeResult {
  dataRoot: string;
  downloadRoot: string;
  restartRequired: boolean;
}

export interface TelegramWebviewBounds {
  x: number;
  y: number;
  width: number;
  height: number;
}

/** Mirrors `RuntimeStats` in the Rust backend. */
export interface RuntimeStats {
  activeDownloads: number;
  queuedDownloads: number;
  completedDownloads: number;
  failedDownloads: number;
  totalDownloadedBytes: number;
  currentSpeedBytesPerSecond: number;
  estimatedRemainingSeconds?: number | null;
}

export interface AppState {
  appVersion: string;
  settings: Settings;
  storageOptions: StorageOption[];
  stats: RuntimeStats;
}

/** Mirrors `TaskRecord` in the Rust backend. */
export interface DownloadTask {
  taskId: string;
  chatId: string;
  chatTitle?: string | null;
  messageId?: number | null;
  mediaType?: string | null;
  fileName?: string | null;
  status: TaskStatus;
  progress: number;
  downloadedBytes: number;
  totalBytes?: number | null;
  speedBytesPerSecond: number;
  remainingBytes?: number | null;
  startedAt?: string | null;
  updatedAt?: string | null;
  completedAt?: string | null;
  outputPath?: string | null;
  error?: string | null;
  retryCount?: number;
  groupId?: string | null;
}

/** Mirrors `LogEntry` in the Rust backend. */
export interface LogEntry {
  timestamp: string;
  level: "trace" | "debug" | "info" | "warn" | "error" | string;
  target?: string | null;
  message: string;
}

export type AppEvent<T> = (payload: T, event: Event<T>) => void;

export function friendlyError(error: unknown): string {
  if (typeof error === "string") return error;
  if (error instanceof Error) return error.message;
  if (error && typeof error === "object") {
    const maybeMessage = (error as { message?: unknown }).message;
    if (typeof maybeMessage === "string") return maybeMessage;
    const nested = (error as { error?: unknown }).error;
    if (typeof nested === "string") return nested;
  }
  return "操作未能完成，请检查应用状态后重试。";
}

async function call<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(command, args);
  } catch (error) {
    throw new Error(friendlyError(error), { cause: error });
  }
}

export const api = {
  getAppState: () => call<AppState>("get_app_state"),

  pickDirectory: async (title: string): Promise<string | null> => {
    try {
      const { open } = await import("@tauri-apps/plugin-dialog");
      const selected = await open({ multiple: false, directory: true, title });
      return typeof selected === "string" ? selected : null;
    } catch (error) {
      throw new Error(friendlyError(error), { cause: error });
    }
  },

  saveSettings: (settings: Settings) =>
    call<void>("save_settings", { settings }),

  listTasks: (status?: string, limit = 200) =>
    call<DownloadTask[]>("list_tasks", { status, limit }),

  taskAction: (taskId: string, action: TaskAction) =>
    call<DownloadTask>("task_action", { taskId, action }),

  openTaskLocation: (taskId: string) =>
    call<void>("open_task_location", { taskId }),

  deleteTask: (taskId: string, deleteFile = false) =>
    call<void>("delete_task", { taskId, deleteFile }),

  getLogs: (limit = 300) => call<LogEntry[]>("get_logs", { limit }),

  ensureTelegramWebview: () => call<void>("ensure_telegram_webview"),

  setTelegramWebviewVisible: (visible: boolean) =>
    call<void>("set_telegram_webview_visible", { visible }),

  setTelegramWebviewBounds: (bounds: TelegramWebviewBounds) =>
    call<void>("set_telegram_webview_bounds", { ...bounds }),

  changeStorageRoot: (path: string) =>
    call<StorageChangeResult>("change_storage_root", { path }),

  listen: <T>(eventName: string, handler: AppEvent<T>): Promise<UnlistenFn> =>
    listen<T>(eventName, (event) => handler(event.payload, event)),
};
