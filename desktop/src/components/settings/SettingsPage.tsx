import { useState, type ReactNode } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { Button } from "../ui/button";
import { Card } from "../ui/card";
import { Input } from "../ui/input";
import { Segmented } from "../ui/segmented";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "../ui/select";
import { Switch } from "../ui/switch";
import type { AppState, Settings, StorageOption } from "../../lib/api";
import { formatBytes } from "../../lib/format";

type CategoryId = "storage" | "concurrency" | "appearance" | "updates" | "tray" | "about";

const CATEGORIES: { id: CategoryId; label: string }[] = [
  { id: "storage", label: "下载与存储" },
  { id: "concurrency", label: "并发与限速" },
  { id: "appearance", label: "外观与语言" },
  { id: "updates", label: "软件更新" },
  { id: "tray", label: "托盘与启动" },
  { id: "about", label: "关于" },
];

export type ThemePreference = "system" | "light" | "dark";

export interface UpdateUiState {
  phase: "idle" | "checking" | "current" | "available" | "installing" | "ready";
  version?: string | null;
  progress: number;
  onCheck: () => void;
  onInstall: () => void;
  onRestart: () => void;
}

/** 步进器(DESIGN.md §3):34/44/34 三段。 */
function Stepper({ value, min, max, step = 1, onChange, suffix }: {
  value: number; min: number; max: number; step?: number; onChange: (value: number) => void; suffix?: string;
}) {
  const clamp = (next: number) => Math.min(max, Math.max(min, next));
  return (
    <span className="stepper">
      <button type="button" aria-label="减少" onClick={() => onChange(clamp(value - step))}>−</button>
      <span className="val">{value}{suffix ? <small style={{ fontWeight: 400, color: "var(--text-3)" }}>{suffix}</small> : null}</span>
      <button type="button" aria-label="增加" onClick={() => onChange(clamp(value + step))}>+</button>
    </span>
  );
}

function Field({ label, hint, children }: { label: string; hint?: ReactNode; children: ReactNode }) {
  return (
    <div className="field">
      <label>{label}</label>
      <div className="field-control">
        {children}
        {hint ? <div className="hint">{hint}</div> : null}
      </div>
    </div>
  );
}

export function SettingsPage({
  settings,
  appState,
  onSettingsChange,
  onConcurrencyChange,
  onSave,
  onReset,
  saving,
  saved,
  storageTarget,
  onStorageTargetChange,
  onChooseDirectory,
  onMigrate,
  targetIsSystemVolume,
  theme,
  onThemeChange,
  update,
  notifyError,
}: {
  settings: Settings;
  appState: AppState;
  onSettingsChange: (update: Partial<Settings>) => void;
  onConcurrencyChange: (update: Partial<Settings["concurrency"]>) => void;
  onSave: () => void;
  onReset: () => void;
  saving: boolean;
  saved: boolean;
  storageTarget: string;
  onStorageTargetChange: (value: string) => void;
  onChooseDirectory: (target: "storage" | "downloads") => void;
  onMigrate: () => void;
  targetIsSystemVolume: boolean;
  theme: ThemePreference;
  onThemeChange: (theme: ThemePreference) => void;
  update: UpdateUiState;
  notifyError: (message: string) => void;
}) {
  const [category, setCategory] = useState<CategoryId>("storage");
  const concurrency = settings.concurrency;
  const volumes: StorageOption[] = [...appState.storageOptions].sort((left, right) => Number(left.isSystem) - Number(right.isSystem));

  const saveFoot = (label: string) => (
    <div className="card-foot">
      <span style={{ marginRight: "auto", alignSelf: "center", color: "var(--text-3)", fontSize: 11.5 }}>
        {saved ? "已保存 ✓" : label}
      </span>
      <Button variant="ghost" onClick={onReset} disabled={saving}>重置</Button>
      <Button variant="primary" onClick={onSave} disabled={saving}>{saving ? "保存中…" : "应用"}</Button>
    </div>
  );

  return (
    <main className="content">
      <nav className="set-nav" aria-label="设置分类">
        <div className="set-title">设置</div>
        {CATEGORIES.map((entry) => (
          <button
            key={entry.id}
            type="button"
            className={"set-item" + (category === entry.id ? " on" : "")}
            aria-current={category === entry.id ? "true" : undefined}
            onClick={() => setCategory(entry.id)}
          >
            {entry.label}
          </button>
        ))}
      </nav>
      <div className="set-main">
        {category === "storage" ? (
          <>
            <Card>
              <h3>下载与存储</h3>
              <p className="desc">媒体文件保存位置与命名规则,修改后对新任务立即生效。</p>
              <Field label="下载目录">
                <div style={{ display: "flex", gap: 8 }}>
                  <Input value={settings.downloadRoot} onChange={(event) => onSettingsChange({ downloadRoot: event.target.value })} placeholder="选择下载目录" />
                  <Button variant="secondary" onClick={() => onChooseDirectory("downloads")}>浏览</Button>
                </div>
              </Field>
              <Field label="重复文件">
                <Segmented
                  ariaLabel="重复文件处理"
                  value={settings.duplicatePolicy}
                  onChange={(value) => onSettingsChange({ duplicatePolicy: value })}
                  options={[
                    { value: "skip", label: "跳过" },
                    { value: "rename", label: "重命名" },
                    { value: "overwrite", label: "覆盖" },
                  ]}
                />
              </Field>
              <Field label="保留未完成分块" hint="取消任务时保留已抓取分块,便于稍后从断点继续。">
                <Switch checked={settings.preservePartialFiles} onCheckedChange={(checked) => onSettingsChange({ preservePartialFiles: checked })} aria-label="保留未完成的临时文件" />
              </Field>
              <Field label="文件夹组织规则">
                <Input value={settings.pathTemplate} onChange={(event) => onSettingsChange({ pathTemplate: event.target.value })} placeholder="{chat}/{year}/{month}" />
              </Field>
              <Field label="文件命名规则">
                <Input value={settings.fileNameTemplate} onChange={(event) => onSettingsChange({ fileNameTemplate: event.target.value })} placeholder="{date}_{message_id}_{name}" />
              </Field>
              <Field label="媒体日期格式" hint={"{chat} {year} {month} {date} {message_id} {name} {caption} {media_type} · Chrono 写法,如 %Y_%m"}>
                <Input value={settings.dateFormat} onChange={(event) => onSettingsChange({ dateFormat: event.target.value })} placeholder="%Y_%m" />
              </Field>
              {saveFoot("修改后点应用生效")}
            </Card>

            <Card>
              <h3>数据位置与迁移</h3>
              <p className="desc">任务数据库、登录 profile 与日志。迁移期间请保持应用运行,完成后会自动重启。</p>
              <Field label="应用数据目录">
                <div className="input"><span className="path" title={appState.settings.dataRoot}>{appState.settings.dataRoot || "—"}</span></div>
              </Field>
              <Field label="迁移到">
                <div style={{ display: "flex", gap: 8 }}>
                  <Input value={storageTarget} onChange={(event) => onStorageTargetChange(event.target.value)} placeholder="选择磁盘或输入目标目录" />
                  <Button variant="secondary" onClick={() => onChooseDirectory("storage")}>浏览</Button>
                  <Button variant="secondary" onClick={onMigrate} disabled={!storageTarget || storageTarget === appState.settings.dataRoot}>迁移数据…</Button>
                </div>
              </Field>
              {targetIsSystemVolume ? (
                <p className="desc" style={{ color: "var(--warn)" }}>目标位于系统盘;建议把大型缓存与下载放在非系统卷。</p>
              ) : null}
              {volumes.length > 0 ? (
                <div className="volume-list">
                  {volumes.map((volume) => (
                    <button key={volume.path} type="button" className="volume-row" onClick={() => onStorageTargetChange(volume.path)}>
                      <span className="drive">{volume.label || volume.path}</span>
                      <span>{formatBytes(volume.availableBytes)} 可用</span>
                      <span className="volume-bar"><i style={{ width: `${volume.totalBytes ? Math.min(100, (1 - volume.availableBytes / volume.totalBytes) * 100) : 0}%` }} /></span>
                      <small style={{ marginLeft: "auto", color: "var(--text-3)" }}>{volume.path}{volume.isSystem ? " · 系统盘" : ""}</small>
                    </button>
                  ))}
                </div>
              ) : null}
            </Card>
          </>
        ) : null}

        {category === "concurrency" ? (
          <Card>
            <h3>并发与限速</h3>
            <p className="desc">页面抓取并发与分块策略;限速 0 表示不限。</p>
            <Field label="同时下载文件数" hint="最多 64">
              <Stepper value={concurrency.maxFiles} min={1} max={64} onChange={(value) => onConcurrencyChange({ maxFiles: value })} />
            </Field>
            <Field label="自适应分块并发" hint="按实时投递率自动加减单文件并发(BBR 式探测 + 乘性退避),通常能跑满链路。">
              <Switch checked={concurrency.adaptive} onCheckedChange={(checked) => onConcurrencyChange({ adaptive: checked })} aria-label="自适应分块并发" />
            </Field>
            <Field label="失败自动重试" hint="网络中断等情况按指数退避自动重试(5 秒起,最多 5 分钟一次);URL 失效等永久性失败不重试。">
              <Switch checked={concurrency.autoRetry} onCheckedChange={(checked) => onConcurrencyChange({ autoRetry: checked })} aria-label="失败自动重试" />
            </Field>
            <Field label="分块并发上限" hint="自适应模式的并发上限(1–16);关掉自适应后即固定并发数。">
              <Stepper value={concurrency.perFileChunks} min={1} max={16} onChange={(value) => onConcurrencyChange({ perFileChunks: value })} />
            </Field>
            <Field label="分块大小" hint="自适应开启且已学到链路数据时按 BDP 自动选档;这里的值是兜底。">
              <Select value={String(concurrency.chunkSizeKib)} onValueChange={(value) => onConcurrencyChange({ chunkSizeKib: Number(value) })}>
                <SelectTrigger aria-label="分块大小"><SelectValue /></SelectTrigger>
                <SelectContent>
                  {[64, 128, 256, 512, 1024].map((kib) => (
                    <SelectItem key={kib} value={String(kib)}>{kib} KiB</SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </Field>
            <Field label="请求超时" hint="单个分块请求的最长等待时间">
              <Stepper value={concurrency.requestTimeoutSeconds} min={5} max={600} step={5} suffix="s" onChange={(value) => onConcurrencyChange({ requestTimeoutSeconds: value })} />
            </Field>
            <Field label="自动重试次数">
              <Stepper value={concurrency.retries} min={0} max={20} onChange={(value) => onConcurrencyChange({ retries: value })} />
            </Field>
            <Field label="带宽上限" hint="0 表示不限速">
              <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
                <Input style={{ maxWidth: 160 }} type="number" min={0} value={concurrency.maxBandwidthKib} onChange={(event) => onConcurrencyChange({ maxBandwidthKib: Number(event.target.value) })} />
                <span style={{ color: "var(--text-3)", fontSize: 12 }}>KiB/s</span>
              </div>
            </Field>
            {saveFoot("修改后点应用生效")}
          </Card>
        ) : null}

        {category === "appearance" ? (
          <Card>
            <h3>外观与语言</h3>
            <p className="desc">主题跟随系统或手动指定;界面语言随设置保存。</p>
            <Field label="主题">
              <Segmented
                ariaLabel="主题"
                value={theme}
                onChange={onThemeChange}
                options={[
                  { value: "system", label: "跟随系统" },
                  { value: "light", label: "浅色" },
                  { value: "dark", label: "深色" },
                ]}
              />
            </Field>
            <Field label="界面语言" hint="当前界面文案为简体中文">
              <Segmented
                ariaLabel="界面语言"
                value={settings.language}
                onChange={(value) => onSettingsChange({ language: value })}
                options={[
                  { value: "zh-CN", label: "简体中文" },
                  { value: "en-US", label: "English" },
                ]}
              />
            </Field>
            {saveFoot("语言偏好保存后生效")}
          </Card>
        ) : null}

        {category === "updates" ? (
          <Card>
            <h3>软件更新</h3>
            <p className="desc">从 GitHub Releases 检查并安装新版本;安装完成后应用会自动重启。</p>
            <div className="update-row">
              <div className="update-copy">
                <strong>当前版本 {appState.appVersion}</strong>
                <small>
                  {update.phase === "idle" ? "点击检查更新。" : null}
                  {update.phase === "checking" ? "正在检查更新,请稍候…" : null}
                  {update.phase === "current" ? "已是最新版本。" : null}
                  {update.phase === "available" ? `发现新版本 ${update.version ?? ""}。` : null}
                  {update.phase === "installing" ? `正在下载并安装… ${update.progress}%` : null}
                  {update.phase === "ready" ? "更新已安装,重启后生效。" : null}
                </small>
              </div>
              <div style={{ display: "flex", gap: 8, flex: "none" }}>
                {update.phase === "available" ? <Button variant="primary" onClick={update.onInstall}>下载并安装</Button> : null}
                {update.phase === "ready" ? <Button variant="primary" onClick={update.onRestart}>立即重启</Button> : null}
                {update.phase === "available" || update.phase === "ready" || update.phase === "installing" ? null : (
                  <Button variant="secondary" onClick={update.onCheck} disabled={update.phase === "checking"}>
                    {update.phase === "checking" ? "检查中…" : "检查更新"}
                  </Button>
                )}
              </div>
            </div>
            {update.phase === "installing" ? <div className="update-progress"><i style={{ width: `${update.progress}%` }} /></div> : null}
          </Card>
        ) : null}

        {category === "tray" ? (
          <Card>
            <h3>托盘与启动</h3>
            <p className="desc">关闭窗口的行为与开机启动。</p>
            <Field label="关闭窗口时">
              <div className="switch-copy">
                <strong>驻留托盘,下载继续</strong>
                <small>点击托盘图标可恢复窗口;托盘菜单可打开下载目录或退出。</small>
              </div>
            </Field>
            <Field label="开机自动启动" hint="规划中,将在后续版本提供。">
              <span style={{ color: "var(--text-3)", fontSize: 12.5 }}>尚未提供</span>
            </Field>
          </Card>
        ) : null}

        {category === "about" ? (
          <Card>
            <h3>关于</h3>
            <p className="desc">Telegram Media Downloader 桌面客户端 —— 下载由 Telegram 页面内的抓取完成,Rust 负责任务与落盘。</p>
            <Field label="版本"><span style={{ fontSize: 13 }}>v{appState.appVersion}</span></Field>
            <Field label="项目主页">
              <Button variant="secondary" onClick={() => {
                void openUrl("https://github.com/0verL1nk/telegram_media_downloader").catch((error) => notifyError(String(error)));
              }}>在浏览器中打开</Button>
            </Field>
            <Field label="数据目录"><div className="input"><span className="path" title={appState.settings.dataRoot}>{appState.settings.dataRoot || "—"}</span></div></Field>
            <Field label="下载目录"><div className="input"><span className="path" title={settings.downloadRoot}>{settings.downloadRoot || "—"}</span></div></Field>
            <p className="desc" style={{ marginTop: 12, marginBottom: 0 }}>
              下载由 Telegram 页面内的抓取持续进行:媒体保持打开时,暂停/继续立即生效。应用关闭或页面中断后,
              任务与已下载分块会保留 —— 重新打开该媒体再点下载即可从已完成分块继续。
            </p>
            <p className="desc" style={{ marginTop: 6, marginBottom: 0 }}>下载内容仅保存在本机;Telegram 登录态保存在应用数据目录内的 WebView profile 中。</p>
          </Card>
        ) : null}
      </div>
    </main>
  );
}
