# Editor integration

USBuddy as the model behind a coding assistant — VS Code, Zed, JetBrains, and
the extension ecosystem (Continue, Cline, Roo Code) — while keeping
offline-by-default, no-phone-home, and no-host-footprint.

Plan of record. Phase 1 ships; phases 2 and 3 are designed.

---

## Protocol

The integration surface is an OpenAI-compatible `/v1` HTTP endpoint.

Every editor that supports local models takes a base URL speaking the OpenAI
chat-completions shape. That's what Ollama (`:11434/v1`), LM Studio, vLLM, and
LiteLLM expose, and what the editors' "custom model" boxes ask for.

`llama-server` already speaks OpenAI `/v1`, and the runtime already
reverse-proxies `/api/chat/**` → `/v1/chat/**` with SSE streaming intact. The
bridge is a second, auth-gated door onto that machinery.

MCP is a separate feature aimed at a different user story — see
[MCP](#mcp) at the end.

## Cursor

Cursor's "Override OpenAI Base URL" routes requests through Cursor's backend
(`api2.cursor.sh`), so Cursor's servers dial the URL, not your machine:

- `localhost` / `127.0.0.1` / private addresses are rejected.
- Plain HTTP is rejected; a public **HTTPS** origin is required.

Satisfying that requires a public tunnel (Cloudflare Tunnel, ngrok), which puts
your prompts and code through a third-party backend — the thing USBuddy exists
to avoid. Unsupported and undocumented by design.

**Use an extension inside Cursor.** Cursor is a VS Code fork and accepts VSIX
sideloads and OpenVSX extensions. Continue, Cline, and Roo Code run in the
extension host on your machine and dial `127.0.0.1` directly. Same for Windsurf,
Trae, and other forks: in-process extension works, cloud-routed base URL doesn't.

### Client compatibility

| Client | Reaches `127.0.0.1` | Notes |
| --- | --- | --- |
| Continue (VS Code / JetBrains) | yes | Best all-round target; chat + edit + autocomplete |
| Cline / Roo Code | yes | Agentic; needs tool calling and a large context |
| VS Code Copilot Chat BYOK | yes | "Custom OpenAI Compatible" provider |
| Zed | yes | `openai_compatible` provider in settings |
| JetBrains AI / AI Assistant | yes | Custom OpenAI-compatible endpoint |
| Aider, `llm`, `sgpt`, curl | yes | Plain `OPENAI_BASE_URL` consumers |
| **Cursor base-URL override** | **no** | Cloud-routed; use an extension |

---

## Phase 1 — the bridge (shipped)

A token-gated OpenAI-compatible surface on the runtime's existing port, off by
default, toggled from the chat UI with no restart.

### Endpoints

Served on the chat UI's listener (`127.0.0.1:8765` by default):

| Route | Purpose |
| --- | --- |
| `GET /v1/models` | Models present on the drive, OpenAI list shape |
| `POST /v1/chat/completions` | Chat, streaming and non-streaming |
| `POST /v1/completions` | Legacy/raw completion |

All three require `Authorization: Bearer <token>`; `x-api-key` is also accepted.
All three 404 with a pointed message when the bridge is off, so a stale editor
config fails legibly.

Errors use OpenAI's `{"error": {...}}` envelope so editors render them as
readable messages.

### Model-directed launch

An editor connects cold, possibly minutes after an idle-unload, and names its
model in the request body. The bridge resolves that `model` field against the
drive (catalog id → alias → drop-in file stem) and starts or swaps
`llama-server` itself, under the same RAM-fit gate `start_llama` enforces. Red
band refuses, returned as a 400 in OpenAI error shape.

Resolution order:

1. `model` names something on the drive → use it; swap if a different model is
   loaded.
2. `model` is absent, empty, or unrecognized **and** a model is loaded → serve
   the loaded one. Clients mangle and hardcode model names; a working completion
   beats a 400 on a request we can obviously satisfy.
3. Otherwise → 400 listing available ids.

A request that matches the loaded model and context is served as-is. A changed
`bridge_ctx_tokens` triggers one reload on the next request.

**One model at a time.** The bridge and the chat UI share a single
`llama-server`. Naming a different model in your editor swaps it out from under
an open chat tab, matching Ollama. Phase 2 revisits this.

### Context length

The chat UI's `4096` is far too small for coding work — Cline and Roo Code
routinely send 20k+ tokens of file context. Bridge launches use
`bridge_ctx_tokens` (default **16384**), capped to the model's trained context
from the GGUF header. The RAM advisor models KV cache growth, so a context that
won't fit is caught by the existing gate.

### Tool calling

`llama-server` emits OpenAI-shaped `tool_calls` only when started with
`--jinja`, which applies the model's own chat template. Without it, agent mode
in Cline/Roo/Continue doesn't work. The runtime passes `--jinja` on every spawn,
chat UI launches included — better behavior for templated models generally.

**The model still has to hold up its end.** Verified against
`Qwen2.5-Coder-7B-Instruct-abliterated`: the template's tool section applies
correctly and the model clearly sees the tool definitions and tries to call one,
but it emits `<tools>{…}</tools>` where Qwen's template specifies
`<tool_call>{…}</tool_call>`. llama.cpp's parser declines it and `tool_calls`
comes back null. Reproducible at temperature 0.

Abliterated and heavily re-fine-tuned variants often lose this exact format
adherence while their template still advertises tools. Verify any catalog
"works with agent mode" claim per model by asserting a non-null `tool_calls` in
a real response. The template's contents prove nothing.

### Auth, binding, and origin

- The listener stays bound to `127.0.0.1`. Never `0.0.0.0`.
- A token is generated from the OS CSPRNG on first enable and stored at
  `.usbuddy/bridge-token` — same posture as `hf-token`, plain-text and greppable
  on purpose. It persists across sessions so an editor configured once keeps
  working. A rotate button invalidates it.
- **Origin allowlist over the whole HTTP surface.** This closes a pre-existing
  hole: `/api/*` previously had no auth and no origin check, so any web page you
  visited could `POST` to `127.0.0.1:8765` and drive the model, enumerate
  `/api/chats`, or hit `/api/shutdown-eject`. Requests now pass only when
  `Origin` is absent (non-browser clients — editors, curl, extension hosts) or
  matches `http://127.0.0.1:{port}` / `http://localhost:{port}`. `*` is never
  used.

### Discovery

No host writes. The footprint invariant forbids dropping a config file in
`$HOME` for extensions to find. Clients probe
`http://127.0.0.1:8765/v1/models`; a future extension can sweep a small port
range. The stick leaves nothing behind.

### Control surface

`RuntimePrefs` in `.usbuddy/runtime-prefs.toml` carries `bridge_enabled` and
`bridge_ctx_tokens`. Routes register unconditionally and gate on the pref, so
the toggle takes effect live.

- `GET /api/bridge` → enabled, ctx, base URL, token
- `PUT /api/bridge` → set enabled / ctx
- `POST /api/bridge/rotate` → new token

`PUT /api/prefs` takes a merge patch. The chat header's incognito switch and the
bridge panel each own different fields, and a full-object PUT from either would
reset the other's.

The sidebar gains a **Developer bridge** section: a switch plus a setup panel
with the endpoint, the token (masked, copyable), a context slider, and config
snippets for Continue, VS Code, Zed, Cline, and shell `OPENAI_BASE_URL` use.

---

## Phase 2 — autocomplete (designed)

Inline tab-completion is what makes a coding assistant feel native, and it's a
different workload from chat:

- **FIM** (fill-in-middle) via `llama-server`'s `/infill` — prefix and suffix
  around the cursor.
- A **different, much smaller model**: a 1.5B–3B *base* model trained with FIM
  tokens (Qwen2.5-Coder-1.5B/3B-base is the reference). Chat fine-tunes are the
  wrong shape, and instruction-tuned 7B+ models are far too slow for keystroke
  latency.
- **Two resident models**, which breaks the single-`llama-server` assumption.

The work, in order:

1. A second `llama-server` slot in `RuntimeState`, spawned on `--port` +1 with
   its own, much shorter idle timer — the autocomplete model is small and cheap
   to reload.
2. **A combined RAM gate.** `assess_fit` must price the *sum* of both resident
   models plus both KV caches. This is the load-bearing change: getting it wrong
   reintroduces swap-to-disk, the #1 footprint leak the advisor exists to
   prevent. The chat model's launch must be re-gated when a completion model is
   resident, and vice versa.
3. `POST /v1/fim/completion` (Continue's shape) plus a passthrough to `/infill`.
4. Catalog: a `coding` profile and FIM-capable base-model entries in
   `seed.toml`, with a `fim: true` marker and the model's FIM token triple so
   the runtime doesn't guess.

Ship the combined gate before autocomplete. A laptop that green-lights a 7B chat
model and then quietly also loads a 3B completion model is the exact failure
this project promises to prevent.

## Phase 3 — the VS Code extension (designed)

A thin extension registering a `LanguageModelChatProvider` puts USBuddy directly
in the VS Code chat model picker — no BYOK dialog, no pasted base URL — and
sideloads into Cursor and other forks as a VSIX. It would:

- Probe `127.0.0.1` for a running USBuddy and show a status-bar item when found.
- Enumerate `/v1/models` into the picker.
- Read the token from a workspace/user setting, with a "paste from USBuddy"
  prompt.
- Fail with a real message when the stick is unplugged.

Highest polish, highest ongoing maintenance — the VS Code LM provider API is
still moving. Ships as its own package in `editors/vscode/`, versioned
independently of the drive, and stays **off the stick**: it installs on the host
and is therefore host footprint, which the user opts into knowingly, once, as a
normal extension.

## MCP

MCP's role here is a separate feature from the bridge.

Expose USBuddy as an **MCP server** offering tools like `summarize_locally`,
`classify_locally`, `redact_locally`. A cloud assistant — Claude Code, Copilot —
hands a privacy-sensitive chunk to the offline model on the stick and gets back
only the result. The sensitive bytes stay on the machine; the cloud model sees
the summary it asked for.

On-brand and differentiated, but it serves "I use a cloud assistant and want a
local escape hatch," while this document serves "I want my editor to run
entirely on my own hardware." It belongs in its own design doc.

---

## Invariants this plan preserves

- Offline by default; the bridge does no network I/O beyond loopback.
- No background work, no phone-home; the bridge acts only on editor requests.
- RAM-fit gates every load, bridge-initiated included; Red refuses.
- Drive writes during a session happen only on explicit user action (the bridge
  toggle and token are user-initiated prefs writes via `usbuddy_core::atomic`).
- No host footprint: no config files, no daemons, no registry keys. Discovery is
  by probe.
- Loopback binding only, token-gated, origin-checked.
