// Typed client for the USBuddy runtime API (axum server on localhost).

export interface ArchMeta {
    architecture: string;
    block_count: number;
    head_count: number;
    head_count_kv: number;
    embedding_length: number;
    context_length: number;
}

export interface ModelEntry {
    id: string;
    display_name: string;
    profile: string;
    size_bytes: number;
    file_name: string;
    aliases: string[];
}

export interface DropInModel {
    path: string;
    display_name: string;
    profile: string;
    size_bytes: number;
    arch_meta?: ArchMeta | null;
}

export interface Advisory {
    id: string;
    severity: string;
    summary: string;
}

export interface RuntimeStatus {
    message: string;
    version: string;
    platform: { os: string; arch: string };
    current: { active: string } | null;
    models: ModelEntry[];
    drop_in_models: DropInModel[];
    advisories: Advisory[];
    ram: { total_bytes: number; available_bytes: number };
    ram_previews: unknown[];
    catalog_arch_meta: (ArchMeta | null)[];
    llama_running: boolean;
    llama_port: number;
    idle_timeout_secs: number;
    last_activity_epoch_secs: number;
}

export interface RuntimePrefs {
    save_chats: boolean;
}

export interface ChatMessage {
    role: "user" | "assistant" | "system";
    content: string;
}

export interface ChatSummary {
    id: string;
    title: string;
    model_id: string | null;
    updated_epoch_secs: number;
}

export interface ChatRecord {
    id: string;
    title: string;
    model_id: string | null;
    created_epoch_secs: number;
    updated_epoch_secs: number;
    messages: ChatMessage[];
}

export async function fetchStatus(): Promise<RuntimeStatus> {
    const r = await fetch("/api/status");
    if (!r.ok) throw new Error(`HTTP ${r.status}`);
    return r.json();
}

export async function fetchPrefs(): Promise<RuntimePrefs> {
    try {
        const r = await fetch("/api/prefs");
        if (!r.ok) return { save_chats: false };
        return await r.json();
    } catch {
        return { save_chats: false };
    }
}

export async function putPrefs(prefs: RuntimePrefs): Promise<void> {
    try {
        await fetch("/api/prefs", {
            method: "PUT",
            headers: { "content-type": "application/json" },
            body: JSON.stringify(prefs),
        });
    } catch {
        /* best-effort; UI state remains the source of truth this session */
    }
}

export async function fetchChats(): Promise<ChatSummary[]> {
    try {
        const r = await fetch("/api/chats");
        if (!r.ok) return [];
        return await r.json();
    } catch {
        return [];
    }
}

export async function fetchChat(id: string): Promise<ChatRecord> {
    const r = await fetch(`/api/chats/${id}`);
    if (!r.ok) throw new Error(`HTTP ${r.status}`);
    return r.json();
}

export async function putChat(chat: ChatRecord): Promise<boolean> {
    try {
        const r = await fetch(`/api/chats/${chat.id}`, {
            method: "PUT",
            headers: { "content-type": "application/json" },
            body: JSON.stringify(chat),
        });
        return r.ok;
    } catch {
        return false; /* drive may be gone — next refresh will reconcile */
    }
}

export async function deleteChatApi(id: string): Promise<void> {
    try {
        await fetch(`/api/chats/${id}`, { method: "DELETE" });
    } catch {
        /* swallow; refresh will reconcile */
    }
}

export interface LaunchRequest {
    model_id: string;
    model_size_bytes?: number;
    context_tokens: number;
}

export interface LaunchResult {
    ok: boolean;
    ram_band?: string;
    error?: string;
}

export async function launchModel(req: LaunchRequest): Promise<LaunchResult> {
    const r = await fetch("/api/launch", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify(req),
    });
    const data = await r.json().catch(() => ({}));
    if (!r.ok) return { ok: false, error: formatApiError(data, r.statusText) };
    return { ok: true, ram_band: data.ram_band };
}

export async function stopModel(): Promise<void> {
    await fetch("/api/stop", { method: "POST" });
}

export async function shutdown(endpoint: "/api/shutdown" | "/api/shutdown-eject"): Promise<void> {
    try {
        await fetch(endpoint, { method: "POST" });
    } catch {
        /* expected — connection will drop */
    }
}

export function formatApiError(payload: unknown, fallback: string): string {
    if (!payload || typeof payload !== "object") return fallback || "unknown error";
    const p = payload as Record<string, unknown>;
    const e = p.error;
    if (typeof e === "string") return e;
    if (e && typeof e === "object") {
        const eo = e as Record<string, unknown>;
        return (eo.message as string) || (eo.type as string) || (eo.code as string) || JSON.stringify(e);
    }
    if (typeof p.message === "string") return p.message;
    return fallback || "unknown error";
}

/// Streams a chat completion. Calls `onDelta` with the accumulated text as
/// tokens arrive. Resolves with the final text when the stream ends.
/// Throws on transport / mid-stream errors (AbortError included).
export async function streamChatCompletion(
    model: string | null,
    messages: ChatMessage[],
    signal: AbortSignal,
    onDelta: (accumulated: string) => void,
): Promise<string> {
    const r = await fetch("/api/chat/completions", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ model, messages, stream: true }),
        signal,
    });
    if (!r.ok || !r.body) {
        const errPayload = await r.json().catch(() => ({}));
        throw new Error(formatApiError(errPayload, r.statusText));
    }
    const reader = r.body.getReader();
    const decoder = new TextDecoder();
    let buf = "";
    let accum = "";
    for (;;) {
        const { done, value } = await reader.read();
        if (done) break;
        buf += decoder.decode(value, { stream: true });
        for (let idx = buf.indexOf("\n"); idx !== -1; idx = buf.indexOf("\n")) {
            const line = buf.slice(0, idx).trim();
            buf = buf.slice(idx + 1);
            if (!line.startsWith("data:")) continue;
            const data = line.slice(5).trim();
            if (data === "[DONE]") continue;
            let obj: unknown;
            try {
                obj = JSON.parse(data);
            } catch {
                continue; // non-JSON SSE comment / keepalive
            }
            const o = obj as { error?: unknown; choices?: { delta?: { content?: string } }[] };
            if (o.error) {
                // Mid-stream error from llama-server (e.g. context overflow).
                throw new Error(formatApiError(o, "stream error"));
            }
            const delta = o.choices?.[0]?.delta?.content;
            if (delta) {
                accum += delta;
                onDelta(accum);
            }
        }
    }
    return accum;
}

/// Asks the active llama-server for a 2-3 word title summarizing the user's
/// opening message. Returns null on any failure. Times out after 20s so a
/// wedged engine never blocks the save loop indefinitely.
export async function generateChatTitle(model: string | null, firstUserMessage: string): Promise<string | null> {
    const controller = new AbortController();
    const timeoutId = setTimeout(() => controller.abort(), 20_000);
    try {
        const r = await fetch("/api/chat/completions", {
            method: "POST",
            headers: { "content-type": "application/json" },
            body: JSON.stringify({
                model,
                stream: false,
                temperature: 0.3,
                max_tokens: 16,
                messages: [
                    {
                        role: "system",
                        content:
                            "You write a concise 2-3 word title summarizing the user message. " +
                            "Reply with ONLY the title — no quotes, no punctuation, no preamble, " +
                            'no explanation. Examples: "fix nginx config", "vacation ideas", ' +
                            '"rust async basics".',
                    },
                    { role: "user", content: firstUserMessage },
                ],
            }),
            signal: controller.signal,
        });
        if (!r.ok) return null;
        const data = await r.json();
        const raw = data.choices?.[0]?.message?.content;
        if (!raw) return null;
        return cleanTitle(raw);
    } catch {
        return null;
    } finally {
        clearTimeout(timeoutId);
    }
}

export function cleanTitle(raw: string): string | null {
    const cleaned = String(raw)
        .replace(/<think>[\s\S]*?<\/think>/gi, "") // strip CoT tags if model adds them
        .replace(/^["'`*_]+|["'`*_]+$/g, "") // strip wrapping quotes / markdown
        .replace(/[.!?,;:]+$/g, "") // strip trailing punctuation
        .replace(/\s+/g, " ")
        .trim();
    if (!cleaned) return null;
    // Cap at 4 words; smaller models sometimes ramble past the instruction.
    return cleaned.split(/\s+/).slice(0, 4).join(" ");
}

// Chat templates (Gemma, Llama, …) hard-reject histories whose roles don't
// strictly alternate user/assistant. Sanitize at send time: drop empty
// messages, then merge consecutive same-role messages. The on-screen
// transcript and the saved chat keep the real shape — only the model
// payload is normalized.
export function toModelMessages(history: ChatMessage[]): ChatMessage[] {
    const out: ChatMessage[] = [];
    for (const m of history) {
        if (!m.content) continue;
        const prev = out[out.length - 1];
        if (prev && prev.role === m.role) {
            prev.content += "\n\n" + m.content;
        } else {
            out.push({ role: m.role, content: m.content });
        }
    }
    return out;
}

export function fallbackUuid(): string {
    const r = crypto.getRandomValues(new Uint8Array(16));
    r[6] = (r[6] & 0x0f) | 0x40;
    r[8] = (r[8] & 0x3f) | 0x80;
    const h = [...r].map((b) => b.toString(16).padStart(2, "0"));
    return `${h.slice(0, 4).join("")}-${h.slice(4, 6).join("")}-${h.slice(6, 8).join("")}-${h.slice(8, 10).join("")}-${h.slice(10, 16).join("")}`;
}

export function newUuid(): string {
    return crypto.randomUUID?.() ?? fallbackUuid();
}
