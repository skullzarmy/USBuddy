import { useEffect } from "react";
import { useAppStore } from "./store";
import { Header } from "./components/Header";
import { Sidebar } from "./components/Sidebar";
import { MessageList } from "./components/MessageList";
import { ChatInput } from "./components/ChatInput";
import { ConfirmDialogHost } from "./components/ui/confirm-dialog";
import { TooltipProvider } from "./components/ui/tooltip";

function ShutdownScreen({ message }: { message: string }) {
    return (
        <div className="flex min-h-screen flex-col items-center justify-center gap-4 bg-bg text-dim">
            <img src="/assets/icon.png" alt="" className="w-28 opacity-60" />
            <div className="max-w-md px-6 text-center leading-relaxed">{message}</div>
        </div>
    );
}

export default function App() {
    const boot = useAppStore((s) => s.boot);
    const sidebarOpen = useAppStore((s) => s.sidebarOpen);
    const shutdownMessage = useAppStore((s) => s.shutdownMessage);

    useEffect(() => {
        void boot();
    }, [boot]);

    if (shutdownMessage) {
        return <ShutdownScreen message={shutdownMessage} />;
    }

    return (
        <TooltipProvider delayDuration={250}>
            <div className="flex h-screen overflow-hidden">
                {sidebarOpen && <Sidebar />}
                <main className="flex min-w-0 flex-1 flex-col">
                    <Header />
                    <MessageList />
                    <ChatInput />
                </main>
            </div>
            <ConfirmDialogHost />
        </TooltipProvider>
    );
}
