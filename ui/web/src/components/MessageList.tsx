import { useEffect, useRef } from "react";
import { useAppStore } from "../store";
import { Message } from "./Message";

/// Only auto-scroll on new tokens if the user is already near the bottom.
const STICK_THRESHOLD_PX = 40;

export function MessageList() {
    const messages = useAppStore((s) => s.messages);
    const llamaRunning = useAppStore((s) => s.llamaRunning);
    const containerRef = useRef<HTMLDivElement>(null);
    const stickRef = useRef(true);
    const prevCountRef = useRef(0);

    // A fresh user message means the user is committing to this turn — always
    // show its reply, even if they had scrolled away from a previous one.
    if (messages.length > prevCountRef.current) {
        const last = messages[messages.length - 1];
        if (last?.role === "user") stickRef.current = true;
    }
    prevCountRef.current = messages.length;

    // No dep array: re-runs after every render (i.e. each new message or
    // streamed token) and pins the scroll while the user is near the bottom.
    useEffect(() => {
        const el = containerRef.current;
        if (el && stickRef.current) {
            el.scrollTop = el.scrollHeight;
        }
    });

    const onScroll = () => {
        const el = containerRef.current;
        if (!el) return;
        stickRef.current = el.scrollHeight - el.scrollTop - el.clientHeight < STICK_THRESHOLD_PX;
    };

    return (
        <div ref={containerRef} onScroll={onScroll} className="flex-1 overflow-y-auto py-4">
            {messages.length === 0 ? (
                <div className="flex h-full flex-col items-center justify-center gap-4 px-6 text-center">
                    <img src="/assets/icon.png" alt="" className="h-28 w-28 opacity-90 drop-shadow-lg" />
                    <h2 className="text-xl font-semibold text-fg">Hi, I&rsquo;m USBuddy.</h2>
                    <p className="max-w-sm text-sm text-dim">
                        {llamaRunning ? (
                            "Model ready. Type a message below to start chatting."
                        ) : (
                            <>
                                Pick a model in the sidebar and click <strong>Launch model</strong>.
                            </>
                        )}
                    </p>
                </div>
            ) : (
                messages.map((m) => <Message key={m.key} message={m} />)
            )}
        </div>
    );
}
