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

// ---------------------------------------------------------------------------
// DOM
// ---------------------------------------------------------------------------

const $ = (id) => document.getElementById(id);
const app = document.querySelector('.app');

const sidebarToggle = $('sidebar-toggle');
const sidebarShow = $('sidebar-show');
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

// ---------------------------------------------------------------------------
// Boot
// ---------------------------------------------------------------------------

async function boot() {
  try {
    const payload = await fetchStatus();
    applyStatus(payload);
    syncLlamaState(payload.llama_running);
  } catch (err) {
    statusLine.textContent = `Runtime API unavailable: ${err.message}`;
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
  renderModelOptions(payload.models, payload.drop_in_models, payload.ram_previews);
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

function renderModelOptions(catalogModels, dropIns, ramPreviews) {
  const items = [
    ...catalogModels.map((m, i) => ({
      id: m.id,
      label: m.display_name,
      profile: m.profile,
      sizeBytes: m.size_bytes,
      ram: ramPreviews?.[i] ?? null,
    })),
    ...dropIns.map((d) => ({
      id: (d.path.split(/[\\/]/).pop() ?? d.display_name).replace(/\.gguf$/i, ''),
      label: d.display_name,
      profile: d.profile,
      sizeBytes: 0,
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
    modelSelect.appendChild(opt);
  }

  if (!selectedModelId || !items.find((m) => m.id === selectedModelId)) {
    selectedModelId = items[0].id;
  }
  modelSelect.value = selectedModelId;
  updateRamBadge();
}

modelSelect.addEventListener('change', () => {
  selectedModelId = modelSelect.value;
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
  const ctx = Number(contextSlider.value);
  const kv = ctx * 131_072;
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

sidebarToggle.addEventListener('click', () => {
  app.classList.add('sidebar-hidden');
});
sidebarShow.addEventListener('click', () => {
  app.classList.remove('sidebar-hidden');
  sidebarShow.hidden = true;
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
  chatHistory.length = 0;
  chatMessages.innerHTML = '';
  chatMessages.appendChild(emptyState);
  emptyState.hidden = false;
});

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
  const wrap = document.createElement('div');
  wrap.className = `msg ${role}`;
  const role_el = document.createElement('div');
  role_el.className = 'msg-role';
  role_el.textContent = role === 'user' ? 'You' : 'AI';
  const body = document.createElement('div');
  body.className = 'msg-body';
  body.innerHTML = renderMarkdown(content);
  wireCopyButtons(body);
  wrap.append(role_el, body);
  chatMessages.appendChild(wrap);
  chatMessages.scrollTop = chatMessages.scrollHeight;
  return body;
}

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
    chatMessages.scrollTop = chatMessages.scrollHeight;
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
    chatMessages.scrollTop = chatMessages.scrollHeight;
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
// Start
// ---------------------------------------------------------------------------

boot().catch((err) => {
  statusLine.textContent = `Startup error: ${err.message}`;
});
