import * as ProgressPrimitive from "@radix-ui/react-progress";
import type { ComponentProps } from "react";
import { cn } from "../../lib/utils";

type ProgressProps = ComponentProps<typeof ProgressPrimitive.Root> & {
  tone?: "default" | "success" | "danger";
  /** 面板行用的矮条。 */
  size?: "default" | "sm";
};

/** 任务进度条:Radix Progress + token 皮肤(DESIGN.md §3 进度条)。 */
export function Progress({ className, value, tone = "default", size = "default", ...props }: ProgressProps) {
  const percent = Math.min(100, Math.max(0, value ?? 0));
  return (
    <ProgressPrimitive.Root
      className={cn(size === "sm" ? "pr-track" : "track", className)}
      value={percent}
      {...props}
    >
      <ProgressPrimitive.Indicator
        className={cn(size === "sm" ? "pr-fill" : "fill", tone === "success" && "ok", tone === "danger" && "bad")}
        style={{ width: `${percent}%` }}
      />
    </ProgressPrimitive.Root>
  );
}
