// Minimal, dependency-free markdown renderer.
// Supports: headings, paragraphs, **bold**, *italic*, _italic_, `inline code`,
// ```fenced code blocks``` (with language label + copy button), unordered and
// ordered lists, blockquotes, [links](url), horizontal rules, line breaks.
// Designed for chat output — not a full CommonMark implementation, but enough
// to make typical LLM responses look right.

function escapeHtml(s) {
  return s
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;');
}

function escapeAttr(s) {
  return escapeHtml(s).replace(/`/g, '&#96;');
}

function renderInline(text) {
  // Pull inline code spans out first so their contents don't get re-parsed.
  const placeholders = [];
  const stash = (html) => {
    placeholders.push(html);
    return `\u0000${placeholders.length - 1}\u0000`;
  };

  let out = text.replace(/`([^`\n]+)`/g, (_, code) =>
    stash(`<code>${escapeHtml(code)}</code>`)
  );

  out = escapeHtml(out);

  // Restore the placeholders by escaping again would re-escape; so we use
  // a delimiter that survived the escape pass: \u0000N\u0000.
  out = out.replace(/\u0000(\d+)\u0000/g, (_, i) => placeholders[Number(i)]);

  // Links: [text](url)
  out = out.replace(/\[([^\]]+)\]\(([^)\s]+)\)/g, (_, label, href) => {
    const safeHref = /^(https?:|mailto:|#|\/)/i.test(href) ? href : '#';
    return `<a href="${escapeAttr(safeHref)}" target="_blank" rel="noopener noreferrer">${label}</a>`;
  });

  // Bold then italic (longest delimiter first to avoid clashes).
  out = out.replace(/\*\*([^*\n]+)\*\*/g, '<strong>$1</strong>');
  out = out.replace(/__([^_\n]+)__/g, '<strong>$1</strong>');
  out = out.replace(/(?<![\w*])\*([^*\n]+)\*(?!\w)/g, '<em>$1</em>');
  out = out.replace(/(?<![\w_])_([^_\n]+)_(?!\w)/g, '<em>$1</em>');

  // Strikethrough.
  out = out.replace(/~~([^~\n]+)~~/g, '<del>$1</del>');

  return out;
}

function renderCodeBlock(lang, code) {
  const label = (lang || 'text').toLowerCase();
  const encoded = escapeHtml(code);
  // The copy button uses data-code to grab the raw text.
  const raw = encoded.replace(/"/g, '&quot;');
  return (
    `<pre><div class="codeblock-head">` +
    `<span>${escapeHtml(label)}</span>` +
    `<button class="codeblock-copy" type="button" data-code="${raw}">Copy</button>` +
    `</div><code>${encoded}</code></pre>`
  );
}

export function renderMarkdown(src) {
  if (!src) return '';
  // Normalise line endings.
  const lines = src.replace(/\r\n?/g, '\n').split('\n');
  const out = [];
  let i = 0;

  while (i < lines.length) {
    const line = lines[i];

    // Fenced code block: ``` or ```lang
    const fence = line.match(/^```\s*([\w+\-.#]*)\s*$/);
    if (fence) {
      const lang = fence[1];
      const buf = [];
      i++;
      while (i < lines.length && !/^```\s*$/.test(lines[i])) {
        buf.push(lines[i]);
        i++;
      }
      i++; // skip closing fence (if present)
      out.push(renderCodeBlock(lang, buf.join('\n')));
      continue;
    }

    // Horizontal rule
    if (/^\s*(?:-{3,}|\*{3,}|_{3,})\s*$/.test(line)) {
      out.push('<hr>');
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
      const buf = [];
      while (i < lines.length && /^>\s?/.test(lines[i])) {
        buf.push(lines[i].replace(/^>\s?/, ''));
        i++;
      }
      out.push(`<blockquote>${renderMarkdown(buf.join('\n'))}</blockquote>`);
      continue;
    }

    // Unordered list
    if (/^\s*[-*+]\s+/.test(line)) {
      const buf = [];
      while (i < lines.length && /^\s*[-*+]\s+/.test(lines[i])) {
        buf.push(lines[i].replace(/^\s*[-*+]\s+/, ''));
        i++;
      }
      out.push(
        `<ul>${buf.map((item) => `<li>${renderInline(item)}</li>`).join('')}</ul>`
      );
      continue;
    }

    // Ordered list
    if (/^\s*\d+\.\s+/.test(line)) {
      const buf = [];
      while (i < lines.length && /^\s*\d+\.\s+/.test(lines[i])) {
        buf.push(lines[i].replace(/^\s*\d+\.\s+/, ''));
        i++;
      }
      out.push(
        `<ol>${buf.map((item) => `<li>${renderInline(item)}</li>`).join('')}</ol>`
      );
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
    out.push(`<p>${renderInline(buf.join('\n')).replace(/\n/g, '<br>')}</p>`);
  }
  return out.join('');
}

// Hook copy-to-clipboard for any code block buttons inside `root`.
export function wireCopyButtons(root) {
  for (const btn of root.querySelectorAll('.codeblock-copy:not([data-bound])')) {
    btn.dataset.bound = '1';
    btn.addEventListener('click', async () => {
      const code = btn.dataset.code || '';
      const decoded = code
        .replace(/&#96;/g, '`')
        .replace(/&quot;/g, '"')
        .replace(/&#39;/g, "'")
        .replace(/&lt;/g, '<')
        .replace(/&gt;/g, '>')
        .replace(/&amp;/g, '&');
      try {
        await navigator.clipboard.writeText(decoded);
        const orig = btn.textContent;
        btn.textContent = 'Copied';
        setTimeout(() => (btn.textContent = orig), 1200);
      } catch {
        btn.textContent = 'Copy failed';
      }
    });
  }
}
