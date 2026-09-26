import { getCurrentWindow } from "@tauri-apps/api/window";
import { Icon } from "../Icon";

/** 自绘标题栏 36px:拖拽区 + 窗口按钮(DESIGN.md §1)。 */
export function TitleBar({ caption, maximized, onError }: { caption?: string; maximized?: boolean; onError: (message: string) => void }) {
  const nativeWindow = getCurrentWindow();
  return (
    <header className="titlebar">
      <div
        className="tb-left"
        data-tauri-drag-region
        onDoubleClick={() => void nativeWindow.toggleMaximize().catch((error) => onError(String(error)))}
      >
        <span className="tb-mark" aria-hidden />
        <span data-tauri-drag-region>Telegram Media Downloader</span>
        {caption ? <span className="tb-caption" data-tauri-drag-region>· {caption}</span> : null}
      </div>
      <div className="tb-controls">
        <button type="button" className="tb-btn" aria-label="最小化" title="最小化"
          onClick={() => void nativeWindow.minimize().catch((error) => onError(String(error)))}>
          <Icon name="minimize" size={15} />
        </button>
        <button type="button" className="tb-btn" aria-label={maximized ? "还原" : "最大化"} title={maximized ? "还原" : "最大化"}
          onClick={() => void nativeWindow.toggleMaximize().catch((error) => onError(String(error)))}>
          <Icon name={maximized ? "restore" : "maximize"} size={15} />
        </button>
        <button type="button" className="tb-btn close" aria-label="关闭窗口(下载继续)" title="关闭窗口(下载继续)"
          onClick={() => void nativeWindow.close().catch((error) => onError(String(error)))}>
          <Icon name="close" size={15} />
        </button>
      </div>
    </header>
  );
}
