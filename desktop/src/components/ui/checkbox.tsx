import * as CheckboxPrimitive from "@radix-ui/react-checkbox";
import type { ComponentProps } from "react";
import { Check } from "lucide-react";
import { cn } from "../../lib/utils";

export function Checkbox({ className, ...props }: ComponentProps<typeof CheckboxPrimitive.Root>) {
  return <CheckboxPrimitive.Root className={cn("check-box radix-check", className)} {...props}><CheckboxPrimitive.Indicator><Check size={12} /></CheckboxPrimitive.Indicator></CheckboxPrimitive.Root>;
}
