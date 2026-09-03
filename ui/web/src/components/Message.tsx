import { useEffect, useRef } from "react";
import { decodeDataCode, renderMarkdown } from "../lib/markdown";
import type { UiMessage } from "../store";

/// Renders one chat message. Markdown HTML comes from our own escaping
/// renderer (lib/markdown.ts) — all model output is entity-escaped before
/// insertion, so dangerouslySetInnerHTML is safe here.
export function Message({ message }: { message: UiMessage }) {
    const bodyRef = useRef<HTMLDivElement>(null);
    const isUser = message.role === "user";
    const source = message.content || (message.streaming ? "" : (message.placeholder ?? ""));
    const html = renderMarkdown(source) + (message.streaming ? '<span class="cursor"></span>' : "");

    // Copy-to-clipboard for fenced code blocks via event delegation.
    useEffect(() => {
        const root = bodyRef.current;
        if (!root) return;
        const onClick = async (e: Event) => {
            const btn = (e.target as HTMLElement).closest(".codeblock-copy");
            if (!(btn instanceof HTMLButtonElement)) return;
            try {
                await navigator.clipboard.writeText(decodeDataCode(btn.dataset.code || ""));
                const orig = btn.textContent;
                btn.textContent = "Copied";
                setTimeout(() => {
                    btn.textContent = orig;
                }, 1200);
            } catch {
                btn.textContent = "Copy failed";
            }
        };
        root.addEventListener("click", onClick);
        return () => root.removeEventListener("click", onClick);
    }, []);

    return (
        <div className={`w-full py-3 ${isUser ? "" : "bg-white/2"}`}>
            <div className="mx-auto flex w-full max-w-3xl gap-3 px-4">
                <div className="w-9 shrink-0 pt-0.5 text-right">
                    {isUser ? (
                        <span className="text-xs font-semibold text-mute">You</span>
                    ) : (
                        <img src="/assets/icon.png" alt="USBuddy" className="ml-auto h-7 w-7 rounded-md" />
                    )}
                </div>
                <div className="min-w-0 flex-1">
                    <div
                        ref={bodyRef}
                        className="msg-body wrap-break-word text-[15px] leading-relaxed"
                        // markdown.ts entity-escapes all model output before this point
                        dangerouslySetInnerHTML={{ __html: html }}
                    />
                    {message.error && <em className="mt-2 block text-sm text-danger">Error: {message.error}</em>}
                </div>
            </div>
        </div>
    );
}
