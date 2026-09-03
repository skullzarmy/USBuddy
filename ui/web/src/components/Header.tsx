import { Glasses, PanelLeftClose, PanelLeftOpen, Plus, Save } from "lucide-react";
import { useAppStore, selectedModel } from "../store";
import { Button } from "./ui/button";
import { Switch } from "./ui/switch";
import { Tooltip, TooltipContent, TooltipTrigger } from "./ui/tooltip";
import { confirmDialog } from "./ui/confirm-dialog";

export function Header() {
    const sidebarOpen = useAppStore((s) => s.sidebarOpen);
    const toggleSidebar = useAppStore((s) => s.toggleSidebar);
    const llamaRunning = useAppStore((s) => s.llamaRunning);
    const saveChats = useAppStore((s) => s.saveChats);
    const setSaveChats = useAppStore((s) => s.setSaveChats);
    const newChat = useAppStore((s) => s.newChat);
    const model = useAppStore(selectedModel);

    const onToggleSave = async (next: boolean) => {
        if (next) {
            const ok = await confirmDialog({
                title: "Save conversations to the drive?",
                description: (
                    <>
                        Saved chats live under <code className="font-mono">.usbuddy/chats/</code> on this USB stick in
                        plaintext. Anyone who plugs the stick into a computer can read them. Keep incognito on if you
                        are not sure.
                    </>
                ),
                confirmLabel: "Enable memory",
            });
            if (!ok) return;
        }
        await setSaveChats(next);
    };

    return (
        <header className="flex h-13 shrink-0 items-center gap-3 border-b border-line-soft px-3">
            <Button
                variant="ghost"
                size="icon"
                onClick={toggleSidebar}
                title={sidebarOpen ? "Hide sidebar" : "Show sidebar"}
                aria-label={sidebarOpen ? "Hide sidebar" : "Show sidebar"}
                aria-expanded={sidebarOpen}
            >
                {sidebarOpen ? <PanelLeftClose className="h-5 w-5" /> : <PanelLeftOpen className="h-5 w-5" />}
            </Button>

            <div className="min-w-0 flex-1 truncate text-sm font-semibold text-fg">
                {llamaRunning && model ? model.label : "USBuddy"}
            </div>

            <Tooltip>
                <TooltipTrigger asChild>
                    <label
                        htmlFor="memory-switch"
                        className="flex cursor-pointer items-center gap-2 rounded-lg px-2 py-1 text-xs text-dim hover:bg-elev"
                    >
                        {saveChats ? <Save className="h-4 w-4 text-accent" /> : <Glasses className="h-4 w-4" />}
                        <span>{saveChats ? "Memory on" : "Incognito"}</span>
                        <Switch id="memory-switch" checked={saveChats} onCheckedChange={(v) => void onToggleSave(v)} />
                    </label>
                </TooltipTrigger>
                <TooltipContent side="bottom">
                    {saveChats
                        ? "Chats are saved to the drive under .usbuddy/chats/. Toggle off to go incognito — the current chat will live only in RAM."
                        : "Incognito: nothing is written to the drive. Toggle on to save chats under .usbuddy/chats/."}
                </TooltipContent>
            </Tooltip>

            <Button variant="outline" size="sm" onClick={newChat} title="Start a fresh conversation">
                <Plus className="h-3.5 w-3.5" /> New chat
            </Button>
        </header>
    );
}
