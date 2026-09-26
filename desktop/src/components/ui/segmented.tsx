import type { ReactNode } from "react";
import { cn } from "../../lib/utils";

export type SegmentedOption<T extends string> = { value: T; label: ReactNode };

/** 分段控件(样稿 .seg):少量互斥选项,选中 = 蓝软底蓝字。 */
export function Segmented<T extends string>({
  value,
  options,
  onChange,
  className,
  ariaLabel,
}: {
  value: T;
  options: SegmentedOption<T>[];
  onChange: (value: T) => void;
  className?: string;
  ariaLabel?: string;
}) {
  return (
    <div className={cn("seg", className)} role="radiogroup" aria-label={ariaLabel}>
      {options.map((option) => (
        <button
          key={option.value}
          type="button"
          role="radio"
          aria-checked={option.value === value}
          className={option.value === value ? "on" : undefined}
          onClick={() => onChange(option.value)}
        >
          {option.label}
        </button>
      ))}
    </div>
  );
}
