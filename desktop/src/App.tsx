import { startTransition, useEffect, useMemo, useRef, useState } from "react";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { check, type Update } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";
import {
  api,
  friendlyError,
  type AppState,
  type DownloadTask,
  type LogEntry,
  type Settings,
  type TaskAction,
} from "./lib/api";
import { Icon } from "./components/Icon";
import { Button } from "./components/ui/button";
import { AlertDialog, AlertDialogContent, AlertDialogDescription, AlertDialogTitle } from "./components/ui/alert-dialog";
import { Switch } from "./components/ui/switch";
import { TooltipProvider } from "./components/ui/tooltip";
import { Rail, type AppView } from "./components/shell/Rail";
import { TitleBar } from "./components/shell/TitleBar";
import { StatusBar } from "./components/shell/StatusBar";
import { TaskPanel, type PanelFilter } from "./components/tasks/TaskPanel";
import { TaskList, type TaskFilter } from "./components/tasks/TaskList";
import { LogsPage } from "./components/logs/LogsPage";
import { SettingsPage, type ThemePreference, type UpdateUiState } from "./components/settings/SettingsPage";
import { actionLabel, getTaskActions } from "./lib/format";

type Toast = { kind: "success" | "error" | "info"; message: string };
type UpdatePhase = "idle" | "checking" | "current" | "available" | "installing" | "ready";

const PANEL_MIN = 280;
const PANEL_MAX = 560;
const PANEL_COLLAPSE = 240;
const PREFS_KEY = "tmd.ui.v1";
const THEME_KEY = "tmd.theme.v1";

const VIEW_LABELS: Record<AppView, string> = {
  telegram: "Telegram Web",
  tasks: "任务",
  logs: "日志",
  settings: "设置",
};

type UiPrefs = { view: AppView; panelWidth: number };

function loadPrefs(): UiPrefs {
  try {
    const raw = localStorage.getItem(PREFS_KEY);
    if (!raw) return { view: "telegram", panelWidth: 360 };
    const parsed = JSON.parse(raw) as Partial<UiPrefs>;
    const view: AppView = ["telegram", "tasks", "logs", "settings"].includes(String(parsed.view)) ? (parsed.view as AppView) : "telegram";
    const width = typeof parsed.panelWidth === "number" && Number.isFinite(parsed.panelWidth)
      ? Math.min(PANEL_MAX, Math.max(PANEL_MIN, parsed.panelWidth))
      : 360;
    return { view, panelWidth: width };
  } catch {
    return { view: "telegram", panelWidth: 360 };
  }
}

function savePrefs(prefs: Partial<UiPrefs>) {
  try {
    localStorage.setItem(PREFS_KEY, JSON.stringify({ ...loadPrefs(), ...prefs }));
  } catch {
    /* 隐私模式等场景下静默忽略 */
  }
}

function loadTheme(): ThemePreference {
  try {
    const raw = localStorage.getItem(THEME_KEY);
    return raw === "light" || raw === "dark" ? raw : "system";
  } catch {
    return "system";
  }
}

function App() {
  const initialPrefs = useRef(loadPrefs());
  const [view, setView] = useState<AppView>(initialPrefs.current.view);
  const viewRef = useRef<AppView>(initialPrefs.current.view);
  const [panelWidth, setPanelWidth] = useState(initialPrefs.current.panelWidth);
  const [panelFilter, setPanelFilter] = useState<PanelFilter>("active");
  const [theme, setTheme] = useState<ThemePreference>(loadTheme);
  const [appState, setAppState] = useState<AppState | null>(null);
  const [editableSettings, setEditableSettings] = useState<Settings | null>(null);
  const [tasks, setTasks] = useState<DownloadTask[]>([]);
  const [logs, setLogs] = useState<LogEntry[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [selectedIds, setSelectedIds] = useState<Set<string>>(() => new Set());
  const [webviewEmbeddedReady, setWebviewEmbeddedReady] = useState(false);
  const [loading, setLoading] = useState(true);
  const [logsLoading, setLogsLoading] = useState(false);
  const [taskFilter, setTaskFilter] = useState<TaskFilter>("all");
  const [toast, setToast] = useState<Toast | null>(null);
  const [busy, setBusy] = useState<Record<string, boolean>>({});
  const [fatalError, setFatalError] = useState("");
  const [settingsSaved, setSettingsSaved] = useState(false);
  const [storageTarget, setStorageTarget] = useState("");
  const [storageConfirm, setStorageConfirm] = useState(false);
  const [deleteTarget, setDeleteTarget] = useState<
    { kind: "single"; task: DownloadTask } | { kind: "selection"; tasks: DownloadTask[] } | { kind: "clearFinished" } | null
  >(null);
  const [deleteFile, setDeleteFile] = useState(false);
  const [logLevel, setLogLevel] = useState("all");
  const [webviewError, setWebviewError] = useState("");
  const [windowMaximized, setWindowMaximized] = useState(false);
  const [draggingPanel, setDraggingPanel] = useState(false);
  const [updatePhase, setUpdatePhase] = useState<UpdatePhase>("idle");
  const [updateRelease, setUpdateRelease] = useState<{ version: string } | null>(null);
  const [updateProgress, setUpdateProgress] = useState(0);
  const pendingUpdateRef = useRef<Update | null>(null);
  const toastTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const nativeWindow = getCurrentWindow();
  const telegramWebviewRef = useRef<HTMLDivElement | null>(null);
  const splitAreaRef = useRef<HTMLDivElement | null>(null);
  const panelRef = useRef<HTMLElement | null>(null);
  const dragStateRef = useRef<{ startX: number; startWidth: number } | null>(null);
  const syncTelegramWebviewRef = useRef<(() => Promise<void>) | null>(null);

  const stats = appState?.stats ?? null;
  const webviewHostMounted = appState !== null;
  const availableVolumes = appState?.storageOptions ?? [];
  const targetIsSystemVolume = availableVolumes.some((volume) => {
    if (!volume.isSystem || !storageTarget) return false;
    const root = volume.path.replace(/[\\/]+$/, "").toLocaleLowerCase();
    const target = storageTarget.replace(/[\\/]+$/, "").toLocaleLowerCase();
    return target === root || target.startsWith(`${root}\\`) || target.startsWith(`${root}/`);
  });

  const counts = useMemo(() => {
    const count = (status: string) => tasks.filter((task) => task.status.toLowerCase() === status).length;
    return {
      active: count("downloading") + count("queued") + count("paused"),
      completed: count("completed"),
      failed: count("failed") + count("cancelled"),
      downloading: count("downloading"),
      queued: count("queued"),
      paused: count("paused"),
      all: tasks.length,
    };
  }, [tasks]);

  const visibleTasks = useMemo(
    () => tasks.filter((task) => taskFilter === "all" || task.status.toLowerCase() === taskFilter),
    [tasks, taskFilter],
  );
  const panelTasks = useMemo(() => {
    const match = (task: DownloadTask) => {
      const status = task.status.toLowerCase();
      if (panelFilter === "active") return status === "downloading" || status === "queued" || status === "paused";
      if (panelFilter === "completed") return status === "completed";
      return status === "failed" || status === "cancelled";
    };
    return tasks.filter(match).slice(0, 60);
  }, [tasks, panelFilter]);
  const filteredLogs = useMemo(
    () => logs.filter((item) => logLevel === "all" || (item.level ?? "info").toLowerCase() === logLevel),
    [logs, logLevel],
  );
  const selectedTask = useMemo(() => tasks.find((task) => task.taskId === selectedId) ?? null, [tasks, selectedId]);

  function notify(message: string, kind: Toast["kind"] = "success") {
    setToast({ message, kind });
    if (toastTimer.current) clearTimeout(toastTimer.current);
    toastTimer.current = setTimeout(() => setToast(null), 4200);
  }

  function navigate(next: AppView) {
    viewRef.current = next;
    setView(next);
    savePrefs({ view: next });
    if (next === "logs" && logs.length === 0) void refreshLogs();
    if (next === "tasks") void refreshTasks();
  }

  async function runBusy<T>(key: string, action: () => Promise<T>, success?: string): Promise<T | undefined> {
    setBusy((current) => ({ ...current, [key]: true }));
    try {
      const result = await action();
      if (success) notify(success);
      return result;
    } catch (error) {
      notify(friendlyError(error), "error");
      return undefined;
    } finally {
      setBusy((current) => ({ ...current, [key]: false }));
    }
  }

  async function loadState() {
    const result = await api.getAppState();
    setAppState(result);
    setEditableSettings(structuredClone(result.settings));
    setStorageTarget(result.settings.dataRoot);
    setFatalError("");
  }

  async function refreshTasks() {
    try {
      const rows = await api.listTasks(undefined, 300);
      setTasks(rows);
      setSelectedIds((current) => {
        if (current.size === 0) return current;
        const alive = new Set(rows.map((task) => task.taskId));
        const next = new Set([...current].filter((id) => alive.has(id)));
        return next.size === current.size ? current : next;
      });
    } catch (error) {
      notify(`读取下载任务失败:${friendlyError(error)}`, "error");
    }
  }

  async function refreshLogs() {
    setLogsLoading(true);
    try {
      setLogs(await api.getLogs(500));
    } catch (error) {
      notify(`读取日志失败:${friendlyError(error)}`, "error");
    } finally {
      setLogsLoading(false);
    }
  }

  async function refreshAll() {
    setLoading(true);
    try {
      await Promise.all([loadState(), refreshTasks(), refreshLogs()]);
    } catch (error) {
      setFatalError(friendlyError(error));
    } finally {
      setLoading(false);
    }
  }

  function mergeTaskUpdate(payload: unknown) {
    const candidate = payload && typeof payload === "object" && "task" in payload
      ? (payload as { task: DownloadTask }).task
      : (payload as DownloadTask);
    if (!candidate || typeof candidate.taskId !== "string") {
      void refreshTasks();
      return;
    }
    startTransition(() => {
      setTasks((current) => {
        const next = current.some((task) => task.taskId === candidate.taskId)
          ? current.map((task) => (task.taskId === candidate.taskId ? candidate : task))
          : [candidate, ...current];
        return next.slice(0, 300);
      });
    });
  }

  // 主题:显式解析"跟随系统"(data-theme 永远有值),并监听系统主题变化。
  useEffect(() => {
    const media = window.matchMedia("(prefers-color-scheme: dark)");
    const apply = () => {
      const resolved = theme === "system" ? (media.matches ? "dark" : "light") : theme;
      document.documentElement.dataset.theme = resolved;
    };
    apply();
    if (theme !== "system") {
      try {
        localStorage.setItem(THEME_KEY, theme);
      } catch {
        /* 忽略存储失败 */
      }
      return;
    }
    try {
      localStorage.removeItem(THEME_KEY);
    } catch {
      /* 忽略存储失败 */
    }
    media.addEventListener("change", apply);
    return () => media.removeEventListener("change", apply);
  }, [theme]);

  // 窗口最大化状态(标题栏按钮图标)。
  useEffect(() => {
    let active = true;
    let stopResize: UnlistenFn | undefined;
    void nativeWindow.isMaximized().then((maximized) => {
      if (active) setWindowMaximized(maximized);
    }).catch(() => undefined);
    void nativeWindow.onResized(() => {
      void nativeWindow.isMaximized().then((maximized) => {
        if (active) setWindowMaximized(maximized);
      }).catch(() => undefined);
    }).then((unlisten) => {
      if (active) stopResize = unlisten;
      else unlisten();
    }).catch(() => undefined);
    return () => {
      active = false;
      stopResize?.();
    };
  }, [nativeWindow]);

  // 全局快捷键:Ctrl+1..4 切视图 / F5 刷新 / Esc 取消选择。
  useEffect(() => {
    function onKeyDown(event: KeyboardEvent) {
      const target = event.target;
      const typing = target instanceof HTMLElement && (target.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(target.tagName));
      if (event.key === "Escape") {
        setSelectedId(null);
        return;
      }
      if (typing) return;
      if ((event.ctrlKey || event.metaKey) && ["1", "2", "3", "4"].includes(event.key)) {
        event.preventDefault();
        const destination: AppView = event.key === "1" ? "telegram" : event.key === "2" ? "tasks" : event.key === "3" ? "logs" : "settings";
        navigate(destination);
      } else if (event.key === "F5") {
        event.preventDefault();
        void runBusy("refresh", refreshAll);
      }
    }
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
    // navigate/runBusy 都通过 ref 与最新 state 交互,无需重新订阅。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    let alive = true;
    const unlisten: UnlistenFn[] = [];
    void refreshAll();
    void (async () => {
      const stopTask = await api.listen<unknown>("task-updated", (payload) => mergeTaskUpdate(payload));
      if (alive) unlisten.push(stopTask); else stopTask();
      const stopStats = await api.listen<unknown>("stats-updated", (payload) => {
        if (!payload || typeof payload !== "object") return;
        const data = payload as { stats?: AppState["stats"] } & Partial<AppState["stats"]>;
        startTransition(() => setAppState((current) => current
          ? { ...current, stats: { ...current.stats, ...(data.stats ?? data as AppState["stats"]) } }
          : current));
      });
      if (alive) unlisten.push(stopStats); else stopStats();
      const stopWeb = await api.listen<unknown>("webview-task-submitted", () => {
        void refreshTasks();
        notify("Telegram 已提交下载任务,可在右侧任务面板查看进度。", "info");
      });
      if (alive) unlisten.push(stopWeb); else stopWeb();
    })().catch((error) => { if (alive) notify(`无法订阅实时状态:${friendlyError(error)}`, "error"); });
    return () => {
      alive = false;
      unlisten.forEach((stop) => stop());
      if (toastTimer.current) clearTimeout(toastTimer.current);
    };
    // Tauri 事件监听只注册一次。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Telegram 子 WebView 的 bounds 同步:只在 Telegram 视图可见且完全落在内容区内时显示。
  useEffect(() => {
    if (view !== "telegram") {
      syncTelegramWebviewRef.current = null;
      setWebviewEmbeddedReady(false);
      // 不隐藏 WebView:WebView2 一旦被隐藏,Chromium 会节流页面定时器与渲染,
      // 正在进行的页面抓取会跟着停摆(用户只是想边下边看任务列表)。
      // 收到 1×1 停靠:人眼看不见,但页面仍处于"可见"状态。
      void api
        .setTelegramWebviewBounds({ x: 0, y: 0, width: 1, height: 1 })
        .then(() => api.setTelegramWebviewVisible(true))
        .catch(() => undefined);
      return;
    }

    const container = telegramWebviewRef.current;
    if (!container) return;
    const webviewContainer = container as HTMLDivElement;

    let active = true;
    let animationFrame = 0;
    let running = false;
    let requested = false;
    let syncPromise = Promise.resolve();
    let previousBounds = "";
    let nativeVisible = false;
    let webviewReady: Promise<void> | null = null;
    let stopResize: UnlistenFn | undefined;
    let stopScale: UnlistenFn | undefined;

    async function applyBounds() {
      if (!active) return;
      if (viewRef.current !== "telegram" || document.visibilityState === "hidden") {
        if (nativeVisible) {
          await api.setTelegramWebviewVisible(false);
          nativeVisible = false;
        }
        return;
      }

      const rect = webviewContainer.getBoundingClientRect();
      const viewportWidth = document.documentElement.clientWidth;
      const viewportHeight = document.documentElement.clientHeight;
      const titlebarBottom = document.querySelector(".titlebar")?.getBoundingClientRect().bottom ?? 0;
      const fullyVisible = rect.width > 0
        && rect.height > 0
        && rect.left >= 0
        && rect.top >= titlebarBottom - 1
        && rect.right <= viewportWidth + 1
        && rect.bottom <= viewportHeight + 1;

      if (!fullyVisible) {
        if (nativeVisible) {
          await api.setTelegramWebviewVisible(false);
          nativeVisible = false;
        }
        setWebviewEmbeddedReady(false);
        return;
      }

      webviewReady ??= api.ensureTelegramWebview().catch((error) => {
        webviewReady = null;
        throw error;
      });
      await webviewReady;
      if (!active || viewRef.current !== "telegram") return;

      const scaleFactor = await nativeWindow.scaleFactor().catch(() => window.devicePixelRatio || 1);
      if (!active || viewRef.current !== "telegram") return;
      // DOMRect 是 CSS 像素;换算成 Tauri 逻辑像素。
      const cssToLogical = (window.devicePixelRatio || scaleFactor) / scaleFactor;
      const bounds = {
        x: rect.left * cssToLogical,
        y: rect.top * cssToLogical,
        width: rect.width * cssToLogical,
        height: rect.height * cssToLogical,
      };
      const key = [bounds.x, bounds.y, bounds.width, bounds.height]
        .map((value) => Math.round(value * 100) / 100)
        .join(":");

      if (key !== previousBounds) {
        await api.setTelegramWebviewBounds(bounds);
        if (!active || viewRef.current !== "telegram") return;
        previousBounds = key;
      }
      if (!nativeVisible) {
        await api.setTelegramWebviewVisible(true);
        if (active && viewRef.current === "telegram") {
          nativeVisible = true;
          setWebviewEmbeddedReady(true);
        }
      }
    }

    function synchronize(): Promise<void> {
      requested = true;
      if (running) return syncPromise;
      running = true;
      syncPromise = (async () => {
        try {
          while (active && requested) {
            requested = false;
            await applyBounds();
          }
        } catch (error) {
          if (active) {
            setWebviewError(friendlyError(error));
            setWebviewEmbeddedReady(false);
            void api.setTelegramWebviewVisible(false).catch(() => undefined);
          }
          throw error;
        } finally {
          running = false;
          if (active && requested) void synchronize().catch(() => undefined);
        }
      })();
      return syncPromise;
    }

    function scheduleSync() {
      if (!active || animationFrame) return;
      animationFrame = window.requestAnimationFrame(() => {
        animationFrame = 0;
        void synchronize().catch(() => undefined);
      });
    }

    syncTelegramWebviewRef.current = synchronize;
    const observer = new ResizeObserver(scheduleSync);
    observer.observe(webviewContainer);
    if (splitAreaRef.current) observer.observe(splitAreaRef.current);

    const scrollOptions: AddEventListenerOptions = { capture: true, passive: true };
    window.addEventListener("resize", scheduleSync, { passive: true });
    window.addEventListener("scroll", scheduleSync, scrollOptions);
    window.visualViewport?.addEventListener("resize", scheduleSync, { passive: true });
    window.visualViewport?.addEventListener("scroll", scheduleSync, { passive: true });
    document.addEventListener("visibilitychange", scheduleSync);

    void nativeWindow.onResized(scheduleSync).then((unlisten) => {
      if (active) stopResize = unlisten;
      else unlisten();
    }).catch((error) => {
      if (active) setWebviewError(friendlyError(error));
    });
    void nativeWindow.onScaleChanged(scheduleSync).then((unlisten) => {
      if (active) stopScale = unlisten;
      else unlisten();
    }).catch((error) => {
      if (active) setWebviewError(friendlyError(error));
    });
    scheduleSync();

    return () => {
      active = false;
      requested = false;
      if (animationFrame) window.cancelAnimationFrame(animationFrame);
      observer.disconnect();
      window.removeEventListener("resize", scheduleSync);
      window.removeEventListener("scroll", scheduleSync, true);
      window.visualViewport?.removeEventListener("resize", scheduleSync);
      window.visualViewport?.removeEventListener("scroll", scheduleSync);
      document.removeEventListener("visibilitychange", scheduleSync);
      stopResize?.();
      stopScale?.();
      if (syncTelegramWebviewRef.current === synchronize) syncTelegramWebviewRef.current = null;
      void api.setTelegramWebviewVisible(false).catch(() => undefined);
    };
  }, [view, webviewHostMounted, nativeWindow]);

  // 分隔条拖拽:280–560 调整面板宽度,拖到 240 以下松手 = 折叠(转到任务全览)。
  function startPanelDrag(event: React.MouseEvent<HTMLDivElement>) {
    event.preventDefault();
    const panel = panelRef.current;
    if (!panel) return;
    const startWidth = panel.getBoundingClientRect().width;
    const restoreWidth = panelWidth;
    dragStateRef.current = { startX: event.clientX, startWidth };
    setDraggingPanel(true);

    const onMove = (moveEvent: MouseEvent) => {
      const state = dragStateRef.current;
      if (!state) return;
      const next = Math.min(PANEL_MAX, Math.max(160, state.startWidth - (moveEvent.clientX - state.startX)));
      setPanelWidth(next);
    };
    const onUp = () => {
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
      dragStateRef.current = null;
      setDraggingPanel(false);
      const finalWidth = panelRef.current?.getBoundingClientRect().width ?? restoreWidth;
      if (finalWidth < PANEL_COLLAPSE) {
        setPanelWidth(restoreWidth);
        savePrefs({ panelWidth: restoreWidth });
        navigate("tasks");
      } else {
        const settled = Math.round(Math.min(PANEL_MAX, Math.max(PANEL_MIN, finalWidth)));
        setPanelWidth(settled);
        savePrefs({ panelWidth: settled });
      }
    };
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
  }

  async function showTelegramWebview() {
    setWebviewError("");
    setBusy((current) => ({ ...current, webview: true }));
    try {
      if (viewRef.current !== "telegram" || !syncTelegramWebviewRef.current) {
        throw new Error("Telegram Web 内容区尚未就绪,请稍后重试。");
      }
      await syncTelegramWebviewRef.current();
    } catch (error) {
      const message = friendlyError(error);
      setWebviewError(message);
      notify(message, "error");
    } finally {
      setBusy((current) => ({ ...current, webview: false }));
    }
  }

  async function actionTask(task: DownloadTask, action: TaskAction) {
    await runBusy(`task-${task.taskId}`, async () => {
      const updated = await api.taskAction(task.taskId, action);
      mergeTaskUpdate(updated);
      await refreshTasks();
      notify(
        action === "resume"
          ? "已继续:媒体仍打开时会自动续传;否则重新打开该媒体再点下载即可续传。"
          : action === "retry"
            ? "已重新排队:媒体仍打开时会自动续传;否则重新打开该媒体再点下载。"
            : ({ pause: "已暂停任务。", cancel: "任务已取消。" })[action],
      );
    });
  }

  async function openTaskFolder(task: DownloadTask) {
    await runBusy(`open-${task.taskId}`, () => api.openTaskLocation(task.taskId), "已打开文件位置。");
  }

  async function copyTaskName(task: DownloadTask) {
    const name = task.fileName ?? "";
    if (!name) return;
    try {
      await navigator.clipboard.writeText(name);
      notify("已复制文件名。");
    } catch (error) {
      notify(`复制失败:${friendlyError(error)}`, "error");
    }
  }

  function requestDelete(task: DownloadTask) {
    setDeleteFile(false);
    setDeleteTarget({ kind: "single", task });
  }

  function requestClearFinished() {
    setDeleteFile(false);
    setDeleteTarget({ kind: "clearFinished" });
  }

  function requestBulkDelete() {
    const targets = tasks.filter((task) => selectedIds.has(task.taskId));
    if (targets.length === 0) return;
    setDeleteFile(false);
    setDeleteTarget({ kind: "selection", tasks: targets });
  }

  function toggleSelect(task: DownloadTask) {
    setSelectedIds((current) => {
      const next = new Set(current);
      if (next.has(task.taskId)) next.delete(task.taskId);
      else next.add(task.taskId);
      return next;
    });
  }

  function toggleSelectAll(checked: boolean) {
    setSelectedIds(checked ? new Set(visibleTasks.map((task) => task.taskId)) : new Set());
  }

  function clearSelection() {
    setSelectedIds(new Set());
  }

  async function bulkAction(action: "pause" | "resume" | "retry") {
    const targets = tasks.filter((task) => selectedIds.has(task.taskId) && getTaskActions(task).includes(action));
    if (targets.length === 0) return;
    await runBusy("bulk-action", async () => {
      for (const task of targets) {
        await api.taskAction(task.taskId, action);
      }
      await refreshTasks();
    }, `已对 ${targets.length} 项执行${actionLabel(action)}。`);
  }

  async function confirmDelete() {
    const target = deleteTarget;
    if (!target) return;
    const finished = target.kind === "clearFinished"
      ? tasks.filter((task) => ["completed", "cancelled"].includes(task.status.toLowerCase()))
      : [];
    const selected = target.kind === "selection" ? target.tasks : [];
    await runBusy("delete-task", async () => {
      if (target.kind === "single") {
        await api.deleteTask(target.task.taskId, deleteFile);
        setSelectedId((current) => (current === target.task.taskId ? null : current));
        setSelectedIds((current) => {
          const next = new Set(current);
          next.delete(target.task.taskId);
          return next;
        });
      } else {
        for (const task of [...finished, ...selected]) {
          await api.deleteTask(task.taskId, false);
        }
        if (selected.length > 0) clearSelection();
      }
      await refreshTasks();
    }, target.kind === "single"
      ? (deleteFile ? "任务与文件已删除。" : "任务已删除。")
      : target.kind === "selection"
        ? `已删除 ${selected.length} 项任务。`
        : `已清除 ${finished.length} 条已完成记录。`);
    setDeleteTarget(null);
    setDeleteFile(false);
  }

  async function saveSettings() {
    if (!editableSettings) return;
    const settingsToSave = structuredClone(editableSettings);
    const saved = await runBusy("save-settings", async () => {
      await api.saveSettings(settingsToSave);
      await loadState();
      return true;
    }, "设置已保存。");
    if (saved) {
      setSettingsSaved(true);
      setTimeout(() => setSettingsSaved(false), 2200);
    }
  }

  function resetSettings() {
    if (!appState) return;
    setEditableSettings(structuredClone(appState.settings));
    notify("已还原为当前生效的设置。", "info");
  }

  async function applyStorageRoot() {
    const path = storageTarget.trim();
    if (!path) return;
    const result = await runBusy("storage-root", () => api.changeStorageRoot(path));
    if (result) {
      setStorageConfirm(false);
      notify(result.restartRequired ? "存储数据已迁移,客户端将在几秒后自动重启。" : "存储位置已更新。");
    }
  }

  async function chooseDirectory(target: "storage" | "downloads") {
    try {
      const path = await api.pickDirectory(target === "storage" ? "选择应用数据目录" : "选择下载目录");
      if (!path) return;
      if (target === "storage") setStorageTarget(path);
      else updateSettings({ downloadRoot: path });
    } catch (error) {
      notify(`无法选择目录:${friendlyError(error)}`, "error");
    }
  }

  async function checkForUpdates() {
    if (updatePhase === "checking" || updatePhase === "installing") return;
    setUpdatePhase("checking");
    try {
      const update = await check();
      if (!update) {
        setUpdateRelease(null);
        setUpdatePhase("current");
        return;
      }
      const previous = pendingUpdateRef.current;
      pendingUpdateRef.current = update;
      if (previous) void previous.close().catch(() => undefined);
      setUpdateRelease({ version: update.version });
      setUpdatePhase("available");
    } catch (error) {
      setUpdatePhase("idle");
      notify(`检查更新失败:${friendlyError(error)}`, "error");
    }
  }

  async function installUpdate() {
    const update = pendingUpdateRef.current;
    if (!update || updatePhase === "installing") return;
    setUpdatePhase("installing");
    setUpdateProgress(0);
    let totalBytes = 0;
    let receivedBytes = 0;
    try {
      await update.downloadAndInstall((event) => {
        if (event.event === "Started") {
          totalBytes = event.data.contentLength ?? 0;
          receivedBytes = 0;
          setUpdateProgress(0);
        } else if (event.event === "Progress") {
          receivedBytes += event.data.chunkLength;
          setUpdateProgress(totalBytes > 0 ? Math.min(100, Math.round((receivedBytes / totalBytes) * 100)) : 0);
        } else {
          setUpdateProgress(100);
        }
      });
      pendingUpdateRef.current = null;
      setUpdatePhase("ready");
      notify("更新已安装,重启后生效。");
    } catch (error) {
      setUpdatePhase("available");
      notify(`安装更新失败:${friendlyError(error)}`, "error");
    }
  }

  async function restartForUpdate() {
    try {
      await relaunch();
    } catch (error) {
      notify(`无法自动重启:${friendlyError(error)},请手动关闭并重新打开应用。`, "error");
    }
  }

  function updateSettings(update: Partial<Settings>) {
    setEditableSettings((current) => current ? { ...current, ...update } : current);
  }

  function updateConcurrency(update: Partial<Settings["concurrency"]>) {
    setEditableSettings((current) => current ? { ...current, concurrency: { ...current.concurrency, ...update } } : current);
  }

  const updateUi: UpdateUiState = {
    phase: updatePhase,
    version: updateRelease?.version,
    progress: updateProgress,
    onCheck: () => void checkForUpdates(),
    onInstall: () => void installUpdate(),
    onRestart: () => void restartForUpdate(),
  };
  const deleteTaskStatus = deleteTarget && deleteTarget.kind === "single"
    ? deleteTarget.task.status.toLowerCase()
    : "";
  const deleteInFlight = ["downloading", "queued", "paused"].includes(deleteTaskStatus);

  return (
    <TooltipProvider delayDuration={420}>
      <div className="app">
        <TitleBar caption={VIEW_LABELS[view]} maximized={windowMaximized} onError={(message) => notify(message, "error")} />

        {fatalError ? (
          <div className="fatal">
            <h2>客户端未能完成启动</h2>
            <p>{fatalError}</p>
            <Button variant="primary" onClick={() => void runBusy("refresh", refreshAll)}>重试</Button>
          </div>
        ) : loading && !appState ? (
          <div className="loading-screen"><span className="spinner" />正在读取应用状态…</div>
        ) : (
          <div className="body">
            <Rail view={view} onNavigate={navigate} activeCount={stats?.activeDownloads ?? 0} />

            {view === "telegram" ? (
              <div className="content">
                <div className="web-area" ref={splitAreaRef}>
                  <div className="webview-surface" ref={telegramWebviewRef} data-tauri-webview-slot="telegram">
                    {!webviewEmbeddedReady ? (
                      <div className="webview-placeholder">
                        <span className="ph-mark"><Icon name="telegram" size={22} /></span>
                        {webviewError ? (
                          <>
                            <span className="err">{webviewError}</span>
                            <Button variant="secondary" onClick={() => void showTelegramWebview()} disabled={busy.webview}>
                              {busy.webview ? "正在恢复…" : "重新加载 Telegram Web"}
                            </Button>
                          </>
                        ) : (
                          <>
                            <span>正在连接 Telegram Web…<br />登录后打开任意媒体,点击下载按钮即可开始。</span>
                            <Button variant="secondary" onClick={() => void showTelegramWebview()} disabled={busy.webview}>
                              {busy.webview ? "正在加载…" : "立即加载"}
                            </Button>
                          </>
                        )}
                      </div>
                    ) : null}
                  </div>
                  <div
                    className={"divider" + (draggingPanel ? " dragging" : "")}
                    role="separator"
                    aria-orientation="vertical"
                    aria-label="调整任务面板宽度"
                    onMouseDown={startPanelDrag}
                    onDoubleClick={() => navigate("tasks")}
                  />
                  <TaskPanel
                    ref={panelRef}
                    width={panelWidth}
                    tasks={panelTasks}
                    filter={panelFilter}
                    onFilterChange={setPanelFilter}
                    onSelect={(task) => setSelectedId(task.taskId)}
                    onAction={(task, action) => void actionTask(task, action)}
                    onOpenFolder={(task) => void openTaskFolder(task)}
                    onCopyName={(task) => void copyTaskName(task)}
                    onDelete={requestDelete}
                    onShowAll={() => navigate("tasks")}
                    counts={{ active: counts.active, completed: counts.completed, failed: counts.failed }}
                    selectedId={selectedId}
                  />
                </div>
              </div>
            ) : null}

            {view === "tasks" ? (
              <TaskList
                tasks={visibleTasks}
                counts={counts}
                filter={taskFilter}
                onFilterChange={setTaskFilter}
                selectedId={selectedId}
                selectedIds={selectedIds}
                onSelect={(task) => setSelectedId(task?.taskId ?? null)}
                onToggleSelect={toggleSelect}
                onToggleSelectAll={toggleSelectAll}
                onAction={(task, action) => void actionTask(task, action)}
                onOpenFolder={(task) => void openTaskFolder(task)}
                onCopyName={(task) => void copyTaskName(task)}
                onDelete={requestDelete}
                onClearFinished={requestClearFinished}
                onBulkAction={(action) => void bulkAction(action)}
                onBulkDelete={requestBulkDelete}
                onClearSelection={clearSelection}
              />
            ) : null}

            {view === "logs" ? (
              <LogsPage
                logs={filteredLogs}
                level={logLevel}
                onLevelChange={setLogLevel}
                loading={logsLoading}
                onRefresh={() => void refreshLogs()}
              />
            ) : null}

            {view === "settings" && editableSettings && appState ? (
              <SettingsPage
                settings={editableSettings}
                appState={appState}
                onSettingsChange={updateSettings}
                onConcurrencyChange={updateConcurrency}
                onSave={() => void saveSettings()}
                onReset={resetSettings}
                saving={Boolean(busy["save-settings"])}
                saved={settingsSaved}
                storageTarget={storageTarget}
                onStorageTargetChange={setStorageTarget}
                onChooseDirectory={(target) => void chooseDirectory(target)}
                onMigrate={() => setStorageConfirm(true)}
                targetIsSystemVolume={targetIsSystemVolume}
                theme={theme}
                onThemeChange={setTheme}
                update={updateUi}
                notifyError={(message) => notify(message, "error")}
              />
            ) : null}
          </div>
        )}

        <StatusBar stats={stats} version={appState?.appVersion} />

        {toast ? (
          <div className="toast-stack">
            <div className={"toast " + toast.kind} role="status">
              <span className="ic"><Icon name={toast.kind === "error" ? "alert" : toast.kind === "info" ? "info" : "check"} size={12} /></span>
              <span>{toast.message}</span>
            </div>
          </div>
        ) : null}

        <AlertDialog open={storageConfirm} onOpenChange={setStorageConfirm}>
          <AlertDialogContent>
            <AlertDialogTitle asChild><h2>迁移应用数据目录?</h2></AlertDialogTitle>
            <AlertDialogDescription asChild>
              <p>
                任务数据库、登录 profile 与日志会迁移到 <b>{storageTarget}</b>。迁移期间请保持应用运行;
                完成后客户端会自动重启。
              </p>
            </AlertDialogDescription>
            <div className="modal-foot">
              <Button variant="ghost" onClick={() => setStorageConfirm(false)}>取消</Button>
              <Button variant="primary" onClick={() => void applyStorageRoot()} disabled={busy["storage-root"]}>
                {busy["storage-root"] ? "迁移中…" : "开始迁移"}
              </Button>
            </div>
          </AlertDialogContent>
        </AlertDialog>

        <AlertDialog
          open={deleteTarget !== null}
          onOpenChange={(open) => {
            if (!open) {
              setDeleteTarget(null);
              setDeleteFile(false);
            }
          }}
        >
          <AlertDialogContent>
            {deleteTarget && deleteTarget.kind === "single" ? (
              <>
                <AlertDialogTitle asChild><h2>删除任务?</h2></AlertDialogTitle>
                <AlertDialogDescription asChild>
                  <p>
                    将从任务列表移除「{deleteTarget.task.fileName || "未命名媒体"}」。
                    {deleteInFlight ? "该任务正在下载,会先被停止;" : ""}临时分块文件会一并清理。
                  </p>
                </AlertDialogDescription>
                {deleteTaskStatus === "completed" ? (
                  <div className="switch-row" style={{ marginBottom: 14 }}>
                    <span className="switch-copy">
                      <strong>同时删除已下载的文件</strong>
                      <small>文件将从磁盘移除,不可恢复。</small>
                    </span>
                    <Switch checked={deleteFile} onCheckedChange={setDeleteFile} aria-label="同时删除已下载的文件" />
                  </div>
                ) : null}
              </>
            ) : deleteTarget && deleteTarget.kind === "selection" ? (
              <>
                <AlertDialogTitle asChild><h2>删除选中的 {deleteTarget.tasks.length} 项任务?</h2></AlertDialogTitle>
                <AlertDialogDescription asChild>
                  <p>将从任务列表移除选中的任务,正在下载的会先被停止;已下载的文件不受影响。</p>
                </AlertDialogDescription>
              </>
            ) : (
              <>
                <AlertDialogTitle asChild><h2>清除已完成记录?</h2></AlertDialogTitle>
                <AlertDialogDescription asChild>
                  <p>将移除 {counts.completed + counts.failed} 条已完成 / 已取消的任务记录,已下载的文件不受影响。</p>
                </AlertDialogDescription>
              </>
            )}
            <div className="modal-foot">
              <Button variant="ghost" onClick={() => { setDeleteTarget(null); setDeleteFile(false); }}>取消</Button>
              <Button variant="destructive" onClick={() => void confirmDelete()} disabled={busy["delete-task"]}>
                {busy["delete-task"] ? "正在删除…" : "删除"}
              </Button>
            </div>
          </AlertDialogContent>
        </AlertDialog>
      </div>
    </TooltipProvider>
  );
}

export default App;
