import { describe, expect, it } from "vitest";
import { EDITOR_TARGETS, maskToken, renderSnippet, type BridgeConfig } from "../src/lib/editors";

const cfg: BridgeConfig = {
    baseUrl: "http://127.0.0.1:8765/v1",
    token: "usb-0123456789abcdef0123456789abcdef",
    model: "qwen2.5-7b-instruct-q4_k_m",
    ctxTokens: 16384,
};

describe("editor targets", () => {
    it("gives every target a stable id", () => {
        const ids = EDITOR_TARGETS.map((t) => t.id);
        expect(new Set(ids).size).toBe(ids.length);
    });

    it("carries the endpoint and model into every snippet", () => {
        const withSnippets = EDITOR_TARGETS.filter((t) => t.snippet);
        expect(withSnippets.length).toBeGreaterThan(0);
        for (const target of withSnippets) {
            const snippet = renderSnippet(target, cfg);
            expect(snippet, target.id).toContain(cfg.baseUrl);
            expect(snippet, target.id).toContain(cfg.model);
        }
    });

    it("either puts the token in the snippet or tells the user where it goes", () => {
        for (const target of EDITOR_TARGETS) {
            const snippet = renderSnippet(target, cfg);
            if (target.tokenInSnippet) {
                expect(snippet, target.id).toContain(cfg.token);
            } else if (!target.blocked) {
                // No slot for the key in this format — the steps must say so,
                // otherwise the user pastes the config and gets a silent 401.
                const mentionsKey = target.steps.some((s) => /token|api key/i.test(s));
                expect(mentionsKey, `${target.id} steps must explain where the key goes`).toBe(true);
            }
        }
    });

    it("emits valid JSON for the JSON targets", () => {
        for (const target of EDITOR_TARGETS.filter((t) => t.language === "json" && t.snippet)) {
            expect(() => JSON.parse(renderSnippet(target, cfg) as string), target.id).not.toThrow();
        }
    });

    it("returns null rather than an empty block for steps-only targets", () => {
        const stepsOnly = EDITOR_TARGETS.find((t) => t.id === "cline");
        expect(stepsOnly).toBeDefined();
        expect(renderSnippet(stepsOnly!, cfg)).toBeNull();
    });

    it("marks Cursor's base-URL override as blocked and offers no snippet", () => {
        const cursor = EDITOR_TARGETS.find((t) => t.id === "cursor");
        expect(cursor?.blocked).toBeTruthy();
        expect(cursor?.snippet).toBeNull();
    });
});

describe("maskToken", () => {
    it("keeps a recognizable head and tail", () => {
        const masked = maskToken(cfg.token);
        expect(masked.startsWith("usb-0123")).toBe(true);
        expect(masked.endsWith(cfg.token.slice(-4))).toBe(true);
        expect(masked).not.toContain(cfg.token.slice(8, -4));
    });

    it("reveals nothing from short or empty tokens", () => {
        expect(maskToken("short")).toBe("•••••");
        expect(maskToken("")).toBe("");
    });
});
