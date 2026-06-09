// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

let state = null;
let selectedModelId = null;
let llamaRunning = false;
const chatHistory = [];

// ---------------------------------------------------------------------------
// DOM refs
// ---------------------------------------------------------------------------

const statusEl = document.getElementById('status');
const versionEl = document.getElementById('version');
const ramInfoEl = document.getElementById('ram-info');
const platformEl = document.getElementById('platform');
const modelListEl = document.getElementById('model-list');
const noModelsEl = document.getElementById('no-models');
const pickerSection = document.getElementById('picker-section');
const contextRow = document.getElementById('context-row');
const contextSlider = document.getElementById('context-slider');
const contextValue = document.getElementById('context-value');
const ramBadge = document.getElementById('ram-badge');
const launchControls = document.getElementById('launch-controls');
const launchBtn = document.getElementById('launch-btn');
const stopBtn = document.getElementById('stop-btn');
const launchStatus = document.getElementById('launch-status');
const chatSection = document.getElementById('chat-section');
const chatMessages = document.getElementById('chat-messages');
const chatForm = document.getElementById('chat-form');
const chatInput = document.getElementById('chat-input');
const advisoriesSection = document.getElementById('advisories-section');
const advisoriesEl = document.getElementById('advisories');

// ---------------------------------------------------------------------------
// Boot
// ---------------------------------------------------------------------------

async function boot() {
  try {
    const payload = await fetchStatus();
    applyStatus(payload);
    syncLlamaState(payload.llama_running);
  } catch (err) {
    statusEl.textContent = `Runtime API unavailable: ${err.message}`;
  }
}

async function fetchStatus() {
  const r = await fetch('/api/status');
  if (!r.ok) throw new Error(`HTTP ${r.status}`);
  return r.json();
}

// ---------------------------------------------------------------------------
// Render
// ---------------------------------------------------------------------------

function applyStatus(payload) {
  state = payload;
  statusEl.textContent = payload.message;
  versionEl.textContent = payload.current?.active ?? 'uninitialized';
  platformEl.textContent = `${payload.platform.os} / ${payload.platform.arch}`;

  const availGib = (payload.ram.available_bytes / (1024 ** 3)).toFixed(1);
  const totalGib = (payload.ram.total_bytes / (1024 ** 3)).toFixed(1);
  ramInfoEl.textContent = `${availGib} GiB free of ${totalGib} GiB`;

  renderAdvisories(payload.advisories);
  renderModelList(payload.models, payload.drop_in_models, payload.ram_previews);
}

function renderAdvisories(advisories) {
  if (!advisories.length) { advisoriesSection.hidden = true; return; }
  advisoriesSection.hidden = false;
  advisoriesEl.innerHTML = '';
  for (const a of advisories) {
    const li = document.createElement('li');
    li.textContent = `[${a.severity.toUpperCase()}] ${a.id}: ${a.summary}`;
    advisoriesEl.appendChild(li);
  }
}

function renderModelList(catalogModels, dropIns, ramPreviews) {
  const allModels = [
    ...catalogModels.map((m, i) => ({ id: m.id, label: m.display_name, profile: m.profile, sizeBytes: m.size_bytes, ram: ramPreviews[i] })),
    ...dropIns.map(d => ({ id: d.display_name.replace(/\s+/g, '-').toLowerCase(), label: d.display_name, profile: d.profile, sizeBytes: 0, ram: null })),
  ];

  modelListEl.innerHTML = '';

  if (!allModels.length) {
    noModelsEl.hidden = false;
    contextRow.hidden = true;
    launchControls.hidden = true;
    return;
  }
  noModelsEl.hidden = true;

  for (const m of allModels) {
    const row = document.createElement('div');
    row.className = 'model-row';
    row.dataset.modelId = m.id;
    row.dataset.sizeBytes = m.sizeBytes;

    const radio = document.createElement('input');
    radio.type = 'radio';
    radio.name = 'model';
    radio.id = `model-${m.id}`;
    radio.value = m.id;

    const lbl = document.createElement('label');
    lbl.htmlFor = `model-${m.id}`;

    const nameSpan = document.createElement('span');
    nameSpan.className = 'model-name';
    nameSpan.textContent = m.label;

    const metaSpan = document.createElement('span');
    metaSpan.className = `model-meta profile-${m.profile}`;
    const sizeLabel = m.sizeBytes ? ` · ${(m.sizeBytes / 1024 ** 3).toFixed(1)} GiB` : '';
    metaSpan.textContent = `${m.profile}${sizeLabel}`;

    const ramSpan = document.createElement('span');
    if (m.ram) {
      ramSpan.className = `ram-dot band-${m.ram.band}`;
      ramSpan.title = `RAM: ${m.ram.band}`;
    }

    lbl.append(nameSpan, metaSpan, ramSpan);
    row.append(radio, lbl);
    row.addEventListener('click', () => selectModel(m));
    modelListEl.appendChild(row);
  }

  contextRow.hidden = false;
  launchControls.hidden = false;
}

function selectModel(m) {
  selectedModelId = m.id;
  document.querySelectorAll('.model-row').forEach(r => r.classList.remove('selected'));
  const row = modelListEl.querySelector(`[data-model-id="${m.id}"]`);
  if (row) {
    row.classList.add('selected');
    row.querySelector('input[type=radio]').checked = true;
  }
  updateRamBadge(m.sizeBytes, parseInt(contextSlider.value, 10));
}

function updateRamBadge(sizeBytes, contextTokens) {
  if (!sizeBytes || !state) { ramBadge.textContent = ''; return; }
  // 131_072 bytes = 128 KiB per token (llama.cpp KV-cache default).
  const kv = contextTokens * 131_072;
  const overhead = 512 * 1024 * 1024;
  const required = sizeBytes + kv + overhead;
  const available = state.ram.available_bytes;
  const remaining = available - required;
  const margin = remaining / required;

  let band, label;
  if (remaining < 0 || remaining < 1_073_741_824) {
    band = 'red'; label = '🔴 Won\'t fit';
  } else if (margin < 0.2 || remaining < 3_221_225_472) {
    band = 'yellow'; label = '🟡 Tight fit';
  } else {
    band = 'green'; label = '🟢 Good fit';
  }
  ramBadge.textContent = label;
  ramBadge.className = `ram-badge band-${band}`;
}

// ---------------------------------------------------------------------------
// Context slider
// ---------------------------------------------------------------------------

contextSlider.addEventListener('input', () => {
  const tokens = parseInt(contextSlider.value, 10);
  contextValue.textContent = `${tokens.toLocaleString()} tokens`;
  if (selectedModelId) {
    const row = modelListEl.querySelector(`[data-model-id="${selectedModelId}"]`);
    const sizeBytes = row ? parseInt(row.dataset.sizeBytes, 10) : 0;
    updateRamBadge(sizeBytes, tokens);
  }
});

// ---------------------------------------------------------------------------
// Launch / stop
// ---------------------------------------------------------------------------

launchBtn.addEventListener('click', async () => {
  if (!selectedModelId) { launchStatus.textContent = 'Select a model first.'; return; }
  const row = modelListEl.querySelector(`[data-model-id="${selectedModelId}"]`);
  const sizeBytes = row ? parseInt(row.dataset.sizeBytes, 10) : 0;

  launchBtn.disabled = true;
  launchStatus.textContent = 'Starting model…';
  try {
    const r = await fetch('/api/launch', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({
        model_id: selectedModelId,
        model_size_bytes: sizeBytes || undefined,
        context_tokens: parseInt(contextSlider.value, 10),
      }),
    });
    const data = await r.json();
    if (!r.ok) { launchStatus.textContent = `Error: ${data.error}`; launchBtn.disabled = false; return; }
    launchStatus.textContent = `Model running (RAM band: ${data.ram_band})`;
    syncLlamaState(true);
  } catch (err) {
    launchStatus.textContent = `Launch failed: ${err.message}`;
    launchBtn.disabled = false;
  }
});

stopBtn.addEventListener('click', async () => {
  stopBtn.disabled = true;
  await fetch('/api/stop', { method: 'POST' });
  launchStatus.textContent = 'Model stopped.';
  syncLlamaState(false);
  chatHistory.length = 0;
  chatMessages.innerHTML = '';
});

function syncLlamaState(running) {
  llamaRunning = running;
  launchBtn.hidden = running;
  launchBtn.disabled = false;
  stopBtn.hidden = !running;
  stopBtn.disabled = false;
  chatSection.hidden = !running;
}

// ---------------------------------------------------------------------------
// Chat
// ---------------------------------------------------------------------------

chatForm.addEventListener('submit', async (e) => {
  e.preventDefault();
  const text = chatInput.value.trim();
  if (!text) return;
  chatInput.value = '';
  addChatMessage('user', text);
  chatHistory.push({ role: 'user', content: text });
  await sendChat();
});

chatInput.addEventListener('keydown', (e) => {
  if (e.key === 'Enter' && !e.shiftKey) {
    e.preventDefault();
    chatForm.requestSubmit();
  }
});

async function sendChat() {
  const thinkingEl = addChatMessage('assistant', '…');
  try {
    const r = await fetch('/api/chat/completions', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({
        model: selectedModelId,
        messages: chatHistory,
        stream: false,
      }),
    });
    if (!r.ok) {
      const err = await r.json().catch(() => ({ error: r.statusText }));
      thinkingEl.textContent = `Error: ${err.error ?? r.statusText}`;
      return;
    }
    const data = await r.json();
    const reply = data.choices?.[0]?.message?.content ?? '(empty response)';
    thinkingEl.textContent = reply;
    chatHistory.push({ role: 'assistant', content: reply });
  } catch (err) {
    thinkingEl.textContent = `Error: ${err.message}`;
  }
  chatMessages.scrollTop = chatMessages.scrollHeight;
}

function addChatMessage(role, text) {
  const div = document.createElement('div');
  div.className = `chat-msg ${role}`;
  div.textContent = text;
  chatMessages.appendChild(div);
  chatMessages.scrollTop = chatMessages.scrollHeight;
  return div;
}

// ---------------------------------------------------------------------------
// Start
// ---------------------------------------------------------------------------

boot().catch((err) => {
  statusEl.textContent = `Startup error: ${err.message}`;
});

