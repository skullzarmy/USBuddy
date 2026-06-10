import { renderMarkdown, wireCopyButtons } from '/assets/markdown.js';

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

let state = null;
let selectedModelId = null;
let llamaRunning = false;
let inflight = null; // AbortController for the active /api/chat request
let activeStream = null; // Promise of the in-flight streamReply (or null)
const chatHistory = [];

// Chat persistence + incognito state. Default is incognito (no writes to the
// drive); the user opts in via the "Enable memory" toggle. Overwritten by
// /api/prefs at boot.
let saveChats = false;
let currentChatId = null;     // UUID of the chat being persisted, if any
let currentChatCreatedAt = 0; // epoch seconds; preserved across saves
let currentChatAiTitled = false; // true once we've upgraded the slice-title to an AI summary (or loaded an existing chat)
let savedChats = [];          // summaries, newest first

// Scroll-pinning: only auto-scroll on new tokens if the user is already near
// the bottom. Reset to true on send so a fresh prompt always shows its reply.
let stickToBottom = true;
const STICK_THRESHOLD_PX = 40;

// ---------------------------------------------------------------------------
// DOM
// ---------------------------------------------------------------------------

const $ = (id) => document.getElementById(id);
const app = document.querySelector('.app');

const sidebarToggleBtn = $('sidebar-toggle-btn');
const modelSelect = $('model-select');
const noModels = $('no-models');
const contextSlider = $('context-slider');
const contextValue = $('context-value');
const ramBadge = $('ram-badge');
const launchBtn = $('launch-btn');
const stopBtn = $('stop-btn');
const launchStatus = $('launch-status');
const advisoriesSection = $('advisories-section');
const advisoriesList = $('advisories');
const ramInfoEl = $('ram-info');
const platformEl = $('platform');
const versionEl = $('version');
const chatTitle = $('chat-title');
const chatMessages = $('chat-messages');
const emptyState = $('empty-state');
const emptyHint = $('empty-hint');
const chatForm = $('chat-form');
const chatInput = $('chat-input');
const sendBtn = $('send-btn');
const stopGenBtn = $('stop-gen-btn');
const statusLine = $('status-line');
const newChatBtn = $('new-chat');
const quitBtn = $('quit-btn');
const incognitoToggle = $('incognito-toggle');
const chatsList = $('chats-list');
const chatsEmpty = $('chats-empty');

// ---------------------------------------------------------------------------
// Boot
// ---------------------------------------------------------------------------

async function boot() {
  try {
    const [payload, prefs, chats] = await Promise.all([
      fetchStatus(),
      fetchPrefs(),
      fetchChats(),
    ]);
    applyStatus(payload);
    syncLlamaState(payload.llama_running);
    saveChats = !!prefs.save_chats;
    syncIncognitoUi();
    savedChats = chats;
    renderChatsList();
  } catch (err) {
    statusLine.textContent = `Runtime API unavailable: ${err.message}`;
  }
}

async function fetchPrefs() {
  try {
    const r = await fetch('/api/prefs');
    if (!r.ok) return { save_chats: false };
    return await r.json();
  } catch {
    return { save_chats: false };
  }
}

async function fetchChats() {
  try {
    const r = await fetch('/api/chats');
    if (!r.ok) return [];
    return await r.json();
  } catch {
    return [];
  }
}

async function fetchStatus() {
  const r = await fetch('/api/status');
  if (!r.ok) throw new Error(`HTTP ${r.status}`);
  return r.json();
}

// ---------------------------------------------------------------------------
// Sidebar
// ---------------------------------------------------------------------------

function applyStatus(payload) {
  state = payload;
  statusLine.textContent = payload.message || 'Ready';

  versionEl.textContent = payload.current?.active ?? 'uninitialized';
  platformEl.textContent = `${payload.platform.os}/${payload.platform.arch}`;
  const availGib = (payload.ram.available_bytes / 1024 ** 3).toFixed(1);
  const totalGib = (payload.ram.total_bytes / 1024 ** 3).toFixed(1);
  ramInfoEl.textContent = `${availGib} / ${totalGib} GiB RAM`;

  renderAdvisories(payload.advisories || []);
  renderModelOptions(
    payload.models,
    payload.drop_in_models,
    payload.ram_previews,
    payload.catalog_arch_meta || [],
  );
}

function renderAdvisories(advisories) {
  if (!advisories.length) {
    advisoriesSection.hidden = true;
    return;
  }
  advisoriesSection.hidden = false;
  advisoriesList.innerHTML = '';
  for (const a of advisories) {
    const li = document.createElement('li');
    li.textContent = `[${a.severity.toUpperCase()}] ${a.id}: ${a.summary}`;
    advisoriesList.appendChild(li);
  }
}

function renderModelOptions(catalogModels, dropIns, ramPreviews, catalogArchMeta) {
  const items = [
    ...catalogModels.map((m, i) => ({
      id: m.id,
      label: m.display_name,
      profile: m.profile,
      sizeBytes: m.size_bytes,
      arch: catalogArchMeta?.[i] ?? null,
      ram: ramPreviews?.[i] ?? null,
    })),
    ...dropIns.map((d) => ({
      id: (d.path.split(/[\\/]/).pop() ?? d.display_name).replace(/\.gguf$/i, ''),
      label: d.display_name,
      profile: d.profile,
      sizeBytes: d.size_bytes || 0,
      arch: d.arch_meta || null,
      ram: null,
    })),
  ];

  modelSelect.innerHTML = '';
  if (!items.length) {
    noModels.hidden = false;
    modelSelect.disabled = true;
    launchBtn.disabled = true;
    return;
  }
  noModels.hidden = true;
  modelSelect.disabled = false;
  launchBtn.disabled = false;

  for (const m of items) {
    const opt = document.createElement('option');
    opt.value = m.id;
    const sz = m.sizeBytes ? ` · ${(m.sizeBytes / 1024 ** 3).toFixed(1)} GiB` : '';
    opt.textContent = `${m.label} (${m.profile}${sz})`;
    opt.dataset.sizeBytes = m.sizeBytes || 0;
    if (m.arch) {
      opt.dataset.archJson = JSON.stringify(m.arch);
    }
    modelSelect.appendChild(opt);
  }

  if (!selectedModelId || !items.find((m) => m.id === selectedModelId)) {
    selectedModelId = items[0].id;
  }
  modelSelect.value = selectedModelId;
  syncContextSliderToModel();
  updateRamBadge();
}

/// Conservative KV bytes/token when we couldn't parse the GGUF header.
/// Assumes non-GQA (full KV heads). It's better for the advisor to err on
/// the side of "tight" than to silently green-light a model that will OOM.
const FALLBACK_KV_BYTES_PER_TOKEN = 524_288;

function selectedArch() {
  const opt = modelSelect.selectedOptions[0];
  if (!opt || !opt.dataset.archJson) return null;
  try {
    return JSON.parse(opt.dataset.archJson);
  } catch {
    return null;
  }
}

/// Caps the slider to the model's trained context length when we know it.
/// Drops the value if the user had it set higher than the new max. Falls
/// back to 32K when arch is unknown so the user can still push it.
function syncContextSliderToModel() {
  const arch = selectedArch();
  const cap = arch?.context_length || 32_768;
  // Snap the slider step to 512 but ensure the cap itself is reachable.
  const step = 512;
  const snappedCap = Math.max(step, Math.floor(cap / step) * step);
  contextSlider.max = String(snappedCap);
  if (Number(contextSlider.value) > snappedCap) {
    contextSlider.value = String(snappedCap);
  }
  contextValue.textContent = Number(contextSlider.value).toLocaleString();
}

modelSelect.addEventListener('change', () => {
  selectedModelId = modelSelect.value;
  syncContextSliderToModel();
  updateRamBadge();
});

contextSlider.addEventListener('input', () => {
  contextValue.textContent = Number(contextSlider.value).toLocaleString();
  updateRamBadge();
});

function updateRamBadge() {
  if (!state) {
    ramBadge.textContent = '';
    ramBadge.className = 'ram-badge';
    return;
  }
  const opt = modelSelect.selectedOptions[0];
  const sizeBytes = opt ? Number(opt.dataset.sizeBytes || 0) : 0;
  if (!sizeBytes) {
    ramBadge.textContent = 'unknown size';
    ramBadge.className = 'ram-badge';
    return;
  }
  const arch = selectedArch();
  const kvPerToken = arch ? archKvBytesPerTokenF16(arch) : FALLBACK_KV_BYTES_PER_TOKEN;
  const ctx = Number(contextSlider.value);
  const kv = ctx * kvPerToken;
  const overhead = 512 * 1024 * 1024;
  const required = sizeBytes + kv + overhead;
  const avail = state.ram.available_bytes;
  const remaining = avail - required;
  const margin = remaining / required;

  let band, label;
  if (remaining < 0 || remaining < 1_073_741_824) {
    band = 'red';
    label = '🔴 Won\u2019t fit';
  } else if (margin < 0.2 || remaining < 3_221_225_472) {
    band = 'yellow';
    label = '🟡 Tight fit';
  } else {
    band = 'green';
    label = '🟢 Good fit';
  }
  ramBadge.textContent = label;
  ramBadge.className = `ram-badge band-${band}`;
  // Transparent breakdown — hover the badge to see the math the advisor used.
  const gib = (b) => (b / 1024 ** 3).toFixed(2);
  const archNote = arch
    ? `${arch.architecture} · ${arch.block_count}L · ${arch.head_count_kv}/${arch.head_count} KV/heads · ${kvPerToken.toLocaleString()} B/tok`
    : `unknown arch · assuming ${kvPerToken.toLocaleString()} B/tok (non-GQA worst case)`;
  ramBadge.title =
    `model ${gib(sizeBytes)} GiB + KV ${gib(kv)} GiB (${ctx.toLocaleString()} ctx) ` +
    `+ overhead ${gib(overhead)} GiB = ${gib(required)} GiB needed\n` +
    `available ${gib(avail)} GiB → ${gib(remaining)} GiB headroom (margin ${(margin * 100).toFixed(0)}%)\n` +
    archNote;
}

function archKvBytesPerTokenF16(arch) {
  const headDim = Math.floor(arch.embedding_length / Math.max(1, arch.head_count));
  return 2 * arch.block_count * arch.head_count_kv * headDim * 2;
}

// ---------------------------------------------------------------------------
// Launch / stop
// ---------------------------------------------------------------------------

launchBtn.addEventListener('click', async () => {
  if (!selectedModelId) return;
  const opt = modelSelect.selectedOptions[0];
  const sizeBytes = opt ? Number(opt.dataset.sizeBytes || 0) : 0;

  launchBtn.disabled = true;
  launchStatus.textContent = 'Starting model…';
  try {
    const r = await fetch('/api/launch', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({
        model_id: selectedModelId,
        model_size_bytes: sizeBytes || undefined,
        context_tokens: Number(contextSlider.value),
      }),
    });
    const data = await r.json();
    if (!r.ok) {
      launchStatus.textContent = `Error: ${data.error ?? r.statusText}`;
      launchBtn.disabled = false;
      return;
    }
    launchStatus.textContent = `Running (RAM: ${data.ram_band})`;
    syncLlamaState(true);
  } catch (err) {
    launchStatus.textContent = `Launch failed: ${err.message}`;
    launchBtn.disabled = false;
  }
});

stopBtn.addEventListener('click', async () => {
  stopBtn.disabled = true;
  if (inflight) inflight.abort();
  await fetch('/api/stop', { method: 'POST' });
  launchStatus.textContent = 'Model stopped.';
  syncLlamaState(false);
});

function syncLlamaState(running) {
  llamaRunning = running;
  launchBtn.hidden = running;
  launchBtn.disabled = false;
  stopBtn.hidden = !running;
  stopBtn.disabled = false;
  sendBtn.disabled = !running;
  chatInput.disabled = !running;
  if (running) {
    emptyHint.innerHTML = 'Model ready. Type a message below to start chatting.';
    chatTitle.textContent = selectedModelId || 'USBuddy';
  } else {
    emptyHint.innerHTML =
      'Pick a model in the sidebar and click <strong>Launch model</strong>.';
    chatTitle.textContent = 'USBuddy';
  }
}

// ---------------------------------------------------------------------------
// Sidebar toggle
// ---------------------------------------------------------------------------

sidebarToggleBtn.addEventListener('click', () => {
  const hidden = app.classList.toggle('sidebar-hidden');
  sidebarToggleBtn.title = hidden ? 'Show sidebar' : 'Hide sidebar';
  sidebarToggleBtn.setAttribute('aria-label', hidden ? 'Show sidebar' : 'Hide sidebar');
  sidebarToggleBtn.setAttribute('aria-expanded', String(!hidden));
});

newChatBtn.addEventListener('click', async () => {
  if (inflight) {
    inflight.abort();
    try {
      await activeStream;
    } catch {
      /* handled in streamReply */
    }
  }
  resetActiveChat();
});

function resetActiveChat() {
  chatHistory.length = 0;
  chatMessages.innerHTML = '';
  chatMessages.appendChild(emptyState);
  emptyState.hidden = false;
  currentChatId = null;
  currentChatCreatedAt = 0;
  currentChatAiTitled = false;
  stickToBottom = true;
  highlightActiveChat();
}

// ---------------------------------------------------------------------------
// Chat
// ---------------------------------------------------------------------------

function autoResize() {
  chatInput.style.height = 'auto';
  chatInput.style.height = Math.min(chatInput.scrollHeight, 240) + 'px';
}
chatInput.addEventListener('input', autoResize);

chatInput.addEventListener('keydown', (e) => {
  if (e.key === 'Enter' && !e.shiftKey) {
    e.preventDefault();
    chatForm.requestSubmit();
  }
});

chatForm.addEventListener('submit', async (e) => {
  e.preventDefault();
  const text = chatInput.value.trim();
  if (!text || !llamaRunning) return;
  chatInput.value = '';
  autoResize();

  // If a reply is still streaming, cancel it AND wait for streamReply's
  // finally block to push the partial assistant content into chatHistory.
  // Without this await, the next user message gets inserted before the
  // partial reply, scrambling the conversation order; without the abort,
  // llama-server queues the new request behind the old one.
  if (inflight) {
    inflight.abort();
    try {
      await activeStream;
    } catch {
      /* already handled inside streamReply */
    }
  }

  // A fresh send means the user is committing to this turn — always show its
  // reply, even if they had scrolled away from a previous one.
  stickToBottom = true;
  appendMessage('user', text);
  chatHistory.push({ role: 'user', content: text });
  activeStream = streamReply();
  try {
    await activeStream;
  } finally {
    activeStream = null;
  }
});

function appendMessage(role, content) {
  emptyState.hidden = true;
  // .msg is the full-width row (carries optional background tint); .msg-inner
  // is the max-width centered flex that holds the avatar and the body.
  const row = document.createElement('div');
  row.className = `msg ${role}`;
  const inner = document.createElement('div');
  inner.className = 'msg-inner';
  const role_el = document.createElement('div');
  role_el.className = 'msg-role';
  if (role === 'user') {
    role_el.textContent = 'You';
  } else {
    const img = document.createElement('img');
    img.src = '/assets/icon.png';
    img.alt = 'USBuddy';
    img.className = 'msg-role-avatar';
    role_el.appendChild(img);
  }
  const body = document.createElement('div');
  body.className = 'msg-body';
  body.innerHTML = renderMarkdown(content);
  wireCopyButtons(body);
  inner.append(role_el, body);
  row.appendChild(inner);
  chatMessages.appendChild(row);
  maybeScrollToBottom();
  return body;
}

function isNearBottom() {
  return (
    chatMessages.scrollHeight - chatMessages.scrollTop - chatMessages.clientHeight <
    STICK_THRESHOLD_PX
  );
}

function maybeScrollToBottom() {
  if (stickToBottom) {
    chatMessages.scrollTop = chatMessages.scrollHeight;
  }
}

chatMessages.addEventListener('scroll', () => {
  stickToBottom = isNearBottom();
});

stopGenBtn.addEventListener('click', () => {
  if (inflight) inflight.abort();
});

quitBtn.addEventListener('click', async () => {
  if (!confirm('Quit USBuddy? This stops the runtime entirely.')) return;
  quitBtn.disabled = true;
  quitBtn.textContent = 'Shutting down…';
  if (inflight) inflight.abort();
  try {
    await fetch('/api/shutdown', { method: 'POST' });
  } catch {
    /* expected — connection will drop */
  }
  document.body.innerHTML =
    '<div style="display:flex;align-items:center;justify-content:center;min-height:100vh;flex-direction:column;gap:16px;font-family:system-ui;color:#9aa0a6;background:#0f1115;">' +
    '<img src="/assets/icon.png" style="width:120px;opacity:.6">' +
    '<div>USBuddy stopped. You can close this tab.</div></div>';
});

function setStreaming(streaming) {
  stopGenBtn.hidden = !streaming;
  sendBtn.hidden = streaming;
}

async function streamReply() {
  // Placeholder assistant bubble we'll fill in as tokens arrive.
  const body = appendMessage('assistant', '');
  body.innerHTML = '<span class="cursor"></span>';
  let accum = '';
  const renderInto = (text) => {
    body.innerHTML = renderMarkdown(text) + '<span class="cursor"></span>';
    wireCopyButtons(body);
    maybeScrollToBottom();
  };

  inflight = new AbortController();
  setStreaming(true);
  let finishedCleanly = false;
  try {
    const r = await fetch('/api/chat/completions', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({
        model: selectedModelId,
        messages: chatHistory,
        stream: true,
      }),
      signal: inflight.signal,
    });
    if (!r.ok || !r.body) {
      const errPayload = await r.json().catch(() => ({}));
      body.innerHTML = `<em>Error: ${escapeHtml(formatApiError(errPayload, r.statusText))}</em>`;
      return;
    }
    const reader = r.body.getReader();
    const decoder = new TextDecoder();
    let buf = '';
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      buf += decoder.decode(value, { stream: true });
      let idx;
      while ((idx = buf.indexOf('\n')) !== -1) {
        const line = buf.slice(0, idx).trim();
        buf = buf.slice(idx + 1);
        if (!line.startsWith('data:')) continue;
        const data = line.slice(5).trim();
        if (data === '[DONE]') continue;
        try {
          const obj = JSON.parse(data);
          if (obj.error) {
            // Mid-stream error from llama-server (e.g. context overflow).
            throw new Error(formatApiError(obj, 'stream error'));
          }
          const delta = obj.choices?.[0]?.delta?.content;
          if (delta) {
            accum += delta;
            renderInto(accum);
          }
        } catch (parseErr) {
          if (parseErr instanceof Error && parseErr.message !== 'stream error') {
            // Real error (not just an unparseable keepalive line) — re-throw.
            if (!/Unexpected token|JSON/.test(parseErr.message)) throw parseErr;
          }
          // otherwise ignore non-JSON SSE comments / keepalives
        }
      }
    }
    finishedCleanly = true;
    body.innerHTML = renderMarkdown(accum || '(empty response)');
    wireCopyButtons(body);
  } catch (err) {
    if (err.name === 'AbortError') {
      body.innerHTML = renderMarkdown(accum || '_(stopped)_');
    } else {
      body.innerHTML =
        renderMarkdown(accum) +
        `<em class="stream-error">Error: ${escapeHtml(err.message || String(err))}</em>`;
    }
  } finally {
    // Always preserve whatever the assistant produced so the next turn keeps
    // its context — even if the user hit Stop or the stream errored.
    if (accum) {
      chatHistory.push({ role: 'assistant', content: accum });
    } else if (finishedCleanly) {
      chatHistory.push({ role: 'assistant', content: '' });
    }
    inflight = null;
    setStreaming(false);
    maybeScrollToBottom();
    await persistCurrentChat();
  }
}

function formatApiError(payload, fallback) {
  if (!payload) return fallback || 'unknown error';
  const e = payload.error;
  if (typeof e === 'string') return e;
  if (e && typeof e === 'object') {
    return e.message || e.type || e.code || JSON.stringify(e);
  }
  if (typeof payload.message === 'string') return payload.message;
  return fallback || 'unknown error';
}

function escapeHtml(s) {
  return String(s)
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;');
}

// ---------------------------------------------------------------------------
// Chat persistence + incognito
// ---------------------------------------------------------------------------

function syncIncognitoUi() {
  // Button labels are action verbs — they describe what clicking will DO,
  // not the current state. "Go incognito" means memory is currently on and
  // clicking turns it off; "Enable memory" means the opposite.
  const saving = saveChats;
  incognitoToggle.classList.toggle('saving', saving);
  incognitoToggle.querySelector('.incognito-label').textContent = saving
    ? 'Go incognito'
    : 'Enable memory';
  incognitoToggle.querySelector('.incognito-icon').textContent = saving ? '🕶️' : '💾';
  incognitoToggle.setAttribute('aria-label', saving ? 'Go incognito' : 'Enable memory');
  incognitoToggle.title = saving
    ? 'Stop saving chats to the drive — current chat will live only in RAM.'
    : 'Start saving chats to the drive under .usbuddy/chats/.';
}

incognitoToggle.addEventListener('click', async () => {
  const next = !saveChats;
  if (next) {
    const ok = confirm(
      'Save conversations to the drive?\n\n' +
        'Saved chats live under .usbuddy/chats/ on this USB stick in plaintext. ' +
        'Anyone who plugs the stick into a computer can read them. ' +
        'Keep incognito on if you are not sure.'
    );
    if (!ok) return;
  }
  saveChats = next;
  syncIncognitoUi();
  try {
    await fetch('/api/prefs', {
      method: 'PUT',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ save_chats: saveChats }),
    });
  } catch {
    /* best-effort; UI state remains the source of truth this session */
  }
  // If the user just enabled saving mid-conversation, persist what's there.
  if (saveChats && chatHistory.length > 0) {
    await persistCurrentChat();
    await refreshChats();
  }
});

async function persistCurrentChat() {
  if (!saveChats) return;
  if (chatHistory.length === 0) return;
  const now = Math.floor(Date.now() / 1000);
  const isNewChat = !currentChatId;
  if (isNewChat) {
    currentChatId = (crypto.randomUUID && crypto.randomUUID()) || fallbackUuid();
    currentChatCreatedAt = now;
  }
  const firstUser = chatHistory.find((m) => m.role === 'user');
  // Initial title is a naive slice — gives the sidebar something to show
  // immediately while the AI title call (below) runs out of band.
  const fallbackTitle =
    (firstUser?.content || 'Untitled').slice(0, 80).replace(/\s+/g, ' ').trim();

  await saveChatRecord({
    id: currentChatId,
    title: fallbackTitle,
    model_id: selectedModelId,
    created_epoch_secs: currentChatCreatedAt,
    updated_epoch_secs: now,
    messages: chatHistory.map((m) => ({ role: m.role, content: m.content })),
  });

  // Once the chat exists on disk with a placeholder, upgrade the title
  // asynchronously with a small model call. We only do this once per chat
  // (currentChatAiTitled gate) and only when llama is up and there's a
  // user message to summarize.
  if (!currentChatAiTitled && llamaRunning && firstUser?.content) {
    const aiTitle = await generateChatTitle(firstUser.content);
    currentChatAiTitled = true;
    if (aiTitle && currentChatId && saveChats) {
      await saveChatRecord({
        id: currentChatId,
        title: aiTitle,
        model_id: selectedModelId,
        created_epoch_secs: currentChatCreatedAt,
        updated_epoch_secs: now,
        messages: chatHistory.map((m) => ({ role: m.role, content: m.content })),
      });
    }
  }
}

async function saveChatRecord(chat) {
  try {
    const r = await fetch(`/api/chats/${chat.id}`, {
      method: 'PUT',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(chat),
    });
    if (!r.ok) return;
  } catch {
    /* drive may be gone — next refresh will reconcile */
    return;
  }
  // Bump the sidebar summary so this chat floats to the top without a full
  // reload. Newest-first ordering matches the server.
  const filtered = savedChats.filter((c) => c.id !== chat.id);
  savedChats = [
    {
      id: chat.id,
      title: chat.title,
      model_id: chat.model_id,
      updated_epoch_secs: chat.updated_epoch_secs,
    },
    ...filtered,
  ];
  renderChatsList();
}

/// Asks the active llama-server for a 2-3 word title summarizing the user's
/// opening message. Returns null on any failure — the slice fallback already
/// in place stays in that case. Times out after 20s so a wedged engine
/// never blocks the save loop indefinitely.
async function generateChatTitle(firstUserMessage) {
  const controller = new AbortController();
  const timeoutId = setTimeout(() => controller.abort(), 20_000);
  try {
    const r = await fetch('/api/chat/completions', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({
        model: selectedModelId,
        stream: false,
        temperature: 0.3,
        max_tokens: 16,
        messages: [
          {
            role: 'system',
            content:
              'You write a concise 2-3 word title summarizing the user message. ' +
              'Reply with ONLY the title — no quotes, no punctuation, no preamble, ' +
              'no explanation. Examples: "fix nginx config", "vacation ideas", ' +
              '"rust async basics".',
          },
          { role: 'user', content: firstUserMessage },
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

function cleanTitle(raw) {
  const cleaned = String(raw)
    .replace(/<think>[\s\S]*?<\/think>/gi, '') // strip CoT tags if model adds them
    .replace(/^["'`*_]+|["'`*_]+$/g, '')        // strip wrapping quotes / markdown
    .replace(/[.!?,;:]+$/g, '')                 // strip trailing punctuation
    .replace(/\s+/g, ' ')
    .trim();
  if (!cleaned) return null;
  // Cap at 4 words; smaller models sometimes ramble past the instruction.
  return cleaned.split(/\s+/).slice(0, 4).join(' ');
}

function fallbackUuid() {
  // crypto.randomUUID is universal in modern browsers, but a defensive
  // fallback keeps things working in oddball embedded WebViews.
  const r = crypto.getRandomValues(new Uint8Array(16));
  r[6] = (r[6] & 0x0f) | 0x40;
  r[8] = (r[8] & 0x3f) | 0x80;
  const h = [...r].map((b) => b.toString(16).padStart(2, '0'));
  return `${h.slice(0, 4).join('')}-${h.slice(4, 6).join('')}-${h.slice(6, 8).join('')}-${h.slice(8, 10).join('')}-${h.slice(10, 16).join('')}`;
}

async function refreshChats() {
  savedChats = await fetchChats();
  renderChatsList();
}

function renderChatsList() {
  chatsList.innerHTML = '';
  if (!savedChats.length) {
    chatsEmpty.hidden = false;
    return;
  }
  chatsEmpty.hidden = true;
  for (const summary of savedChats) {
    const li = document.createElement('li');
    li.dataset.chatId = summary.id;
    if (summary.id === currentChatId) li.classList.add('active');

    const titleEl = document.createElement('span');
    titleEl.className = 'chat-title-text';
    titleEl.textContent = summary.title || 'Untitled';

    const del = document.createElement('button');
    del.className = 'chat-delete';
    del.type = 'button';
    del.title = 'Delete chat';
    del.setAttribute('aria-label', 'Delete chat');
    del.textContent = '×';
    del.addEventListener('click', async (e) => {
      e.stopPropagation();
      if (!confirm(`Delete "${summary.title || 'Untitled'}"?`)) return;
      await deleteChat(summary.id);
    });

    li.append(titleEl, del);
    li.addEventListener('click', () => loadChat(summary.id));
    chatsList.appendChild(li);
  }
}

function highlightActiveChat() {
  for (const li of chatsList.children) {
    li.classList.toggle('active', li.dataset.chatId === currentChatId);
  }
}

async function loadChat(id) {
  if (inflight) {
    inflight.abort();
    try {
      await activeStream;
    } catch {
      /* handled in streamReply */
    }
  }
  let chat;
  try {
    const r = await fetch(`/api/chats/${id}`);
    if (!r.ok) throw new Error(`HTTP ${r.status}`);
    chat = await r.json();
  } catch (err) {
    statusLine.textContent = `Failed to load chat: ${err.message}`;
    return;
  }
  chatHistory.length = 0;
  chatMessages.innerHTML = '';
  emptyState.hidden = true;
  for (const m of chat.messages || []) {
    chatHistory.push({ role: m.role, content: m.content });
    appendMessage(m.role, m.content);
  }
  currentChatId = chat.id;
  currentChatCreatedAt = chat.created_epoch_secs || Math.floor(Date.now() / 1000);
  currentChatAiTitled = true; // existing chat already has its title; don't re-summarize
  chatTitle.textContent = chat.title || selectedModelId || 'USBuddy';
  stickToBottom = true;
  chatMessages.scrollTop = chatMessages.scrollHeight;
  highlightActiveChat();
}

async function deleteChat(id) {
  try {
    await fetch(`/api/chats/${id}`, { method: 'DELETE' });
  } catch {
    /* swallow; refresh will reconcile */
  }
  savedChats = savedChats.filter((c) => c.id !== id);
  if (currentChatId === id) {
    resetActiveChat();
  }
  renderChatsList();
}

// ---------------------------------------------------------------------------
// Start
// ---------------------------------------------------------------------------

boot().catch((err) => {
  statusLine.textContent = `Startup error: ${err.message}`;
});
