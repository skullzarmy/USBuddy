import { describe, expect, it } from "vitest";
import { cleanTitle, toModelMessages } from "../src/lib/api";
import { archKvBytesPerTokenF16, previewFit } from "../src/lib/ramfit";

describe("toModelMessages", () => {
    it("drops empty messages", () => {
        expect(
            toModelMessages([
                { role: "user", content: "hi" },
                { role: "assistant", content: "" },
                { role: "user", content: "again" },
            ]),
        ).toEqual([{ role: "user", content: "hi\n\nagain" }]);
    });

    it("merges consecutive same-role messages", () => {
        expect(
            toModelMessages([
                { role: "user", content: "a" },
                { role: "user", content: "b" },
                { role: "assistant", content: "c" },
            ]),
        ).toEqual([
            { role: "user", content: "a\n\nb" },
            { role: "assistant", content: "c" },
        ]);
    });

    it("passes through an already-alternating history", () => {
        const history = [
            { role: "user" as const, content: "q" },
            { role: "assistant" as const, content: "a" },
        ];
        expect(toModelMessages(history)).toEqual(history);
    });
});

describe("cleanTitle", () => {
    it("strips think tags, quotes, and trailing punctuation", () => {
        expect(cleanTitle('<think>reasoning</think>"fix nginx config."')).toBe("fix nginx config");
    });

    it("caps at 4 words", () => {
        expect(cleanTitle("one two three four five six")).toBe("one two three four");
    });

    it("returns null for empty results", () => {
        expect(cleanTitle("<think>only thoughts</think>")).toBeNull();
    });
});

describe("ram fit preview", () => {
    const GIB = 1024 ** 3;

    it("computes KV bytes/token from GGUF arch meta (GQA-aware)", () => {
        // llama-3-8B-ish: 32 layers, 32 heads, 8 KV heads, 4096 embed
        const arch = {
            architecture: "llama",
            block_count: 32,
            head_count: 32,
            head_count_kv: 8,
            embedding_length: 4096,
            context_length: 8192,
        };
        // 2 * 32 * 8 * 128 * 2 = 131072
        expect(archKvBytesPerTokenF16(arch)).toBe(131_072);
    });

    it("reports red when the model cannot fit", () => {
        const fit = previewFit(16 * GIB, 4096, 131_072, 8 * GIB);
        expect(fit.band).toBe("red");
    });

    it("reports green with generous headroom", () => {
        const fit = previewFit(4 * GIB, 4096, 131_072, 32 * GIB);
        expect(fit.band).toBe("green");
    });

    it("reports yellow when remaining headroom is under 3 GiB", () => {
        const fit = previewFit(4 * GIB, 4096, 131_072, 7 * GIB);
        expect(fit.band).toBe("yellow");
    });
});
