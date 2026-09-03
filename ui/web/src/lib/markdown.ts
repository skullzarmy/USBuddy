// Minimal, dependency-free markdown renderer (ported from the original
// vanilla-JS UI). Supports: headings, paragraphs, **bold**, *italic*,
// `inline code`, ```fenced code blocks``` (with language label + copy
// button), unordered and ordered lists, blockquotes, [links](url),
// horizontal rules, line breaks. Designed for chat output — not full
// CommonMark, but enough to make typical LLM responses look right.
// All interpolated content is HTML-escaped before insertion.

function escapeHtml(s: string): string {
    return s
        .replace(/&/g, "&amp;")
        .replace(/</g, "&lt;")
        .replace(/>/g, "&gt;")
        .replace(/"/g, "&quot;")
        .replace(/'/g, "&#39;");
}

function escapeAttr(s: string): string {
    return escapeHtml(s).replace(/`/g, "&#96;");
}

function renderInline(text: string): string {
    // Pull inline code spans out first so their contents don't get re-parsed.
    // Sentinel uses a private-use-area char that survives the escape pass.
    const placeholders: string[] = [];
    const stash = (html: string) => {
        placeholders.push(html);
        return `\uE000${placeholders.length - 1}\uE000`;
    };

    let out = text.replace(/`([^`\n]+)`/g, (_, code) => stash(`<code>${escapeHtml(code)}</code>`));

    out = escapeHtml(out);

    out = out.replace(/\uE000(\d+)\uE000/g, (_, i) => placeholders[Number(i)]);

    // Links: [text](url) — only safe protocols; anything else is neutered.
    out = out.replace(/\[([^\]]+)\]\(([^)\s]+)\)/g, (_, label, href) => {
        const safeHref = /^(https?:|mailto:|#|\/)/i.test(href) ? href : "#";
        return `<a href="${escapeAttr(safeHref)}" target="_blank" rel="noopener noreferrer">${label}</a>`;
    });

    // Bold then italic (longest delimiter first to avoid clashes).
    out = out.replace(/\*\*([^*\n]+)\*\*/g, "<strong>$1</strong>");
    out = out.replace(/__([^_\n]+)__/g, "<strong>$1</strong>");
    out = out.replace(/(?<![\w*])\*([^*\n]+)\*(?!\w)/g, "<em>$1</em>");
    out = out.replace(/(?<![\w_])_([^_\n]+)_(?!\w)/g, "<em>$1</em>");

    // Strikethrough.
    out = out.replace(/~~([^~\n]+)~~/g, "<del>$1</del>");

    return out;
}

function renderCodeBlock(lang: string, code: string): string {
    const label = (lang || "text").toLowerCase();
    const encoded = escapeHtml(code);
    // The copy button uses data-code to grab the raw text.
    const raw = encoded.replace(/"/g, "&quot;");
    return (
        `<pre><div class="codeblock-head">` +
        `<span>${escapeHtml(label)}</span>` +
        `<button class="codeblock-copy" type="button" data-code="${raw}">Copy</button>` +
        `</div><code>${encoded}</code></pre>`
    );
}

export function renderMarkdown(src: string): string {
    if (!src) return "";
    const lines = src.replace(/\r\n?/g, "\n").split("\n");
    const out: string[] = [];
    let i = 0;

    while (i < lines.length) {
        const line = lines[i];

        // Fenced code block: ``` or ```lang
        const fence = line.match(/^```\s*([\w+\-.#]*)\s*$/);
        if (fence) {
            const lang = fence[1];
            const buf: string[] = [];
            i++;
            while (i < lines.length && !/^```\s*$/.test(lines[i])) {
                buf.push(lines[i]);
                i++;
            }
            i++; // skip closing fence (if present)
            out.push(renderCodeBlock(lang, buf.join("\n")));
            continue;
        }

        // Horizontal rule
        if (/^\s*(?:-{3,}|\*{3,}|_{3,})\s*$/.test(line)) {
            out.push("<hr>");
            i++;
            continue;
        }

        // Heading
        const h = line.match(/^(#{1,6})\s+(.*)$/);
        if (h) {
            const level = h[1].length;
            out.push(`<h${level}>${renderInline(h[2])}</h${level}>`);
            i++;
            continue;
        }

        // Blockquote (consume contiguous > lines)
        if (/^>\s?/.test(line)) {
            const buf: string[] = [];
            while (i < lines.length && /^>\s?/.test(lines[i])) {
                buf.push(lines[i].replace(/^>\s?/, ""));
                i++;
            }
            out.push(`<blockquote>${renderMarkdown(buf.join("\n"))}</blockquote>`);
            continue;
        }

        // Unordered list
        if (/^\s*[-*+]\s+/.test(line)) {
            const buf: string[] = [];
            while (i < lines.length && /^\s*[-*+]\s+/.test(lines[i])) {
                buf.push(lines[i].replace(/^\s*[-*+]\s+/, ""));
                i++;
            }
            out.push(`<ul>${buf.map((item) => `<li>${renderInline(item)}</li>`).join("")}</ul>`);
            continue;
        }

        // Ordered list
        if (/^\s*\d+\.\s+/.test(line)) {
            const buf: string[] = [];
            while (i < lines.length && /^\s*\d+\.\s+/.test(lines[i])) {
                buf.push(lines[i].replace(/^\s*\d+\.\s+/, ""));
                i++;
            }
            out.push(`<ol>${buf.map((item) => `<li>${renderInline(item)}</li>`).join("")}</ol>`);
            continue;
        }

        // Blank line → paragraph break, skip.
        if (/^\s*$/.test(line)) {
            i++;
            continue;
        }

        // Paragraph: collect until blank line / block element.
        const buf = [line];
        i++;
        while (
            i < lines.length &&
            !/^\s*$/.test(lines[i]) &&
            !/^```/.test(lines[i]) &&
            !/^#{1,6}\s+/.test(lines[i]) &&
            !/^>\s?/.test(lines[i]) &&
            !/^\s*[-*+]\s+/.test(lines[i]) &&
            !/^\s*\d+\.\s+/.test(lines[i]) &&
            !/^\s*(?:-{3,}|\*{3,}|_{3,})\s*$/.test(lines[i])
        ) {
            buf.push(lines[i]);
            i++;
        }
        out.push(`<p>${renderInline(buf.join("\n")).replace(/\n/g, "<br>")}</p>`);
    }
    return out.join("");
}

/// Decodes the entity-encoded data-code attribute back to raw source for
/// the clipboard. Inverse of the escaping in renderCodeBlock.
export function decodeDataCode(code: string): string {
    return code
        .replace(/&#96;/g, "`")
        .replace(/&quot;/g, '"')
        .replace(/&#39;/g, "'")
        .replace(/&lt;/g, "<")
        .replace(/&gt;/g, ">")
        .replace(/&amp;/g, "&");
}
