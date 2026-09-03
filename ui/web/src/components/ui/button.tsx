import * as React from "react";
import { cn } from "../../lib/utils";

export type ButtonVariant = "primary" | "secondary" | "danger" | "ghost" | "outline";
export type ButtonSize = "sm" | "md" | "icon";

const variantClasses: Record<ButtonVariant, string> = {
    primary: "bg-accent text-white hover:bg-accent-2 shadow-sm disabled:bg-elev disabled:text-mute",
    secondary: "bg-elev text-fg hover:bg-bg-3 border border-line",
    danger: "bg-danger/85 text-white hover:bg-danger",
    ghost: "bg-transparent text-dim hover:bg-elev hover:text-fg",
    outline: "bg-transparent border border-line text-dim hover:bg-elev hover:text-fg",
};

const sizeClasses: Record<ButtonSize, string> = {
    sm: "h-7 px-2.5 text-xs rounded-md gap-1",
    md: "h-9 px-4 text-sm rounded-lg gap-1.5",
    icon: "h-8 w-8 rounded-lg",
};

export interface ButtonProps extends React.ButtonHTMLAttributes<HTMLButtonElement> {
    variant?: ButtonVariant;
    size?: ButtonSize;
}

export const Button = React.forwardRef<HTMLButtonElement, ButtonProps>(
    ({ className, variant = "secondary", size = "md", type = "button", ...props }, ref) => (
        <button
            ref={ref}
            type={type}
            className={cn(
                "inline-flex items-center justify-center font-medium transition-colors",
                "focus-visible:outline-2 focus-visible:outline-accent/60 disabled:cursor-not-allowed disabled:opacity-60",
                variantClasses[variant],
                sizeClasses[size],
                className,
            )}
            {...props}
        />
    ),
);
Button.displayName = "Button";
