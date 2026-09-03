import * as React from "react";
import * as AlertDialogPrimitive from "@radix-ui/react-alert-dialog";
import { cn } from "../../lib/utils";
import { Button, type ButtonVariant } from "./button";

/// Promise-based confirm dialog replacing window.confirm(). Render one
/// <ConfirmDialogHost /> near the root; call confirmDialog(...) anywhere.

interface ConfirmOptions {
    title: string;
    description: React.ReactNode;
    confirmLabel?: string;
    cancelLabel?: string;
    confirmVariant?: ButtonVariant;
}

type Pending = ConfirmOptions & { resolve: (ok: boolean) => void };

let openConfirm: ((p: Pending) => void) | null = null;

export function confirmDialog(options: ConfirmOptions): Promise<boolean> {
    return new Promise((resolve) => {
        if (openConfirm) {
            openConfirm({ ...options, resolve });
        } else {
            resolve(false);
        }
    });
}

export function ConfirmDialogHost() {
    const [pending, setPending] = React.useState<Pending | null>(null);

    React.useEffect(() => {
        openConfirm = setPending;
        return () => {
            openConfirm = null;
        };
    }, []);

    const close = (ok: boolean) => {
        pending?.resolve(ok);
        setPending(null);
    };

    return (
        <AlertDialogPrimitive.Root open={pending !== null} onOpenChange={(o) => !o && close(false)}>
            <AlertDialogPrimitive.Portal>
                <AlertDialogPrimitive.Overlay className="fixed inset-0 z-50 bg-black/60 backdrop-blur-sm" />
                <AlertDialogPrimitive.Content
                    className={cn(
                        "fixed left-1/2 top-1/2 z-50 w-full max-w-md -translate-x-1/2 -translate-y-1/2",
                        "rounded-xl border border-line bg-bg-2 p-6 shadow-xl",
                    )}
                >
                    <AlertDialogPrimitive.Title className="text-base font-semibold text-fg">
                        {pending?.title}
                    </AlertDialogPrimitive.Title>
                    <AlertDialogPrimitive.Description asChild>
                        <div className="mt-2 text-sm leading-relaxed text-dim">{pending?.description}</div>
                    </AlertDialogPrimitive.Description>
                    <div className="mt-5 flex justify-end gap-2">
                        <AlertDialogPrimitive.Cancel asChild>
                            <Button variant="ghost">{pending?.cancelLabel ?? "Cancel"}</Button>
                        </AlertDialogPrimitive.Cancel>
                        <AlertDialogPrimitive.Action asChild>
                            <Button variant={pending?.confirmVariant ?? "primary"} onClick={() => close(true)}>
                                {pending?.confirmLabel ?? "Confirm"}
                            </Button>
                        </AlertDialogPrimitive.Action>
                    </div>
                </AlertDialogPrimitive.Content>
            </AlertDialogPrimitive.Portal>
        </AlertDialogPrimitive.Root>
    );
}
