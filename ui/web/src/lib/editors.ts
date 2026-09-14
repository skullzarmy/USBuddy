// Copy-paste setup for pointing a coding assistant at the USBuddy bridge.
//
// Pure functions over a BridgeConfig so they can be unit-tested without a
// running runtime. See docs/EDITOR-INTEGRATION.md for why the protocol is an
// OpenAI-compatible /v1 endpoint rather than MCP.

export interface BridgeConfig {
    baseUrl: string;
    token: string;
    model: string;
    ctxTokens: number;
}

export interface EditorTarget {
    id: string;
    label: string;
    /// Where the snippet goes, or what the user is clicking through.
    where: string;
    /// Syntax hint for the code block; null when the target is steps-only.
    language: string | null;
    steps: string[];
    snippet: ((cfg: BridgeConfig) => string) | null;
    /// False when the target's config format has nowhere to put the API key —
    /// the editor prompts for it separately. Such targets must say so in
    /// `steps`, or the user pastes the config and wonders why they get a 401.
    tokenInSnippet: boolean;
    /// Set when the target cannot reach localhost at all. Rendered as a
    /// warning instead of instructions.
    blocked?: string;
}

export const EDITOR_TARGETS: EditorTarget[] = [
    {
        id: "continue",
        tokenInSnippet: true,
        label: "Continue",
        where: "~/.continue/config.yaml",
        language: "yaml",
        steps: ["Add USBuddy under `models:`, then pick it in the Continue model dropdown."],
        snippet: (c) =>
            [
                "models:",
                "  - name: USBuddy",
                "    provider: openai",
                `    model: ${c.model}`,
                `    apiBase: ${c.baseUrl}`,
                `    apiKey: ${c.token}`,
                "    roles: [chat, edit, apply]",
                "    defaultCompletionOptions:",
                `      contextLength: ${c.ctxTokens}`,
            ].join("\n"),
    },
    {
        id: "cline",
        tokenInSnippet: false,
        label: "Cline / Roo Code",
        where: "Extension settings → API Provider",
        language: null,
        steps: [
            'Set API Provider to "OpenAI Compatible".',
            "Base URL: paste the endpoint above.",
            "API Key: paste the token above.",
            "Model ID: type the model name exactly as shown.",
            "Works the same inside Cursor, Windsurf, and other VS Code forks.",
        ],
        snippet: null,
    },
    {
        id: "vscode",
        tokenInSnippet: false,
        label: "VS Code (Copilot Chat)",
        where: "Chat model picker → Manage Models",
        language: null,
        steps: [
            "Open the model picker in the Chat view, choose Manage Models.",
            'Pick the "OpenAI Compatible" provider (Add Models → Custom Endpoint).',
            "Enter the endpoint above as the base URL and the token as the API key.",
            "Select the model, then pick USBuddy in the chat model dropdown.",
            "On stable VS Code this provider may not be present yet — Continue or Cline work today.",
        ],
        snippet: null,
    },
    {
        id: "zed",
        tokenInSnippet: false,
        label: "Zed",
        where: "settings.json",
        language: "json",
        steps: [
            "Add the provider, then select USBuddy in the agent panel's model picker.",
            "Zed keeps API keys out of settings.json — paste the token when the agent panel asks for it.",
        ],
        snippet: (c) =>
            JSON.stringify(
                {
                    language_models: {
                        openai_compatible: {
                            USBuddy: {
                                api_url: c.baseUrl,
                                available_models: [
                                    {
                                        name: c.model,
                                        display_name: `USBuddy ${c.model}`,
                                        max_tokens: c.ctxTokens,
                                    },
                                ],
                            },
                        },
                    },
                },
                null,
                2,
            ),
    },
    {
        id: "shell",
        tokenInSnippet: true,
        label: "Shell / Aider",
        where: "Your shell, or any OpenAI SDK",
        language: "bash",
        steps: ["Any tool that reads the standard OpenAI env vars will pick USBuddy up."],
        snippet: (c) =>
            [
                `export OPENAI_BASE_URL=${c.baseUrl}`,
                `export OPENAI_API_BASE=${c.baseUrl}`,
                `export OPENAI_API_KEY=${c.token}`,
                "",
                `aider --model openai/${c.model}`,
            ].join("\n"),
    },
    {
        id: "cursor",
        tokenInSnippet: false,
        label: "Cursor (base URL)",
        where: "Settings → Models → Override OpenAI Base URL",
        language: null,
        steps: [
            "Use Cline, Roo Code, or Continue inside Cursor instead — those run on your machine and reach USBuddy directly.",
        ],
        snippet: null,
        blocked:
            "Cursor's base-URL override does not call your machine — requests route through Cursor's " +
            "servers, which reject localhost and plain HTTP. Making it work needs a public HTTPS tunnel, " +
            "which would send your code off the machine and defeat the point of USBuddy.",
    },
];

/// Renders the snippet for a target, or null when the target is steps-only.
export function renderSnippet(target: EditorTarget, cfg: BridgeConfig): string | null {
    return target.snippet ? target.snippet(cfg) : null;
}

/// Shows enough of a token to recognize it without putting the whole secret
/// on screen (or in a screenshot, or a screen share).
export function maskToken(token: string): string {
    if (!token) return "";
    if (token.length <= 12) return "•".repeat(token.length);
    return `${token.slice(0, 8)}${"•".repeat(16)}${token.slice(-4)}`;
}
