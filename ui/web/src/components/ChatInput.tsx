import { type FormEvent, type KeyboardEvent, useRef, useState } from "react";
import { SendHorizonal, Square } from "lucide-react";
import { useAppStore } from "../store";
import { Button } from "./ui/button";

export function ChatInput() {
    const llamaRunning = useAppStore((s) => s.llamaRunning);
    const streaming = useAppStore((s) => s.streaming);
    const statusLine = useAppStore((s) => s.statusLine);
    const send = useAppStore((s) => s.send);
    const stopGeneration = useAppStore((s) => s.stopGeneration);

    const [text, setText] = useState("");
    const textareaRef = useRef<HTMLTextAreaElement>(null);

    const autoResize = () => {
        const el = textareaRef.current;
        if (!el) return;
        el.style.height = "auto";
        el.style.height = Math.min(el.scrollHeight, 240) + "px";
    };

    const submit = (e?: FormEvent) => {
        e?.preventDefault();
        const value = text.trim();
        if (!value || !llamaRunning) return;
        setText("");
        requestAnimationFrame(autoResize);
        void send(value);
    };

    const onKeyDown = (e: KeyboardEvent<HTMLTextAreaElement>) => {
        if (e.key === "Enter" && !e.shiftKey) {
            e.preventDefault();
            submit();
        }
    };

    return (
        <form onSubmit={submit} className="mx-auto w-full max-w-3xl px-4 pb-3" autoComplete="off">
            <div className="flex items-end gap-2 rounded-xl border border-line bg-bg-2 p-2 shadow-md focus-within:border-accent/50">
                <textarea
                    ref={textareaRef}
                    value={text}
                    onChange={(e) => {
                        setText(e.target.value);
                        autoResize();
                    }}
                    onKeyDown={onKeyDown}
                    disabled={!llamaRunning}
                    rows={1}
                    placeholder="Message USBuddy… (Enter to send, Shift+Enter for newline)"
                    className="max-h-60 min-h-9 flex-1 resize-none bg-transparent px-2 py-1.5 text-[15px] text-fg outline-none placeholder:text-mute disabled:cursor-not-allowed"
                />
                {streaming ? (
                    <Button variant="secondary" onClick={stopGeneration} title="Stop generating">
                        <Square className="h-3.5 w-3.5" /> Stop
                    </Button>
                ) : (
                    <Button type="submit" variant="primary" disabled={!llamaRunning || !text.trim()}>
                        <SendHorizonal className="h-4 w-4" /> Send
                    </Button>
                )}
            </div>
            <div className="px-2 pt-2 text-xs text-mute">{statusLine}</div>
        </form>
    );
}
