import * as React from "react";
import * as AlertDialogPrimitive from "@radix-ui/react-alert-dialog";
import { AlertTriangle, Check, Copy, Eye, EyeOff, RefreshCw, X } from "lucide-react";
import { useAppStore, selectedModel } from "../store";
import { EDITOR_TARGETS, maskToken, renderSnippet, type BridgeConfig } from "../lib/editors";
import { cn } from "../lib/utils";
import { Button } from "./ui/button";
import { Slider } from "./ui/slider";
import { confirmDialog } from "./ui/confirm-dialog";

const CTX_STEP = 2048;
const CTX_MAX = 65_536;

/// Copy button that acknowledges the copy for a beat. Falls back to selecting
/// nothing loudly — on a localhost page the Clipboard API is available (a
/// secure context), so failure here means the user denied permission.
function CopyButton({ value, label = "Copy" }: { value: string; label?: string }) {
    const [state, setState] = React.useState<"idle" | "done" | "failed">("idle");

    const copy = async () => {
        try {
            await navigator.clipboard.writeText(value);
            setState("done");
        } catch {
            setState("failed");
        }
        setTimeout(() => setState("idle"), 1600);
    };

    return (
        <Button variant="outline" size="sm" onClick={() => void copy()} title={`${label} to clipboard`}>
            {state === "done" ? <Check className="h-3.5 w-3.5 text-ok" /> : <Copy className="h-3.5 w-3.5" />}
            {state === "failed" ? "Copy failed" : state === "done" ? "Copied" : label}
        </Button>
    );
}

function Field({ label, children }: { label: string; children: React.ReactNode }) {
    return (
        <div className="flex flex-col gap-1.5">
            <div className="text-[10px] font-semibold uppercase tracking-wider text-mute">{label}</div>
            {children}
        </div>
    );
}

export function BridgePanel() {
    const bridge = useAppStore((s) => s.bridge);
    const open = useAppStore((s) => s.bridgePanelOpen);
    const setOpen = useAppStore((s) => s.setBridgePanelOpen);
    const setCtxTokens = useAppStore((s) => s.setBridgeCtxTokens);
    const rotate = useAppStore((s) => s.rotateBridgeKey);
    const model = useAppStore(selectedModel);

    const [targetId, setTargetId] = React.useState(EDITOR_TARGETS[0].id);
    const [revealed, setRevealed] = React.useState(false);
    // Local while dragging so we don't PUT on every tick of the slider.
    const [draftCtx, setDraftCtx] = React.useState<number | null>(null);

    React.useEffect(() => {
        if (!open) {
            setRevealed(false);
            setDraftCtx(null);
        }
    }, [open]);

    if (!bridge) return null;

    const target = EDITOR_TARGETS.find((t) => t.id === targetId) ?? EDITOR_TARGETS[0];
    const ctxTokens = draftCtx ?? bridge.ctx_tokens;
    const cfg: BridgeConfig = {
        baseUrl: bridge.base_url,
        token: bridge.token ?? "<enable the bridge to generate a token>",
        // Prefer whatever the user has selected in the sidebar; fall back to
        // the first model the drive can serve.
        model: model?.id ?? bridge.models[0] ?? "<no model on this drive>",
        ctxTokens,
    };
    const snippet = renderSnippet(target, cfg);

    const onRotate = async () => {
        const ok = await confirmDialog({
            title: "Rotate the bridge token?",
            description:
                "Every editor configured with the current token stops working until you paste the new one in.",
            confirmLabel: "Rotate",
            confirmVariant: "danger",
        });
        if (ok) await rotate();
    };

    return (
        <AlertDialogPrimitive.Root open={open} onOpenChange={setOpen}>
            <AlertDialogPrimitive.Portal>
                <AlertDialogPrimitive.Overlay className="fixed inset-0 z-50 bg-black/60 backdrop-blur-sm" />
                <AlertDialogPrimitive.Content
                    className={cn(
                        "fixed left-1/2 top-1/2 z-50 flex max-h-[85vh] w-full max-w-2xl -translate-x-1/2",
                        "-translate-y-1/2 flex-col overflow-hidden rounded-xl border border-line bg-bg-2 shadow-xl",
                    )}
                >
                    <div className="flex items-start gap-3 border-b border-line-soft p-5">
                        <div className="min-w-0 flex-1">
                            <AlertDialogPrimitive.Title className="text-base font-semibold text-fg">
                                Developer bridge
                            </AlertDialogPrimitive.Title>
                            <AlertDialogPrimitive.Description asChild>
                                <p className="mt-1 text-sm leading-relaxed text-dim">
                                    An OpenAI-compatible endpoint on this machine only. Point a coding assistant at it
                                    and your editor thinks with the model on this stick — nothing leaves the host.
                                </p>
                            </AlertDialogPrimitive.Description>
                        </div>
                        <AlertDialogPrimitive.Cancel asChild>
                            <Button variant="ghost" size="icon" aria-label="Close">
                                <X className="h-4 w-4" />
                            </Button>
                        </AlertDialogPrimitive.Cancel>
                    </div>

                    <div className="flex min-h-0 flex-1 flex-col gap-5 overflow-y-auto p-5">
                        <Field label="Endpoint / Base URL">
                            <div className="flex items-center gap-2">
                                <code className="min-w-0 flex-1 truncate rounded-md border border-line bg-bg px-2.5 py-1.5 font-mono text-xs text-fg">
                                    {bridge.base_url}
                                </code>
                                <CopyButton value={bridge.base_url} />
                            </div>
                        </Field>

                        <Field label="API key">
                            <div className="flex items-center gap-2">
                                <code className="min-w-0 flex-1 truncate rounded-md border border-line bg-bg px-2.5 py-1.5 font-mono text-xs text-fg">
                                    {bridge.token
                                        ? revealed
                                            ? bridge.token
                                            : maskToken(bridge.token)
                                        : "Enable the bridge to generate a token"}
                                </code>
                                {bridge.token && (
                                    <>
                                        <Button
                                            variant="outline"
                                            size="sm"
                                            onClick={() => setRevealed((v) => !v)}
                                            title={revealed ? "Hide token" : "Reveal token"}
                                            aria-label={revealed ? "Hide token" : "Reveal token"}
                                        >
                                            {revealed ? <EyeOff className="h-3.5 w-3.5" /> : <Eye className="h-3.5 w-3.5" />}
                                        </Button>
                                        <CopyButton value={bridge.token} />
                                        <Button
                                            variant="outline"
                                            size="sm"
                                            onClick={() => void onRotate()}
                                            title="Generate a new token and invalidate the old one"
                                        >
                                            <RefreshCw className="h-3.5 w-3.5" />
                                        </Button>
                                    </>
                                )}
                            </div>
                            <p className="text-[11px] text-mute">
                                Stored in plaintext at <code className="font-mono">.usbuddy/bridge-token</code> on this
                                drive so your editor keeps working next time you plug it in.
                            </p>
                        </Field>

                        <Field label={`Context window — ${ctxTokens.toLocaleString()} tokens`}>
                            <Slider
                                min={bridge.min_ctx_tokens}
                                max={CTX_MAX}
                                step={CTX_STEP}
                                value={[Math.min(ctxTokens, CTX_MAX)]}
                                onValueChange={([v]) => setDraftCtx(v)}
                                onValueCommit={([v]) => {
                                    setDraftCtx(null);
                                    void setCtxTokens(v);
                                }}
                            />
                            <p className="text-[11px] text-mute">
                                Coding agents send far more context than chat. Capped at load time to the model's
                                trained length, and the RAM advisor still refuses anything that would swap to disk.
                                Changing this reloads the model on your editor's next request.
                            </p>
                        </Field>

                        <Field label="Model name to configure">
                            <div className="flex items-center gap-2">
                                <code className="min-w-0 flex-1 truncate rounded-md border border-line bg-bg px-2.5 py-1.5 font-mono text-xs text-fg">
                                    {cfg.model}
                                </code>
                                <CopyButton value={cfg.model} />
                            </div>
                            <p className="text-[11px] text-mute">
                                The chat UI and your editor share one engine — naming a different model in your editor
                                swaps the running one.
                            </p>
                        </Field>

                        <Field label="Set up your editor">
                            <div className="flex flex-wrap gap-1.5">
                                {EDITOR_TARGETS.map((t) => (
                                    <button
                                        key={t.id}
                                        type="button"
                                        onClick={() => setTargetId(t.id)}
                                        className={cn(
                                            "rounded-md border px-2.5 py-1 text-xs transition-colors",
                                            t.id === targetId
                                                ? "border-accent/60 bg-accent/15 text-fg"
                                                : "border-line text-dim hover:bg-elev hover:text-fg",
                                        )}
                                    >
                                        {t.label}
                                    </button>
                                ))}
                            </div>

                            <div className="mt-1 text-[11px] text-mute">{target.where}</div>

                            {target.blocked && (
                                <div className="flex gap-2 rounded-md border border-warn/30 bg-warn/10 p-2.5 text-xs leading-relaxed text-dim">
                                    <AlertTriangle className="mt-0.5 h-3.5 w-3.5 shrink-0 text-warn" />
                                    <span>{target.blocked}</span>
                                </div>
                            )}

                            <ol className="ml-4 list-decimal space-y-1 text-xs leading-relaxed text-dim">
                                {target.steps.map((step) => (
                                    <li key={step}>{step}</li>
                                ))}
                            </ol>

                            {snippet && (
                                <div className="flex flex-col gap-1.5">
                                    <pre className="overflow-x-auto rounded-md border border-line bg-bg p-3 font-mono text-[11px] leading-relaxed text-fg">
                                        {snippet}
                                    </pre>
                                    <div className="flex justify-end">
                                        <CopyButton value={snippet} label="Copy config" />
                                    </div>
                                </div>
                            )}
                        </Field>
                    </div>
                </AlertDialogPrimitive.Content>
            </AlertDialogPrimitive.Portal>
        </AlertDialogPrimitive.Root>
    );
}
