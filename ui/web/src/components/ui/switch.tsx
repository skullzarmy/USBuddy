import * as React from "react";
import * as SwitchPrimitive from "@radix-ui/react-switch";
import { cn } from "../../lib/utils";

export const Switch = React.forwardRef<
    React.ElementRef<typeof SwitchPrimitive.Root>,
    React.ComponentPropsWithoutRef<typeof SwitchPrimitive.Root>
>(({ className, ...props }, ref) => (
    <SwitchPrimitive.Root
        ref={ref}
        className={cn(
            "inline-flex h-5 w-9 shrink-0 cursor-pointer items-center rounded-full border border-line transition-colors",
            "data-[state=checked]:bg-accent data-[state=unchecked]:bg-bg-3",
            "focus-visible:outline-2 focus-visible:outline-accent/60 disabled:cursor-not-allowed disabled:opacity-50",
            className,
        )}
        {...props}
    >
        <SwitchPrimitive.Thumb className="block h-4 w-4 translate-x-0.5 rounded-full bg-fg shadow transition-transform data-[state=checked]:translate-x-4.5" />
    </SwitchPrimitive.Root>
));
Switch.displayName = "Switch";
