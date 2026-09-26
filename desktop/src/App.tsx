import { startTransition, useEffect, useMemo, useRef, useState, type FormEvent } from "react";
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
import { Badge } from "./components/ui/badge";
import { Card } from "./components/ui/card";
import { Dialog, DialogContent, DialogDescription, DialogTitle } from "./components/ui/dialog";
import { Switch } from "./components/ui/switch";
import { Tabs, TabsList, TabsTrigger } from "./components/ui/tabs";

type PageKey = "telegram" | "downloads" | "logs" | "settings";
type TaskFilter = "all" | "downloading" | "queued" | "paused" | "completed" | "failed";
type Toast = { kind: "success" | "error" | "info"; message: string };
type UpdatePhase = "idle" | "checking" | "current" | "available" | "installing" | "ready";

const navigation: { id: "telegram" | "downloads"; label: string; icon: string }[] = [
  { id: "telegram", label: "Telegram Web", icon: "telegram" },
  { id: "downloads", label: "下载任务", icon: "download" },
];

const taskFilters: { id: TaskFilter; label: string }[] = [
  { id: "all", label: "全部" },
  { id: "downloading", label: "活动中" },
  { id: "queued", label: "队列" },
  { id: "paused", label: "已暂停" },
  { id: "completed", label: "已完成" },
  { id: "failed", label: "失败" },
];

const webviewLifecycleNote = "下载由 Telegram 页面内的抓取持续进行:媒体保持打开时,暂停/继续立即生效。应用关闭或页面中断后,任务与已下载分块会保留——重新打开该媒体再点下载即可从已完成分块继续;若提示链接过期,同样重新打开媒体刷新后重试。";


function formatBytes(value?: number | null): string {
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

function formatSpeed(value?: number | null): string {
  if (!value) return "0 B/s";
  return `${formatBytes(value)}/s`;
}

function formatDate(value?: string | null): string {
  if (!value) return "—";
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return new Intl.DateTimeFormat("zh-CN", { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" }).format(date);
}

function getPercent(task: DownloadTask): number {
  if (task.totalBytes && task.totalBytes > 0) return Math.min(100, Math.max(0, (task.downloadedBytes / task.totalBytes) * 100));
  return Math.min(100, Math.max(0, task.progress));
}

function statusLabel(status: string): string {
  const labels: Record<string, string> = { downloading: "下载中", queued: "排队中", paused: "已暂停", completed: "已完成", failed: "失败", cancelled: "已取消" };
  return labels[status.toLowerCase()] ?? status;
}

function statusTone(status: string): string {
  const tones: Record<string, string> = { downloading: "blue", queued: "muted", paused: "amber", completed: "green", failed: "red", cancelled: "muted" };
  return tones[status.toLowerCase()] ?? "muted";
}

function getTaskActions(task: DownloadTask): TaskAction[] {
  switch (task.status.toLowerCase()) {
    case "downloading": return ["pause", "cancel"];
    case "paused": return ["resume", "cancel"];
    case "queued": return ["cancel"];
    case "failed":
    case "cancelled": return ["retry"];
    default: return [];
  }
}

function actionLabel(action: TaskAction): string {
  return ({ pause: "暂停", resume: "继续", cancel: "取消", retry: "重试" })[action];
}

function StatusBadge({ status }: { status: string }) {
  return <Badge className={statusTone(status)}><i />{statusLabel(status)}</Badge>;
}

function App() {
  const [page, setPage] = useState<PageKey>("telegram");
  const pageRef = useRef<PageKey>("telegram");
  const nativeWindow = getCurrentWindow();
  const telegramWebviewRef = useRef<HTMLDivElement | null>(null);
  const syncTelegramWebviewRef = useRef<(() => Promise<void>) | null>(null);
  const [appState, setAppState] = useState<AppState | null>(null);
  const [editableSettings, setEditableSettings] = useState<Settings | null>(null);
  const [tasks, setTasks] = useState<DownloadTask[]>([]);
  const [logs, setLogs] = useState<LogEntry[]>([]);
  const [selectedTask, setSelectedTask] = useState<DownloadTask | null>(null);
  const [windowMaximized, setWindowMaximized] = useState(false);
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
  const [logLevel, setLogLevel] = useState("all");
  const [webviewError, setWebviewError] = useState("");
  const [updatePhase, setUpdatePhase] = useState<UpdatePhase>("idle");
  const [updateRelease, setUpdateRelease] = useState<{ version: string } | null>(null);
  const [updateProgress, setUpdateProgress] = useState(0);
  const pendingUpdateRef = useRef<Update | null>(null);
  const toastTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const stats = appState?.stats ?? null;
  const webviewHostMounted = appState !== null;
  const availableVolumes = appState?.storageOptions ?? [];
  const preferredVolumes = useMemo(
    () => [...availableVolumes].sort((left, right) => Number(left.isSystem) - Number(right.isSystem)),
    [availableVolumes],
  );
  const targetIsSystemVolume = availableVolumes.some((volume) => {
    if (!volume.isSystem || !storageTarget) return false;
    const root = volume.path.replace(/[\\/]+$/, "").toLocaleLowerCase();
    const target = storageTarget.replace(/[\\/]+$/, "").toLocaleLowerCase();
    return target === root || target.startsWith(`${root}\\`) || target.startsWith(`${root}/`);
  });
  const pageLabels: Record<PageKey, string> = {
    telegram: "Telegram Web",
    downloads: "下载任务",
    logs: "日志与状态",
    settings: "设置",
  };
  const currentTitle = pageLabels[page];
  const visibleTasks = useMemo(() => tasks.filter((task) => taskFilter === "all" || task.status.toLowerCase() === taskFilter), [tasks, taskFilter]);
  const filteredLogs = useMemo(() => logs.filter((item) => logLevel === "all" || (item.level ?? "info").toLowerCase() === logLevel), [logs, logLevel]);

  function notify(message: string, kind: Toast["kind"] = "success") {
    setToast({ message, kind });
    if (toastTimer.current) clearTimeout(toastTimer.current);
    toastTimer.current = setTimeout(() => setToast(null), 4200);
  }

  async function toggleWindowMaximize() {
    try {
      await nativeWindow.toggleMaximize();
      setWindowMaximized(await nativeWindow.isMaximized());
    } catch (error) {
      notify(`窗口操作失败：${friendlyError(error)}`, "error");
    }
  }

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

  useEffect(() => {
    function onKeyDown(event: KeyboardEvent) {
      const target = event.target;
      const typing = target instanceof HTMLElement && (target.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(target.tagName));
      if (event.key === "Escape" && selectedTask) {
        setSelectedTask(null);
        return;
      }
      if (typing) return;
      if ((event.ctrlKey || event.metaKey) && ["1", "2", ","].includes(event.key)) {
        event.preventDefault();
        const destination: PageKey = event.key === "1" ? "telegram" : event.key === "2" ? "downloads" : "settings";
        void navigate(destination);
      } else if (event.key === "F5") {
        event.preventDefault();
        void runBusy("refresh", refreshAll);
      }
    }
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, [selectedTask]);

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
      setSelectedTask((selected) => selected ? rows.find((task) => task.taskId === selected.taskId) ?? selected : null);
    } catch (error) {
      notify(`读取下载任务失败：${friendlyError(error)}`, "error");
    }
  }

  async function refreshLogs() {
    setLogsLoading(true);
    try { setLogs(await api.getLogs(500)); }
    catch (error) { notify(`读取日志失败：${friendlyError(error)}`, "error"); }
    finally { setLogsLoading(false); }
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
          ? current.map((task) => task.taskId === candidate.taskId ? candidate : task)
          : [candidate, ...current];
        return next.slice(0, 300);
      });
      setSelectedTask((selected) => selected?.taskId === candidate.taskId ? candidate : selected);
    });
  }

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
        notify("Telegram Web 已提交下载任务。", "info");
      });
      if (alive) unlisten.push(stopWeb); else stopWeb();
    })().catch((error) => { if (alive) notify(`无法订阅实时状态：${friendlyError(error)}`, "error"); });
    return () => {
      alive = false;
      unlisten.forEach((stop) => stop());
      if (toastTimer.current) clearTimeout(toastTimer.current);
    };
    // Tauri event listeners are registered once for this window.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    if (page !== "telegram") {
      syncTelegramWebviewRef.current = null;
      setWebviewEmbeddedReady(false);
      void api.setTelegramWebviewVisible(false).catch(() => undefined);
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
      if (pageRef.current !== "telegram" || document.visibilityState === "hidden") {
        if (nativeVisible) {
          await api.setTelegramWebviewVisible(false);
          nativeVisible = false;
        }
        return;
      }

      const rect = webviewContainer.getBoundingClientRect();
      const viewportWidth = document.documentElement.clientWidth;
      const viewportHeight = document.documentElement.clientHeight;
      const titlebarBottom = document.querySelector(".window-titlebar")?.getBoundingClientRect().bottom ?? 0;
      const topbarBottom = Math.max(titlebarBottom, document.querySelector(".topbar")?.getBoundingClientRect().bottom ?? 0);
      const fullyVisible = rect.width > 0
        && rect.height > 0
        && rect.left >= 0
        && rect.top >= topbarBottom
        && rect.right <= viewportWidth
        && rect.bottom <= viewportHeight;

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
      if (!active || pageRef.current !== "telegram") return;

      const scaleFactor = await nativeWindow.scaleFactor().catch(() => window.devicePixelRatio || 1);
      if (!active || pageRef.current !== "telegram") return;
      // DOMRect uses CSS pixels; convert via the WebView device scale to Tauri logical pixels.
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
        if (!active || pageRef.current !== "telegram") return;
        previousBounds = key;
      }
      if (!nativeVisible) {
        await api.setTelegramWebviewVisible(true);
        if (active && pageRef.current === "telegram") {
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
    const panel = webviewContainer.closest(".web-panel");
    const pageContent = webviewContainer.closest(".page-content");
    if (panel) observer.observe(panel);
    if (pageContent) observer.observe(pageContent);

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
  }, [page, webviewHostMounted]);

  async function navigate(next: PageKey) {
    pageRef.current = next;
    setPage(next);
    if (next === "logs" && logs.length === 0) void refreshLogs();
    if (next === "downloads") void refreshTasks();
  }

  async function showTelegramWebview() {
    setWebviewError("");
    setBusy((current) => ({ ...current, webview: true }));
    try {
      if (pageRef.current !== "telegram" || !syncTelegramWebviewRef.current) {
        throw new Error("Telegram Web 内容区尚未就绪，请稍后重试。");
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
      notify(({ pause: "已暂停任务。", resume: "已继续任务。", cancel: "任务已取消。", retry: "任务已重新排队。" })[action]);
    });
  }

  async function openTaskFolder(task: DownloadTask) {
    await runBusy(`open-${task.taskId}`, () => api.openTaskLocation(task.taskId), "已打开文件位置。");
  }

  async function saveSettings(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
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

  async function applyStorageRoot() {
    const path = storageTarget.trim();
    if (!path) return;
    const result = await runBusy("storage-root", () => api.changeStorageRoot(path));
    if (result) {
      setStorageConfirm(false);
      notify(result.restartRequired ? "存储数据已迁移，客户端将在几秒后自动重启。" : "存储位置已更新。");
    }
  }

  async function chooseDirectory(target: "storage" | "downloads") {
    try {
      const path = await api.pickDirectory(target === "storage" ? "选择应用数据目录" : "选择下载目录");
      if (!path) return;
      if (target === "storage") setStorageTarget(path);
      else updateSettings({ downloadRoot: path });
    } catch (error) {
      notify(`无法选择目录：${friendlyError(error)}`, "error");
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
      notify(`检查更新失败：${friendlyError(error)}`, "error");
    }
  }

  function dismissUpdate() {
    const pending = pendingUpdateRef.current;
    pendingUpdateRef.current = null;
    if (pending) void pending.close().catch(() => undefined);
    setUpdateRelease(null);
    setUpdatePhase("idle");
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
      // Windows 下安装程序接管后会直接结束当前进程并自动启动新版本；
      // 若进程仍存活（例如安装器未自动重启），则提示用户手动重启。
      pendingUpdateRef.current = null;
      setUpdatePhase("ready");
      notify("更新已安装，重启后生效。");
    } catch (error) {
      setUpdatePhase("available");
      notify(`安装更新失败：${friendlyError(error)}`, "error");
    }
  }

  async function restartForUpdate() {
    try {
      await relaunch();
    } catch (error) {
      notify(`无法自动重启：${friendlyError(error)}，请手动关闭并重新打开应用。`, "error");
    }
  }

  function updateSettings(update: Partial<Settings>) {
    setEditableSettings((current) => current ? { ...current, ...update } : current);
  }

  function updateConcurrency(update: Partial<Settings["concurrency"]>) {
    setEditableSettings((current) => current ? { ...current, concurrency: { ...current.concurrency, ...update } } : current);
  }

  function taskCount(status: string) { return tasks.filter((task) => task.status.toLowerCase() === status).length; }
  return (
    <div className={"app-shell app-shell-page-" + page}>
      <header className="window-titlebar">
        <div className="window-title-drag" data-tauri-drag-region onDoubleClick={() => void toggleWindowMaximize()}>
          <span className="window-app-mark"><Icon name="telegram" size={15} /></span>
          <span className="window-app-name">Telegram Media Downloader</span>
          <span className="window-page-caption">{currentTitle}</span>
        </div>
        <div className="window-controls">
          <button type="button" className="window-control" aria-label="最小化" title="最小化" onClick={() => void nativeWindow.minimize().catch((error) => notify(friendlyError(error), "error"))}><Icon name="minimize" size={15} /></button>
          <button type="button" className="window-control" aria-label={windowMaximized ? "还原" : "最大化"} title={windowMaximized ? "还原" : "最大化"} onClick={() => void toggleWindowMaximize()}><Icon name={windowMaximized ? "restore" : "maximize"} size={14} /></button>
          <button type="button" className="window-control window-control-close" aria-label="隐藏到托盘" title="隐藏到托盘(退出请用托盘菜单)" onClick={() => void nativeWindow.close().catch((error) => notify(friendlyError(error), "error"))}><Icon name="close" size={15} /></button>
        </div>
      </header>
      <aside className="sidebar">
        <div className="brand-lockup"><div className="brand-mark"><Icon name="telegram" size={21} /></div><div className="brand-copy"><strong>Telegram Media</strong><span>Downloader</span></div><span className="brand-version">{appState?.appVersion ?? "桌面版"}</span></div>
        <div className="nav-section-label">下载管理</div>
        <nav className="nav-list" aria-label="下载管理">{navigation.map((item) => <button key={item.id} className={`nav-item ${page === item.id ? "active" : ""}`} onClick={() => void navigate(item.id)}><Icon name={item.icon} size={18} /><span>{item.label}</span>{item.id === "downloads" && (stats?.activeDownloads ?? 0) > 0 && <span className="nav-count">{stats?.activeDownloads}</span>}</button>)}</nav>
        <div className="sidebar-spacer" />
        <div className="sidebar-storage"><div className="storage-caption"><span>下载存储</span><Icon name="drive" size={15} /></div><strong title={appState?.settings.downloadRoot}>{appState?.settings.downloadRoot || "正在读取…"}</strong><div className="storage-footnote"><span className="status-dot" />应用数据保存在本机</div><button className="sidebar-settings-link" onClick={() => void navigate("settings")}>管理存储位置 <Icon name="arrow" size={14} /></button></div>
        <div className="sidebar-footer"><span className="online-dot" /><span>下载服务运行中</span><span className="footer-spacer" /><button className={`icon-button ${page === "logs" ? "active" : ""}`} title="日志与状态" aria-label="日志与状态" onClick={() => void navigate("logs")}><Icon name="info" size={16} /></button><button className={`icon-button ${page === "settings" ? "active" : ""}`} title="设置" aria-label="设置" onClick={() => void navigate("settings")}><Icon name="settings" size={16} /></button></div>
      </aside>

      <main className="main-shell">
        <header className={"topbar " + (page === "telegram" ? "topbar-telegram" : "")}><div className="breadcrumb"><span>下载管理</span><Icon name="chevron" size={13} /><strong>{currentTitle}</strong></div><div className="topbar-actions"><button type="button" className="connection-chip webview-chip" onClick={() => void navigate("telegram")}><span className="webview-runtime-dot" />Telegram Web</button><button type="button" className="top-icon-button" title="刷新状态" aria-label="刷新状态" onClick={() => void runBusy("refresh", refreshAll)}><Icon name="refresh" size={17} /></button><button type="button" className="top-icon-button" title="设置" aria-label="设置" onClick={() => void navigate("settings")}><Icon name="settings" size={17} /></button></div></header>
        {fatalError && !appState ? <section className="fatal-state"><div className="empty-icon danger"><Icon name="alert" size={24} /></div><h1>无法连接到下载服务</h1><p>{fatalError}</p><Button onClick={() => void refreshAll()} disabled={loading}><Icon name="refresh" size={16} />{loading ? "正在重试…" : "重新连接"}</Button></section>
          : loading && !appState ? <div className="loading-state"><span className="spinner" /><strong>正在读取应用状态</strong><span>连接下载服务并载入本地任务…</span></div>
          : <div className={"page-content " + (page === "telegram" ? "telegram-page" : "")}>
            {page === "telegram" && <section className="telegram-workspace">
              <div ref={telegramWebviewRef} className="webview-surface webview-surface-fill" id="telegram-webview-slot" data-tauri-webview-slot="telegram">
                {(!webviewEmbeddedReady || webviewError) && <div className="webview-start-state" role={webviewError ? "alert" : "status"}>
                  <div className={"webview-status-icon " + (webviewError ? "error" : "")}><Icon name={webviewError ? "alert" : "telegram"} size={25} /></div>
                  <strong>{webviewError ? "Telegram Web 暂不可用" : "正在打开 Telegram Web"}</strong>
                  <p>{webviewError || "首次打开可能需要片刻。请直接在此页面登录 Telegram；网页数据保存在应用数据目录中。"}</p>
                  {webviewError && <Button type="button" variant="secondary" onClick={() => void showTelegramWebview()} disabled={busy.webview}><Icon name="refresh" size={14} />重试</Button>}
                </div>}
              </div>
            </section>}

            {page === "downloads" && <>
              <section className="page-heading download-page-heading">
                <div><div className="eyebrow"><span className="eyebrow-mark" />下载管理</div><h1>任务队列<span className="heading-dot">.</span></h1><p>活动任务实时更新；选择一项查看详情和操作。</p></div>
                <Button onClick={() => void navigate("telegram")}><Icon name="telegram" size={16} />打开 Telegram</Button>
              </section>
              <section className="task-summary-strip" aria-label="下载状态概览">
                <div><span className="summary-label"><i className="summary-blue" />活动中</span><strong>{taskCount("downloading")}</strong></div>
                <div><span className="summary-label"><i className="summary-gray" />排队中</span><strong>{taskCount("queued")}</strong></div>
                <div><span className="summary-label"><i className="summary-green" />已完成</span><strong>{taskCount("completed")}</strong></div>
                <div><span className="summary-label"><i className="summary-red" />失败</span><strong>{taskCount("failed")}</strong></div>
                <div className="summary-speed"><span className="summary-label">总下载速度</span><strong>{formatSpeed(stats?.currentSpeedBytesPerSecond)}</strong></div>
              </section>
              <div className={"download-layout " + (selectedTask ? "has-selection" : "")}>
                <Card className="download-list-panel">
                  <Tabs value={taskFilter} onValueChange={(value) => { setTaskFilter(value as TaskFilter); if (selectedTask && value !== "all" && selectedTask.status.toLowerCase() !== value) setSelectedTask(null); }}>
                    <TabsList aria-label="任务状态筛选">{taskFilters.map((filter) => <TabsTrigger key={filter.id} value={filter.id}>{filter.label}<span>{filter.id === "all" ? tasks.length : taskCount(filter.id)}</span></TabsTrigger>)}</TabsList>
                  </Tabs>
                  {visibleTasks.length ? <div className="download-task-list" role="list" aria-label="下载任务">
                    {visibleTasks.map((task) => <div role="listitem" key={task.taskId}>
                      <button type="button" className={"download-task-card " + (selectedTask?.taskId === task.taskId ? "task-selected" : "")} onClick={() => setSelectedTask(task)} aria-pressed={selectedTask?.taskId === task.taskId} aria-label={(task.fileName || "消息 " + (task.messageId ?? "")) + "，" + statusLabel(task.status) + "，" + Math.round(getPercent(task)) + "%"}>
                        <span className={"task-type-icon " + (task.mediaType === "video" ? "type-video" : task.mediaType === "photo" ? "type-image" : "type-file")}><Icon name={task.mediaType === "video" ? "video" : task.mediaType === "photo" ? "image" : "file"} size={17} /></span>
                        <span className="task-card-main">
                          <span className="task-card-title"><strong title={task.fileName ?? ""}>{task.fileName || "消息 " + (task.messageId ?? "")}</strong><StatusBadge status={task.status} /></span>
                          <span className="task-card-subtitle">{task.chatTitle || "聊天 " + task.chatId} {task.messageId ? "· 消息 " + task.messageId : ""}{task.startedAt ? " · 开始于 " + formatDate(task.startedAt) : ""}</span>
                          <span className="task-progress-line"><span className="progress-track"><i className={task.status.toLowerCase() === "failed" ? "progress-failed" : ""} style={{ width: String(getPercent(task)) + "%" }} /></span><span>{Math.round(getPercent(task))}%</span></span>
                          <span className="task-card-metrics"><span>{formatBytes(task.downloadedBytes)}{task.totalBytes ? " / " + formatBytes(task.totalBytes) : ""}</span><span>{task.status.toLowerCase() === "downloading" ? formatSpeed(task.speedBytesPerSecond) : statusLabel(task.status)}</span><span>{task.remainingBytes != null ? formatBytes(task.remainingBytes) + " 剩余" : ""}</span></span>
                          {task.error && <span className="task-error-preview"><Icon name="alert" size={13} />{task.error}</span>}
                        </span>
                        <span className="task-card-chevron"><Icon name="chevron" size={17} /></span>
                      </button>
                    </div>)}
                  </div> : <div className="empty-inline task-empty"><div className="empty-icon"><Icon name="download" size={20} /></div><strong>{taskFilter === "all" ? "下载队列为空" : "没有" + (taskFilter === "failed" ? "失败" : statusLabel(taskFilter)) + "任务"}</strong><span>{taskFilter === "all" ? "在 Telegram Web 消息中点击“下载”，任务会显示在这里。" : "切换状态筛选查看其他任务。"}</span>{taskFilter === "all" && <Button variant="secondary" onClick={() => void navigate("telegram")}><Icon name="telegram" size={15} />浏览 Telegram</Button>}</div>}
                </Card>
                {selectedTask && <Card className="task-detail-panel" aria-label="任务详情">
                  <div className="detail-heading"><div><span className="eyebrow">任务详情</span><button type="button" className="icon-button detail-close" title="关闭详情" aria-label="关闭详情" onClick={() => setSelectedTask(null)}><Icon name="close" size={15} /></button></div><StatusBadge status={selectedTask.status} /></div>
                  <div className="detail-file-icon"><Icon name={selectedTask.mediaType === "video" ? "video" : selectedTask.mediaType === "photo" ? "image" : "file"} size={24} /></div>
                  <h2 className="detail-title">{selectedTask.fileName || "消息 " + (selectedTask.messageId ?? "")}</h2>
                  <p className="detail-subtitle">{selectedTask.chatTitle || "聊天 " + selectedTask.chatId}{selectedTask.messageId ? " · #" + selectedTask.messageId : ""}</p>
                  <div className="detail-progress"><div className="detail-progress-copy"><strong>{Math.round(getPercent(selectedTask))}%</strong><span>{selectedTask.status.toLowerCase() === "downloading" ? formatSpeed(selectedTask.speedBytesPerSecond) : statusLabel(selectedTask.status)}</span></div><div className="progress-track large"><i className={selectedTask.status.toLowerCase() === "failed" ? "progress-failed" : ""} style={{ width: String(getPercent(selectedTask)) + "%" }} /></div><div className="detail-bytes"><span>{formatBytes(selectedTask.downloadedBytes)} 已下载</span><span>{selectedTask.totalBytes ? "共 " + formatBytes(selectedTask.totalBytes) : "总大小未知"}</span></div></div>
                  {selectedTask.status.toLowerCase() !== "completed" && <div className="download-lifecycle-note"><Icon name="info" size={15} /><span>{webviewLifecycleNote}</span></div>}
                  <div className="detail-facts"><div><span>任务 ID</span><strong>{selectedTask.taskId}</strong></div><div><span>开始时间</span><strong>{formatDate(selectedTask.startedAt)}</strong></div><div><span>最近更新</span><strong>{formatDate(selectedTask.updatedAt)}</strong></div><div><span>重试次数</span><strong>{selectedTask.retryCount ?? 0}</strong></div>{selectedTask.remainingBytes != null && <div><span>剩余数据</span><strong>{formatBytes(selectedTask.remainingBytes)}</strong></div>}{selectedTask.outputPath && <div><span>保存位置</span><strong title={selectedTask.outputPath}>{selectedTask.outputPath}</strong></div>}</div>
                  {selectedTask.error && <div className="detail-error"><Icon name="alert" size={16} /><div><strong>任务失败原因</strong><span>{selectedTask.error}</span></div></div>}
                  <div className="detail-actions">{getTaskActions(selectedTask).map((action) => <Button key={action} variant={action === "cancel" ? "destructive" : "secondary"} onClick={() => void actionTask(selectedTask, action)} disabled={busy["task-" + selectedTask.taskId]}><Icon name={action === "pause" ? "pause" : action === "resume" ? "play" : action === "retry" ? "refresh" : "close"} size={15} />{actionLabel(action)}</Button>)}{(selectedTask.outputPath || selectedTask.status.toLowerCase() === "completed") && <Button className="full-button" onClick={() => void openTaskFolder(selectedTask)} disabled={busy["open-" + selectedTask.taskId]}><Icon name="folder" size={15} />打开文件位置</Button>}</div>
                </Card>}
              </div>
            </>}

            {page === "logs" && <>
              <section className="page-heading"><div><div className="eyebrow"><span className="eyebrow-mark" />诊断中心</div><h1>日志与运行状态<span className="heading-dot">.</span></h1><p>查看 WebView、下载和存储状态，帮助定位问题。</p></div><Button variant="secondary" onClick={() => void refreshLogs()} disabled={logsLoading}><Icon name="refresh" size={16} />{logsLoading ? "正在刷新…" : "刷新日志"}</Button></section>
              <section className="runtime-grid"><Card className="runtime-card"><span className="runtime-icon green"><Icon name="bolt" size={18} /></span><span className="runtime-label">下载服务</span><strong><i className="online-dot" />运行中</strong><small>客户端后台任务处理器</small></Card><Card className="runtime-card"><span className="runtime-icon blue"><Icon name="telegram" size={18} /></span><span className="runtime-label">Telegram Web</span><strong>内嵌页面</strong><small>登录状态由 Telegram Web 管理</small></Card><Card className="runtime-card"><span className="runtime-icon violet"><Icon name="drive" size={18} /></span><span className="runtime-label">本地数据库</span><strong><i className="online-dot" />可用</strong><small>{appState?.settings.dataRoot || "读取中"}</small></Card><Card className="runtime-card"><span className="runtime-icon amber"><Icon name="download" size={18} /></span><span className="runtime-label">实时吞吐</span><strong>{formatSpeed(stats?.currentSpeedBytesPerSecond)}</strong><small>{stats?.activeDownloads ?? 0} 个活动任务</small></Card></section>
              <Card className="log-panel"><div className="panel-heading log-heading"><div><span className="eyebrow">本机日志</span><h2>最近记录</h2></div><select value={logLevel} onChange={(event) => setLogLevel(event.target.value)} aria-label="筛选日志等级"><option value="all">全部等级</option><option value="error">错误</option><option value="warn">警告</option><option value="info">信息</option><option value="debug">调试</option></select></div>{logsLoading && logs.length === 0 ? <div className="list-loading"><span className="spinner small" />正在读取日志…</div> : filteredLogs.length ? <div className="log-table"><div className="log-table-head"><span>时间</span><span>等级</span><span>来源</span><span>内容</span></div>{filteredLogs.map((entry, index) => <div key={`${entry.timestamp}-${index}`} className="log-row"><time>{entry.timestamp ? formatDate(entry.timestamp) : "—"}</time><span className={`log-level level-${(entry.level || "info").toLowerCase()}`}>{(entry.level || "info").toUpperCase()}</span><span className="log-target">{entry.target || "应用"}</span><span className="log-message">{entry.message}</span></div>)}</div> : <div className="empty-inline"><div className="empty-icon"><Icon name="logs" size={20} /></div><strong>当前没有日志记录</strong><span>WebView、下载和存储状态变化会显示在这里。</span></div>}</Card>
            </>}

            {page === "settings" && editableSettings && <>
              <section className="page-heading"><div><div className="eyebrow"><span className="eyebrow-mark" />偏好设置</div><h1>设置与存储<span className="heading-dot">.</span></h1><p>配置下载目录、文件整理规则与下载队列。无需手工编辑配置文件。</p></div></section>
              <form className="settings-layout" onSubmit={saveSettings}>
                <div className="settings-main-column">
                  <Card className="settings-panel storage-settings-panel"><div className="panel-heading"><div><span className="eyebrow">存储与迁移</span><h2>选择数据位置</h2></div><span className="settings-heading-icon"><Icon name="drive" size={18} /></span></div><div className="storage-callout"><span className="callout-icon"><Icon name="info" size={16} /></span><div><strong>把大型缓存和下载放在有足够空间的磁盘</strong><span>Telegram WebView2 profile、任务数据库、临时文件和下载文件都使用本机存储。迁移期间请保持应用运行。</span></div></div><div className="root-path-grid"><div className="root-path-card"><span>应用数据目录</span><strong title={appState?.settings.dataRoot}>{appState?.settings.dataRoot || editableSettings.dataRoot || "—"}</strong><small>Telegram WebView2 profile、任务数据库和任务记录</small></div><div className="root-path-card"><span>下载目录</span><strong title={appState?.settings.downloadRoot}>{appState?.settings.downloadRoot || editableSettings.downloadRoot || "—"}</strong><small>下载完成后的媒体文件</small></div></div><label className="field-label storage-select-label" htmlFor="storage-target">迁移应用数据位置</label><div className="storage-change-row"><input id="storage-target" value={storageTarget} onChange={(event) => setStorageTarget(event.target.value)} placeholder="选择磁盘或浏览目标目录" /><Button type="button" variant="secondary" onClick={() => void chooseDirectory("storage")}><Icon name="folder" size={14} />浏览</Button><Button type="button" variant="secondary" onClick={() => setStorageConfirm(true)} disabled={!storageTarget || storageTarget === appState?.settings.dataRoot || busy["storage-root"]}>迁移数据…</Button></div>{targetIsSystemVolume && <div className="storage-warning"><Icon name="info" size={14} />目标位于系统盘；建议把大型缓存和下载数据放在非系统卷。</div>}{availableVolumes.length ? <div className="volume-list">{preferredVolumes.map((volume) => <button type="button" className="volume-row volume-pick" key={volume.path} onClick={() => setStorageTarget(volume.path)}><span className="volume-drive"><Icon name="drive" size={15} />{volume.label || volume.path}</span><span>{formatBytes(volume.availableBytes)} 可用</span><div className="volume-bar"><i style={{ width: `${volume.totalBytes ? Math.min(100, (1 - volume.availableBytes / volume.totalBytes) * 100) : 0}%` }} /></div><small>{volume.path}{volume.isSystem ? " · 系统盘" : ""}</small></button>)}</div> : <div className="form-hint">尚未获取磁盘扫描结果；请刷新应用状态后重试。</div>}</Card>

                  <Card className="settings-panel"><div className="panel-heading"><div><span className="eyebrow">文件管理</span><h2>下载内容与命名</h2></div><span className="settings-heading-icon"><Icon name="folder" size={18} /></span></div><div className="field-stack"><label htmlFor="download-root">下载目录</label><div className="path-picker-row"><input id="download-root" value={editableSettings.downloadRoot} onChange={(event) => updateSettings({ downloadRoot: event.target.value })} placeholder="选择下载目录" /><Button type="button" variant="secondary" onClick={() => void chooseDirectory("downloads")}><Icon name="folder" size={14} />浏览</Button></div></div><div className="settings-two-col"><label>文件夹组织规则<input value={editableSettings.pathTemplate} onChange={(event) => updateSettings({ pathTemplate: event.target.value })} placeholder="{'{chat}/{year}/{month}'" /></label><label>文件命名规则<input value={editableSettings.fileNameTemplate} onChange={(event) => updateSettings({ fileNameTemplate: event.target.value })} placeholder="{'{date}_{message_id}_{name}'" /></label></div><label>媒体日期格式<input value={editableSettings.dateFormat} onChange={(event) => updateSettings({ dateFormat: event.target.value })} placeholder="%Y_%m" /><small>模板可用字段：{'{chat}'}、{'{year}'}、{'{month}'}、{'{date}'}、{'{message_id}'}、{'{name}'}、{'{caption}'}、{'{media_type}'}；日期采用 Rust/Chrono 写法，例如 %Y_%m 表示 2026_09。</small></label><div className="settings-two-col"><label>重复文件处理<select value={editableSettings.duplicatePolicy} onChange={(event) => updateSettings({ duplicatePolicy: event.target.value })}><option value="skip">跳过重复文件</option><option value="rename">保留两份并自动改名</option><option value="overwrite">覆盖已有文件</option></select></label><div className="switch-setting"><span><strong>保留未完成的临时文件</strong><small>取消任务时保留已抓取的分块数据，便于稍后重新开始时复用。</small></span><Switch checked={editableSettings.preservePartialFiles} onCheckedChange={(checked) => updateSettings({ preservePartialFiles: checked })} aria-label="保留未完成的临时文件" /></div></div></Card>


                  <Card className="settings-panel"><div className="panel-heading"><div><span className="eyebrow">下载队列</span><h2>队列管理</h2></div><span className="settings-heading-icon"><Icon name="bolt" size={18} /></span></div><div className="settings-two-col"><label>最多同时下载<input type="number" min="1" max="64" value={editableSettings.concurrency.maxFiles} onChange={(event) => updateConcurrency({ maxFiles: Number(event.target.value) })} /><small>同时抓取的文件数量，最多 64。</small></label><div className="switch-setting"><span><strong>自适应分块并发</strong><small>按实时投递率自动加减单文件并发（BBR 式探测 + 乘性退避），通常能跑满链路带宽。</small></span><Switch checked={editableSettings.concurrency.adaptive} onCheckedChange={(checked) => updateConcurrency({ adaptive: checked })} aria-label="自适应分块并发" /></div></div><div className="settings-two-col"><label>分块并发上限<input type="number" min="1" max="16" value={editableSettings.concurrency.perFileChunks} onChange={(event) => updateConcurrency({ perFileChunks: Number(event.target.value) })} /><small>自适应模式的并发上限（1–16）；关闭自适应后即固定并发数。</small></label><label>分块大小（KiB）<input type="number" min="64" max="4096" step="64" value={editableSettings.concurrency.chunkSizeKib} onChange={(event) => updateConcurrency({ chunkSizeKib: Number(event.target.value) })} /><small>抓取时会就近对齐到 64 / 128 / 256 / 512 / 1024 KiB。</small></label></div><div className="settings-two-col"><label>请求超时（秒）<input type="number" min="5" max="600" value={editableSettings.concurrency.requestTimeoutSeconds} onChange={(event) => updateConcurrency({ requestTimeoutSeconds: Number(event.target.value) })} /><small>单个分块请求的最长等待时间，5–600 秒。</small></label><label>自动重试次数<input type="number" min="0" max="20" value={editableSettings.concurrency.retries} onChange={(event) => updateConcurrency({ retries: Number(event.target.value) })} /></label></div><div className="settings-two-col"><label>带宽上限（KiB/s）<input type="number" min="0" value={editableSettings.concurrency.maxBandwidthKib} onChange={(event) => updateConcurrency({ maxBandwidthKib: Number(event.target.value) })} /><small>0 表示不限速。</small></label></div><div className="settings-note"><Icon name="info" size={15} /><span>{webviewLifecycleNote}</span></div></Card>

                  <Card className="settings-panel"><div className="panel-heading"><div><span className="eyebrow">界面</span><h2>语言偏好</h2></div><span className="settings-heading-icon"><Icon name="message" size={18} /></span></div><label htmlFor="ui-language">界面语言<select id="ui-language" value={editableSettings.language} onChange={(event) => updateSettings({ language: event.target.value })}><option value="zh-CN">简体中文</option><option value="en-US">English</option></select><small>语言偏好会随设置保存；当前界面文案为简体中文。</small></label></Card>

                  <Card className="settings-panel"><div className="panel-heading"><div><span className="eyebrow">软件更新</span><h2>版本与更新</h2></div><span className="settings-heading-icon"><Icon name="refresh" size={18} /></span></div>
                    <div className="update-status-row">
                      <div className="update-status-copy">
                        <strong>当前版本 {appState?.appVersion ?? "—"}</strong>
                        <small>
                          {updatePhase === "idle" && "从 GitHub Releases 检查并安装新版本。"}
                          {updatePhase === "checking" && "正在检查更新，请稍候…"}
                          {updatePhase === "current" && "已是最新版本。"}
                          {updatePhase === "available" && `发现新版本 ${updateRelease?.version ?? ""}，可立即下载并安装。`}
                          {updatePhase === "installing" && `正在下载并安装 ${updateRelease?.version ?? ""}…安装完成后应用会自动重启。`}
                          {updatePhase === "ready" && "更新已安装，重启应用后生效。"}
                        </small>
                      </div>
                      <div className="update-actions">
                        {(updatePhase === "idle" || updatePhase === "current") && <Button type="button" variant="secondary" onClick={() => void checkForUpdates()}><Icon name="refresh" size={14} />检查更新</Button>}
                        {updatePhase === "checking" && <Button type="button" variant="secondary" disabled><span className="spinner small" />正在检查…</Button>}
                        {updatePhase === "available" && <><Button type="button" variant="subtle" onClick={dismissUpdate}>稍后</Button><Button type="button" onClick={() => void installUpdate()}><Icon name="download" size={14} />下载并安装</Button></>}
                        {updatePhase === "installing" && <Button type="button" disabled><span className="spinner small" />正在安装…</Button>}
                        {updatePhase === "ready" && <Button type="button" onClick={() => void restartForUpdate()}><Icon name="refresh" size={14} />立即重启</Button>}
                      </div>
                    </div>
                    {updatePhase === "installing" && <div className="update-progress"><div className="progress-track"><i style={{ width: `${updateProgress}%` }} /></div><small>{updateProgress}%</small></div>}
                    <div className="settings-note"><Icon name="info" size={15} /><span>更新包由 GitHub Releases 提供，并已用应用内置公钥校验签名；校验失败时不会安装。</span></div>
                  </Card>

                </div>

                <aside className="settings-side-column"><Card className="safety-card"><Icon name="shield" size={17} /><strong>本地优先</strong><span>Telegram WebView2 profile、任务数据库和日志保存在你选择的数据目录。</span></Card></aside>
                <div className="settings-savebar"><span>{settingsSaved ? "已保存更改" : "修改只在点击保存后生效"}</span><Button type="submit" disabled={busy["save-settings"]}>{busy["save-settings"] ? "正在保存…" : "保存设置"}<Icon name="check" size={15} /></Button></div>
              </form>
            </>}
          </div>}
      </main>

      {toast && <div className={`toast toast-${toast.kind}`} role="status"><span className="toast-icon"><Icon name={toast.kind === "success" ? "check" : toast.kind === "error" ? "alert" : "info"} size={15} /></span><span>{toast.message}</span><button aria-label="关闭通知" onClick={() => setToast(null)}><Icon name="close" size={14} /></button></div>}

      <Dialog open={storageConfirm} onOpenChange={setStorageConfirm}><DialogContent aria-labelledby="storage-modal-title"><div className="modal-icon amber-icon"><Icon name="drive" size={21} /></div><span className="eyebrow">存储迁移</span><DialogTitle id="storage-modal-title">迁移应用数据？</DialogTitle><DialogDescription>客户端会将 WebView2 profile、任务数据库和应用缓存移至所选位置。请确保目标磁盘有足够空间，迁移期间不要关闭应用。完成后需要重启以切换 WebView2 配置目录。</DialogDescription><div className="modal-path"><Icon name="folder" size={15} />{storageTarget}</div><div className="modal-actions"><Button type="button" variant="subtle" onClick={() => setStorageConfirm(false)}>取消</Button><Button type="button" onClick={() => void applyStorageRoot()} disabled={busy["storage-root"]}>{busy["storage-root"] ? "正在迁移…" : "确认迁移"}</Button></div></DialogContent></Dialog>

    </div>
  );
}

export default App;

