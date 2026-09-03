import { describe, expect, it } from "vitest";
import { decodeDataCode, renderMarkdown } from "../src/lib/markdown";

describe("renderMarkdown", () => {
    it("returns empty string for empty input", () => {
        expect(renderMarkdown("")).toBe("");
    });

    it("renders paragraphs with inline formatting", () => {
        const html = renderMarkdown("Hello **bold** and *italic* and `code`.");
        expect(html).toContain("<strong>bold</strong>");
        expect(html).toContain("<em>italic</em>");
        expect(html).toContain("<code>code</code>");
    });

    it("escapes raw HTML in text", () => {
        const html = renderMarkdown("<script>alert(1)</script>");
        expect(html).not.toContain("<script>");
        expect(html).toContain("&lt;script&gt;");
    });

    it("escapes HTML inside inline code", () => {
        const html = renderMarkdown("use `<div>` tags");
        expect(html).toContain("<code>&lt;div&gt;</code>");
    });

    it("renders fenced code blocks with language label and copy button", () => {
        const html = renderMarkdown("```rust\nfn main() {}\n```");
        expect(html).toContain("<pre>");
        expect(html).toContain("rust");
        expect(html).toContain("codeblock-copy");
        expect(html).toContain("fn main() {}");
    });

    it("escapes HTML inside code blocks", () => {
        const html = renderMarkdown("```html\n<b>hi</b>\n```");
        expect(html).toContain("&lt;b&gt;hi&lt;/b&gt;");
        expect(html).not.toContain("<b>hi</b>");
    });

    it("renders headings at the right level", () => {
        expect(renderMarkdown("# Title")).toContain("<h1>Title</h1>");
        expect(renderMarkdown("### Sub")).toContain("<h3>Sub</h3>");
    });

    it("renders unordered and ordered lists", () => {
        expect(renderMarkdown("- a\n- b")).toBe("<ul><li>a</li><li>b</li></ul>");
        expect(renderMarkdown("1. a\n2. b")).toBe("<ol><li>a</li><li>b</li></ol>");
    });

    it("renders blockquotes recursively", () => {
        const html = renderMarkdown("> quoted **text**");
        expect(html).toContain("<blockquote>");
        expect(html).toContain("<strong>text</strong>");
    });

    it("allows safe link protocols and neuters unsafe ones", () => {
        const safe = renderMarkdown("[x](https://example.com)");
        expect(safe).toContain('href="https://example.com"');
        // eslint-disable-next-line no-script-url
        const unsafe = renderMarkdown("[x](javascript:alert(1))");
        expect(unsafe).toContain('href="#"');
    });

    it("renders horizontal rules", () => {
        expect(renderMarkdown("---")).toBe("<hr>");
    });

    it("converts single newlines inside a paragraph to <br>", () => {
        expect(renderMarkdown("a\nb")).toBe("<p>a<br>b</p>");
    });
});

describe("decodeDataCode", () => {
    it("round-trips escaped code back to raw text", () => {
        expect(decodeDataCode("&lt;a href=&quot;x&quot;&gt;&amp;&#39;&#96;")).toBe('<a href="x">&\'`');
    });
});
