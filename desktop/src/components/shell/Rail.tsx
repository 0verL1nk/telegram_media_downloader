import { Icon } from "../Icon";
import { IconTip } from "../ui/tooltip";

export type AppView = "telegram" | "tasks" | "logs" | "settings";

/** 图标栏 64px(DESIGN.md §1):44×44 按钮、active 蓝软底、任务角标。 */
export function Rail({
  view,
  onNavigate,
  activeCount,
}: {
  view: AppView;
  onNavigate: (view: AppView) => void;
  activeCount: number;
}) {
  return (
    <nav className="rail" aria-label="主导航">
      <IconTip label="Telegram Web">
        <button type="button" className={"rail-btn" + (view === "telegram" ? " active" : "")} aria-label="Telegram Web" aria-current={view === "telegram" ? "page" : undefined} onClick={() => onNavigate("telegram")}>
          <Icon name="telegram" size={19} />
        </button>
      </IconTip>
      <IconTip label="任务">
        <button type="button" className={"rail-btn" + (view === "tasks" ? " active" : "")} aria-label={`任务${activeCount > 0 ? `,${activeCount} 项进行中` : ""}`} aria-current={view === "tasks" ? "page" : undefined} onClick={() => onNavigate("tasks")}>
          <Icon name="download" size={19} />
          {activeCount > 0 ? <span className="badge">{activeCount > 99 ? "99+" : activeCount}</span> : null}
        </button>
      </IconTip>
      <IconTip label="日志">
        <button type="button" className={"rail-btn" + (view === "logs" ? " active" : "")} aria-label="日志" aria-current={view === "logs" ? "page" : undefined} onClick={() => onNavigate("logs")}>
          <Icon name="logs" size={19} />
        </button>
      </IconTip>
      <div className="rail-spacer" />
      <div className="rail-sep" aria-hidden />
      <IconTip label="设置">
        <button type="button" className={"rail-btn" + (view === "settings" ? " active" : "")} aria-label="设置" aria-current={view === "settings" ? "page" : undefined} onClick={() => onNavigate("settings")}>
          <Icon name="settings" size={19} />
        </button>
      </IconTip>
    </nav>
  );
}
