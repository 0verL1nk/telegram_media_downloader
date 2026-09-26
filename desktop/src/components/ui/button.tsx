import { forwardRef, type ButtonHTMLAttributes } from "react";
import { cva, type VariantProps } from "class-variance-authority";
import { cn } from "../../lib/utils";

export const buttonVariants = cva("button", {
  variants: {
    variant: {
      default: "primary",
      primary: "primary",
      secondary: "secondary",
      outline: "secondary",
      ghost: "subtle",
      subtle: "subtle",
      light: "light",
      destructive: "secondary danger-button",
    },
    size: { default: "", sm: "button-sm", lg: "button-lg", icon: "button-icon" },
  },
  defaultVariants: { variant: "default", size: "default" },
});

export interface ButtonProps extends ButtonHTMLAttributes<HTMLButtonElement>, VariantProps<typeof buttonVariants> {}

export const Button = forwardRef<HTMLButtonElement, ButtonProps>(function Button(
  { className, variant, size, type = "button", ...props },
  ref,
) {
  return <button ref={ref} type={type} className={cn(buttonVariants({ variant, size }), className)} {...props} />;
});
