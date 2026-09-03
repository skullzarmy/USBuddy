import { Play, Power, Square, Trash2, Usb } from "lucide-react";
import { useAppStore, selectedModel } from "../store";
import { gib } from "../lib/utils";
import { Button } from "./ui/button";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "./ui/select";
import { Slider } from "./ui/slider";
import { confirmDialog } from "./ui/confirm-dialog";
import { RamBadge } from "./RamBadge";

const CONTEXT_STEP = 512;

function SectionLabel({ children }: { children: React.ReactNode }) {
    return <div className="text-[10px] font-semibold uppercase tracking-wider text-mute">{children}</div>;
}

export function Sidebar() {
    const status = useAppStore((s) => s.status);
    const models = useAppStore((s) => s.models);
    const selectedModelId = useAppStore((s) => s.selectedModelId);
    const contextTokens = useAppStore((s) => s.contextTokens);
    const llamaRunning = useAppStore((s) => s.llamaRunning);
    const launching = useAppStore((s) => s.launching);
    const launchStatus = useAppStore((s) => s.launchStatus);
    const savedChats = useAppStore((s) => s.savedChats);
    const currentChatId = useAppStore((s) => s.currentChatId);
    const selectModel = useAppStore((s) => s.selectModel);
    const setContextTokens = useAppStore((s) => s.setContextTokens);
    const launch = useAppStore((s) => s.launch);
    const stop = useAppStore((s) => s.stop);
    const loadChat = useAppStore((s) => s.loadChat);
    const deleteChat = useAppStore((s) => s.deleteChat);
    const quit = useAppStore((s) => s.quit);
    const model = useAppStore(selectedModel);

    // Cap the slider to the model's trained context length when known;
    // fall back to 32K so the user can still push an unknown arch.
    const cap = model?.arch?.context_length || 32_768;
    const snappedCap = Math.max(CONTEXT_STEP, Math.floor(cap / CONTEXT_STEP) * CONTEXT_STEP);

    const onDeleteChat = async (id: string, title: string) => {
        const ok = await confirmDialog({
            title: "Delete chat",
            description: `Delete "${title || "Untitled"}"? This cannot be undone.`,
            confirmLabel: "Delete",
            confirmVariant: "danger",
        });
        if (ok) await deleteChat(id);
    };

    const onQuit = async () => {
        const ok = await confirmDialog({
            title: "Quit USBuddy?",
            description: "This stops the runtime entirely.",
            confirmLabel: "Quit",
            confirmVariant: "danger",
        });
        if (ok) await quit(false);
    };

    const onEject = async () => {
        const ok = await confirmDialog({
            title: "Quit & eject drive?",
            description:
                "The runtime stops and the OS ejects the drive a moment later. " +
                "Wait for the drive to disappear before unplugging it.",
            confirmLabel: "Quit & eject",
            confirmVariant: "danger",
        });
        if (ok) await quit(true);
    };

    return (
        <aside className="flex h-full w-75 shrink-0 flex-col gap-5 overflow-y-auto border-r border-line bg-linear-to-b from-bg-2 to-[#12161d] p-4">
            {/* brand */}
            <div className="flex items-center gap-3">
                <img src="/assets/icon.png" alt="" className="h-10 w-10 shrink-0 drop-shadow" />
                <div className="min-w-0 leading-tight">
                    <h1 className="bg-linear-135 from-usb-blue to-[#7eb6ff] bg-clip-text text-lg font-bold text-transparent">
                        USBuddy
                    </h1>
                    <span className="text-[10px] font-semibold uppercase tracking-widest text-mute">
                        portable · offline · yours
                    </span>
                </div>
            </div>

            {/* model picker */}
            <div className="flex flex-col gap-2">
                <SectionLabel>Model</SectionLabel>
                {models.length === 0 ? (
                    <p className="text-xs text-dim">
                        No models on this drive. Drop a <code className="font-mono">.gguf</code> into{" "}
                        <code className="font-mono">models/</code>.
                    </p>
                ) : (
                    <Select value={selectedModelId ?? undefined} onValueChange={selectModel}>
                        <SelectTrigger>
                            <SelectValue placeholder="Select a model" />
                        </SelectTrigger>
                        <SelectContent>
                            {models.map((m) => (
                                <SelectItem key={m.id} value={m.id}>
                                    {m.label} ({m.profile}
                                    {m.sizeBytes ? ` · ${gib(m.sizeBytes)} GiB` : ""})
                                </SelectItem>
                            ))}
                        </SelectContent>
                    </Select>
                )}
            </div>

            {/* context length */}
            <div className="flex flex-col gap-2.5">
                <SectionLabel>Context length</SectionLabel>
                <div className="flex items-center gap-3">
                    <Slider
                        min={CONTEXT_STEP}
                        max={snappedCap}
                        step={CONTEXT_STEP}
                        value={[Math.min(contextTokens, snappedCap)]}
                        onValueChange={([v]) => setContextTokens(v)}
                        disabled={models.length === 0}
                    />
                    <span className="w-14 shrink-0 text-right font-mono text-xs text-dim">
                        {Math.min(contextTokens, snappedCap).toLocaleString()}
                    </span>
                </div>
                <RamBadge />
            </div>

            {/* launch / stop */}
            <div className="flex flex-col gap-2">
                {llamaRunning ? (
                    <Button variant="danger" onClick={stop}>
                        <Square className="h-4 w-4" /> Stop model
                    </Button>
                ) : (
                    <Button
                        variant="primary"
                        onClick={launch}
                        disabled={!selectedModelId || launching || models.length === 0}
                    >
                        <Play className="h-4 w-4" /> {launching ? "Starting…" : "Launch model"}
                    </Button>
                )}
                {launchStatus && <p className="text-xs text-dim">{launchStatus}</p>}
            </div>

            {/* advisories */}
            {status && status.advisories.length > 0 && (
                <div className="flex flex-col gap-2">
                    <SectionLabel>⚠️ Advisories</SectionLabel>
                    <ul className="flex flex-col gap-1.5">
                        {status.advisories.map((a) => (
                            <li
                                key={a.id}
                                className="rounded-md border border-warn/30 bg-warn/10 px-2.5 py-1.5 text-xs text-dim"
                            >
                                [{a.severity.toUpperCase()}] {a.id}: {a.summary}
                            </li>
                        ))}
                    </ul>
                </div>
            )}

            {/* saved chats */}
            <div className="flex min-h-0 flex-1 flex-col gap-2">
                <SectionLabel>Chats</SectionLabel>
                {savedChats.length === 0 ? (
                    <p className="text-xs text-mute">No saved chats yet.</p>
                ) : (
                    <ul className="flex flex-col gap-0.5 overflow-y-auto">
                        {savedChats.map((c) => (
                            <li
                                key={c.id}
                                className={`group flex items-center gap-1 rounded-lg pr-1 transition-colors ${
                                    c.id === currentChatId ? "bg-accent/15" : "hover:bg-elev"
                                }`}
                            >
                                <button
                                    type="button"
                                    onClick={() => loadChat(c.id)}
                                    className={`min-w-0 flex-1 truncate px-2.5 py-1.5 text-left text-sm ${
                                        c.id === currentChatId ? "text-fg" : "text-dim group-hover:text-fg"
                                    }`}
                                >
                                    {c.title || "Untitled"}
                                </button>
                                <button
                                    type="button"
                                    aria-label="Delete chat"
                                    title="Delete chat"
                                    onClick={() => void onDeleteChat(c.id, c.title)}
                                    className="rounded p-0.5 text-mute opacity-0 transition-opacity hover:text-danger group-hover:opacity-100 focus-visible:opacity-100"
                                >
                                    <Trash2 className="h-3.5 w-3.5" />
                                </button>
                            </li>
                        ))}
                    </ul>
                )}
            </div>

            {/* footer */}
            <div className="flex flex-col gap-1.5 border-t border-line-soft pt-3">
                <div className="font-mono text-[11px] text-mute">
                    {status ? `${gib(status.ram.available_bytes)} / ${gib(status.ram.total_bytes)} GiB RAM` : "—"}
                </div>
                <div className="font-mono text-[11px] text-mute">
                    {status ? `${status.platform.os}/${status.platform.arch}` : "—"}
                </div>
                <div className="font-mono text-[11px] text-mute">v{status?.current?.active ?? "uninitialized"}</div>
                <Button
                    variant="ghost"
                    size="sm"
                    className="justify-start"
                    onClick={onQuit}
                    title="Stop the USBuddy runtime entirely"
                >
                    <Power className="h-3.5 w-3.5" /> Quit USBuddy
                </Button>
                <Button
                    variant="ghost"
                    size="sm"
                    className="justify-start"
                    onClick={onEject}
                    title="Stop the USBuddy runtime and eject the USB drive so it is safe to unplug"
                >
                    <Usb className="h-3.5 w-3.5" /> Quit &amp; eject drive
                </Button>
            </div>
        </aside>
    );
}
