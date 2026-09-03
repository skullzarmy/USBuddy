import { create } from "zustand";
import {
    type ChatMessage,
    type ChatSummary,
    type RuntimeStatus,
    deleteChatApi,
    fetchChat,
    fetchChats,
    fetchPrefs,
    fetchStatus,
    generateChatTitle,
    launchModel,
    newUuid,
    putChat,
    putPrefs,
    shutdown,
    stopModel,
    streamChatCompletion,
    toModelMessages,
} from "./lib/api";
import type { ArchMeta } from "./lib/api";

export interface UiModel {
    id: string;
    label: string;
    profile: string;
    sizeBytes: number;
    arch: ArchMeta | null;
}

export interface UiMessage extends ChatMessage {
    /// Local-only render key; not sent to the model or persisted.
    key: string;
    /// Set on the assistant bubble while its tokens are still arriving.
    streaming?: boolean;
    /// Display-only text shown when content is empty (e.g. "(stopped)").
    /// Never sent to the model or persisted.
    placeholder?: string;
    /// Non-fatal stream error to render under the partial content.
    error?: string;
}

interface AppStore {
    // --- runtime status ---
    status: RuntimeStatus | null;
    statusLine: string;
    models: UiModel[];
    selectedModelId: string | null;
    contextTokens: number;
    llamaRunning: boolean;
    launching: boolean;
    launchStatus: string;
    sidebarOpen: boolean;
    shutdownMessage: string | null;

    // --- chat ---
    messages: UiMessage[];
    streaming: boolean;

    // --- persistence ---
    saveChats: boolean;
    savedChats: ChatSummary[];
    currentChatId: string | null;

    // --- actions ---
    boot: () => Promise<void>;
    selectModel: (id: string) => void;
    setContextTokens: (n: number) => void;
    toggleSidebar: () => void;
    launch: () => Promise<void>;
    stop: () => Promise<void>;
    send: (text: string) => Promise<void>;
    stopGeneration: () => void;
    newChat: () => Promise<void>;
    loadChat: (id: string) => Promise<void>;
    deleteChat: (id: string) => Promise<void>;
    setSaveChats: (save: boolean) => Promise<void>;
    quit: (eject: boolean) => Promise<void>;
}

// Streaming internals kept outside the store: an AbortController and the
// in-flight promise aren't state the UI renders, and mutating them must
// never trigger re-renders.
let inflight: AbortController | null = null;
let activeStream: Promise<void> | null = null;
let currentChatCreatedAt = 0;
let currentChatAiTitled = false;

function buildModels(status: RuntimeStatus): UiModel[] {
    const catalog = status.models.map((m, i) => ({
        id: m.id,
        label: m.display_name,
        profile: m.profile,
        sizeBytes: m.size_bytes,
        arch: status.catalog_arch_meta?.[i] ?? null,
    }));
    const dropIns = status.drop_in_models.map((d) => ({
        id: (d.path.split(/[\\/]/).pop() ?? d.display_name).replace(/\.gguf$/i, ""),
        label: d.display_name,
        profile: d.profile,
        sizeBytes: d.size_bytes || 0,
        arch: d.arch_meta ?? null,
    }));
    return [...catalog, ...dropIns];
}

async function abortActiveStream() {
    if (inflight) {
        inflight.abort();
        try {
            await activeStream;
        } catch {
            /* handled inside the stream runner */
        }
    }
}

export const useAppStore = create<AppStore>((set, get) => {
    async function persistCurrentChat(): Promise<void> {
        const { saveChats, messages, selectedModelId, currentChatId, llamaRunning } = get();
        if (!saveChats) return;
        const history = messages.filter((m) => !m.streaming);
        if (history.length === 0) return;

        const now = Math.floor(Date.now() / 1000);
        let chatId = currentChatId;
        if (!chatId) {
            chatId = newUuid();
            currentChatCreatedAt = now;
            set({ currentChatId: chatId });
        }
        const firstUser = history.find((m) => m.role === "user");
        // Initial title is a naive slice — gives the sidebar something to show
        // immediately while the AI title call (below) runs out of band.
        const fallbackTitle = (firstUser?.content || "Untitled").slice(0, 80).replace(/\s+/g, " ").trim();

        const record = (title: string) => ({
            id: chatId as string,
            title,
            model_id: selectedModelId,
            created_epoch_secs: currentChatCreatedAt,
            updated_epoch_secs: now,
            messages: history.map((m) => ({ role: m.role, content: m.content })),
        });

        const bumpSidebar = (title: string) => {
            const filtered = get().savedChats.filter((c) => c.id !== chatId);
            set({
                savedChats: [
                    { id: chatId as string, title, model_id: selectedModelId, updated_epoch_secs: now },
                    ...filtered,
                ],
            });
        };

        if (await putChat(record(fallbackTitle))) bumpSidebar(fallbackTitle);

        // Upgrade the placeholder title asynchronously with a small model call —
        // once per chat, only when llama is up and there's something to summarize.
        if (!currentChatAiTitled && llamaRunning && firstUser?.content) {
            currentChatAiTitled = true;
            const aiTitle = await generateChatTitle(selectedModelId, firstUser.content);
            if (aiTitle && get().currentChatId === chatId && get().saveChats) {
                if (await putChat(record(aiTitle))) bumpSidebar(aiTitle);
            }
        }
    }

    async function runStream(): Promise<void> {
        const { selectedModelId } = get();
        const key = newUuid();
        set((s) => ({
            streaming: true,
            messages: [...s.messages, { key, role: "assistant", content: "", streaming: true }],
        }));

        const controller = new AbortController();
        inflight = controller;

        const patchBubble = (patch: Partial<UiMessage>) =>
            set((s) => ({
                messages: s.messages.map((m) => (m.key === key ? { ...m, ...patch } : m)),
            }));

        let accum = "";
        try {
            const history = get().messages.filter((m) => !m.streaming);
            accum = await streamChatCompletion(
                selectedModelId,
                toModelMessages(history.map((m) => ({ role: m.role, content: m.content }))),
                controller.signal,
                (acc) => {
                    accum = acc;
                    patchBubble({ content: acc });
                },
            );
            patchBubble({ content: accum, placeholder: "(empty response)", streaming: false });
        } catch (err) {
            const e = err as Error;
            if (e.name === "AbortError") {
                patchBubble({ content: accum, placeholder: "_(stopped)_", streaming: false });
            } else {
                patchBubble({ content: accum, streaming: false, error: e.message || String(e) });
            }
        } finally {
            inflight = null;
            set({ streaming: false });
            // The bubble keeps whatever the assistant produced so the next turn
            // keeps its context — even if the user hit Stop or the stream errored.
            await persistCurrentChat();
        }
    }

    return {
        status: null,
        statusLine: "Loading…",
        models: [],
        selectedModelId: null,
        contextTokens: 4096,
        llamaRunning: false,
        launching: false,
        launchStatus: "",
        sidebarOpen: true,
        shutdownMessage: null,
        messages: [],
        streaming: false,
        saveChats: false,
        savedChats: [],
        currentChatId: null,

        boot: async () => {
            try {
                const [status, prefs, chats] = await Promise.all([fetchStatus(), fetchPrefs(), fetchChats()]);
                const models = buildModels(status);
                const { selectedModelId } = get();
                const selected =
                    selectedModelId && models.some((m) => m.id === selectedModelId)
                        ? selectedModelId
                        : (models[0]?.id ?? null);
                set({
                    status,
                    statusLine: status.message || "Ready",
                    models,
                    selectedModelId: selected,
                    llamaRunning: status.llama_running,
                    saveChats: !!prefs.save_chats,
                    savedChats: chats,
                });
            } catch (err) {
                set({ statusLine: `Runtime API unavailable: ${(err as Error).message}` });
            }
        },

        selectModel: (id) => {
            const { models, contextTokens } = get();
            const model = models.find((m) => m.id === id);
            // Cap context to the model's trained length when we know it.
            const cap = model?.arch?.context_length || 32_768;
            set({ selectedModelId: id, contextTokens: Math.min(contextTokens, cap) });
        },

        setContextTokens: (n) => set({ contextTokens: n }),

        toggleSidebar: () => set((s) => ({ sidebarOpen: !s.sidebarOpen })),

        launch: async () => {
            const { selectedModelId, models, contextTokens } = get();
            if (!selectedModelId) return;
            const model = models.find((m) => m.id === selectedModelId);
            set({ launching: true, launchStatus: "Starting model…" });
            const result = await launchModel({
                model_id: selectedModelId,
                model_size_bytes: model?.sizeBytes || undefined,
                context_tokens: contextTokens,
            }).catch((err: Error) => ({ ok: false, error: err.message }) as const);
            if (result.ok) {
                set({
                    launching: false,
                    launchStatus: `Running (RAM: ${"ram_band" in result ? result.ram_band : "?"})`,
                    llamaRunning: true,
                });
            } else {
                set({ launching: false, launchStatus: `Error: ${result.error}` });
            }
        },

        stop: async () => {
            if (inflight) inflight.abort();
            await stopModel();
            set({ llamaRunning: false, launchStatus: "Model stopped." });
        },

        send: async (text) => {
            const trimmed = text.trim();
            const { llamaRunning } = get();
            if (!trimmed || !llamaRunning) return;

            // If a reply is still streaming, cancel it AND wait for the stream
            // runner to finalize the partial assistant bubble. Without the await,
            // the next user message would land before the partial reply,
            // scrambling conversation order.
            await abortActiveStream();

            set((s) => ({
                messages: [...s.messages, { key: newUuid(), role: "user", content: trimmed }],
            }));
            activeStream = runStream();
            try {
                await activeStream;
            } finally {
                activeStream = null;
            }
        },

        stopGeneration: () => {
            if (inflight) inflight.abort();
        },

        newChat: async () => {
            await abortActiveStream();
            currentChatCreatedAt = 0;
            currentChatAiTitled = false;
            set({ messages: [], currentChatId: null });
        },

        loadChat: async (id) => {
            await abortActiveStream();
            let chat: Awaited<ReturnType<typeof fetchChat>>;
            try {
                chat = await fetchChat(id);
            } catch (err) {
                set({ statusLine: `Failed to load chat: ${(err as Error).message}` });
                return;
            }
            currentChatCreatedAt = chat.created_epoch_secs || Math.floor(Date.now() / 1000);
            currentChatAiTitled = true; // existing chat already has its title
            set({
                messages: (chat.messages || []).map((m) => ({ ...m, key: newUuid() })),
                currentChatId: chat.id,
            });
        },

        deleteChat: async (id) => {
            await deleteChatApi(id);
            const { currentChatId, savedChats } = get();
            set({ savedChats: savedChats.filter((c) => c.id !== id) });
            if (currentChatId === id) {
                currentChatCreatedAt = 0;
                currentChatAiTitled = false;
                set({ messages: [], currentChatId: null });
            }
        },

        setSaveChats: async (save) => {
            set({ saveChats: save });
            await putPrefs({ save_chats: save });
            // If the user just enabled saving mid-conversation, persist what's there.
            if (save && get().messages.length > 0) {
                await persistCurrentChat();
                set({ savedChats: await fetchChats() });
            }
        },

        quit: async (eject) => {
            if (inflight) inflight.abort();
            await shutdown(eject ? "/api/shutdown-eject" : "/api/shutdown");
            set({
                shutdownMessage: eject
                    ? "USBuddy stopped — ejecting the drive. Wait a few seconds for it to disappear, then unplug."
                    : "USBuddy stopped. You can close this tab.",
            });
            // Close the tab once the farewell has been visible for a beat.
            // Browsers only honor window.close() for tabs without navigation
            // history — which is exactly how the launcher opens us.
            setTimeout(() => window.close(), 1500);
        },
    };
});

export function selectedModel(s: { models: UiModel[]; selectedModelId: string | null }): UiModel | null {
    return s.models.find((m) => m.id === s.selectedModelId) ?? null;
}
