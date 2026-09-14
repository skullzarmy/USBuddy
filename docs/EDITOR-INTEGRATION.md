# Editor integration

How USBuddy becomes the model behind a coding assistant — VS Code, Zed,
JetBrains, and the extension ecosystem (Continue, Cline, Roo Code) — without
giving up offline-by-default, no-phone-home, or no-host-footprint.

This document is the plan of record. Phase 1 is implemented; phases 2 and 3
are designed but not built.

---

## The protocol decision

**The integration surface is an OpenAI-compatible `/v1` HTTP endpoint, not MCP.**

MCP is how an assistant acquires *tools and context*. It makes a program a tool
provider to some other model. It is the wrong direction for "let my editor's
assistant think with the model on my USB stick." Every editor and extension that
supports local models does so by pointing at a base URL that speaks the OpenAI
chat-completions shape — that is what Ollama (`:11434/v1`), LM Studio, vLLM, and
LiteLLM all expose, and it is what the editors' "custom model" configuration
boxes ask for.

USBuddy is unusually well positioned for this: `llama-server` already speaks
OpenAI `/v1` natively, and the runtime already reverse-proxies `/api/chat/**` →
`/v1/chat/**` with SSE streaming intact. The bridge is a second, auth-gated
door onto machinery that already exists.

MCP still has a legitimate role here, but as a *different feature* — see
[MCP, properly scoped](#mcp-properly-scoped) at the end.

## The Cursor constraint

**Cursor cannot talk to `127.0.0.1`, and the workaround is disqualifying.**

Cursor's "Override OpenAI Base URL" does not make the editor call your endpoint.
Requests route through Cursor's backend (`api2.cursor.sh`), so Cursor's servers
are what dial the base URL. Consequences:

- `localhost` / `127.0.0.1` / private addresses are unreachable and rejected.
- Plain HTTP is rejected; a public **HTTPS** origin is required.

The only way to satisfy that is a public tunnel (Cloudflare Tunnel, ngrok),
which means your prompts and your code leave the machine and transit a
third-party backend. That is the precise thing USBuddy exists to avoid. We do
not support, document, or make it easy.

**The supported Cursor path is an extension inside Cursor.** Cursor is a VS Code
fork and accepts VSIX sideloads and OpenVSX extensions. Continue, Cline, and Roo
Code run in the extension host on your machine and dial `127.0.0.1` directly.
Same for any other Cursor-like fork (Windsurf, Trae) — the rule is *in-process
extension good, cloud-routed base URL bad*.

### Client compatibility summary

| Client | Reaches `127.0.0.1` directly | Notes |
| --- | --- | --- |
| Continue (VS Code / JetBrains) | yes | Best all-round target; chat + edit + autocomplete |
| Cline / Roo Code | yes | Agentic; needs tool calling and a large context |
| VS Code Copilot Chat BYOK | yes | "Custom OpenAI Compatible" provider |
| Zed | yes | `openai_compatible` provider in settings |
| JetBrains AI / AI Assistant | yes | Custom OpenAI-compatible endpoint |
| Aider, `llm`, `sgpt`, curl | yes | Plain `OPENAI_BASE_URL` consumers |
| **Cursor base-URL override** | **no** | Cloud-routed; see above. Use an extension instead |

---

## Phase 1 — the bridge (implemented)

A token-gated OpenAI-compatible surface on the runtime's existing port, off by
default, flipped on from the chat UI with no restart.

### Endpoints

Served on the same listener as the chat UI (`127.0.0.1:8765` by default):

| Route | Purpose |
| --- | --- |
| `GET /v1/models` | Lists models actually present on the drive, OpenAI list shape |
| `POST /v1/chat/completions` | Chat, streaming and non-streaming |
| `POST /v1/completions` | Legacy/raw completion, for clients that use it |

All three require `Authorization: Bearer <token>` (`x-api-key` also accepted for
clients that send that instead). All three 404 with a pointed message when the
bridge is disabled, so a stale editor config fails legibly instead of hanging.

Errors are returned in OpenAI's `{"error": {...}}` envelope so the editors
surface them as readable messages rather than raw 500s.

### Model-directed launch

The chat UI's launch flow cannot be a prerequisite. An editor connects cold,
possibly minutes after an idle-unload, and names its model in the request body.
So the bridge handlers resolve the request's `model` field against the drive
(catalog id → alias → drop-in file stem) and start or swap `llama-server`
themselves, running the same RAM-fit gate `start_llama` already enforces. Red
band still refuses — returned as a 400 in OpenAI error shape.

Resolution rules, in order:

1. `model` names something on the drive → use it; swap if a different model is
   currently loaded.
2. `model` is absent, empty, or unrecognized, **and** a model is already loaded
   → use the loaded one. (Some clients mangle or hardcode model names; being
   forgiving here beats failing a request we can obviously serve.)
3. Otherwise → 400 listing the available ids.

**One model at a time.** The bridge and the chat UI share a single
`llama-server`. Naming a different model from your editor swaps it out from
under the chat tab, matching how Ollama behaves. Phase 2 revisits this.

### Context length

`4096` — the chat UI's default — is far too small for coding work; Cline and
Roo Code routinely send 20k+ tokens of file context. Bridge-initiated launches
use `bridge_ctx_tokens` (default **16384**), capped to the model's trained
context length read from the GGUF header. The RAM advisor already models KV
cache growth, so a context that will not fit gets caught by the existing gate
rather than by an OOM.

### Tool calling

`llama-server` only emits OpenAI-shaped `tool_calls` when started with
`--jinja`, which makes it apply the model's own chat template. Without it,
agent mode in Cline/Roo/Continue does not work at all. The runtime now passes
`--jinja` on every spawn, chat UI launches included — it is strictly better
behavior for templated models, not a bridge-only concession.

**`--jinja` is necessary but not sufficient, and the difference is the model.**
Verified against `Qwen2.5-Coder-7B-Instruct-abliterated`: the template's tool
section is applied correctly (the model clearly sees the tool definitions and
tries to call one), but the model emits `<tools>{…}</tools>` instead of Qwen's
`<tool_call>{…}</tool_call>`, so llama.cpp's parser declines it and
`tool_calls` comes back null. Reproducible at temperature 0.

Abliterated and heavily re-fine-tuned variants frequently lose this kind of
exact format adherence even when their template still advertises tools. Any
future "works with agent mode" claim in the catalog must be verified per model
by asserting a non-null `tool_calls` in the response — never inferred from the
template's contents.

### Auth, binding, and origin

- The listener stays bound to `127.0.0.1`. Never `0.0.0.0`.
- A token is generated from the OS CSPRNG on first enable and stored at
  `.usbuddy/bridge-token` on the drive — same posture as `hf-token`, greppable
  and plain-text on purpose. It survives across sessions so the editor does not
  need reconfiguring every plug-in. A rotate button invalidates it.
- **Origin allowlist over the whole HTTP surface.** This closes a pre-existing
  hole worth naming: before this change, `/api/*` had no auth and no origin
  check, so any web page you happened to visit could `POST` to
  `127.0.0.1:8765` and drive the model, enumerate `/api/chats`, or hit
  `/api/shutdown-eject`. Requests now pass only when the `Origin` header is
  absent (non-browser clients — every editor, curl, extension host) or matches
  `http://127.0.0.1:{port}` / `http://localhost:{port}`. `*` is never used.

### Discovery

No host writes. The footprint invariant means the runtime must not drop a
config file in `$HOME` for extensions to find. Clients probe
`http://127.0.0.1:8765/v1/models` instead; a future extension can sweep a small
port range. The stick leaves nothing behind.

### Control surface

`RuntimePrefs` in `.usbuddy/runtime-prefs.toml` gains `bridge_enabled` and
`bridge_ctx_tokens`. Routes register unconditionally and gate on the pref, so
the toggle takes effect live.

- `GET /api/bridge` → enabled, ctx, base URL, token
- `PUT /api/bridge` → set enabled / ctx
- `POST /api/bridge/rotate` → new token

The chat UI's sidebar gains a **Developer bridge** section: a switch, and a
setup panel showing the endpoint, the token (masked, copyable), a context
slider, and ready-made config snippets for Continue, VS Code, Zed, Cline, and
shell `OPENAI_BASE_URL` use.

---

## Phase 2 — autocomplete (designed, not built)

Chat is the easy half. Inline tab-completion is what makes a coding assistant
feel native, and it is a genuinely different workload:

- It needs **FIM** (fill-in-middle) via `llama-server`'s `/infill`, not chat
  completions — prefix and suffix around the cursor.
- It needs a **different, much smaller model**: a 1.5B–3B *base* model trained
  with FIM tokens (Qwen2.5-Coder-1.5B/3B-base is the reference choice). Chat
  fine-tunes are wrong for this and instruction-tuned 7B+ models are far too
  slow for keystroke latency.
- Therefore it needs **two resident models**, which breaks the single
  `llama-server` assumption.

The work, in order:

1. A second `llama-server` slot in `RuntimeState` (completion engine), spawned
   with `--port` +1 and its own idle timer — a much shorter one, since the
   autocomplete model is small and cheap to reload.
2. **A combined RAM gate.** `assess_fit` must price the *sum* of both resident
   models plus both KV caches, not each independently. This is the load-bearing
   change: getting it wrong reintroduces swap-to-disk, which is the #1 footprint
   leak the advisor exists to prevent. The chat model's launch must be re-gated
   when a completion model is resident and vice versa.
3. `POST /v1/fim/completion` (Continue's shape) and a passthrough to `/infill`.
4. Catalog work: a `coding` profile and FIM-capable base-model entries in
   `seed.toml`, with a `fim: true` marker and the model's FIM token triple so
   the runtime does not have to guess.

Until the combined gate exists, do not ship autocomplete. A laptop that
green-lights a 7B chat model and then quietly also loads a 3B completion model
is exactly the failure this project promises not to have.

## Phase 3 — the VS Code extension (designed, not built)

A thin extension registering a `LanguageModelChatProvider` makes USBuddy appear
natively in the VS Code chat model picker — no BYOK dialog, no pasted base URL —
and sideloads into Cursor and other forks as a VSIX. It would:

- Probe `127.0.0.1` for a running USBuddy, show a status-bar item when found.
- Enumerate `/v1/models` into the picker.
- Read the token from a workspace/user setting, with a "paste from USBuddy"
  prompt.
- Degrade with a real message when the stick is unplugged, rather than timing
  out.

This is the highest-polish path and the highest ongoing maintenance — the VS
Code LM provider API is still moving. It ships as its own package in
`editors/vscode/`, versioned independently of the drive, and is explicitly **not
bundled on the stick** (it installs on the host, so it is host footprint by
definition — the user opts into it knowingly, once, as a normal extension).

## MCP, properly scoped

There *is* a good MCP feature here; it is just not the integration layer.

Expose USBuddy as an **MCP server** offering tools like `summarize_locally`,
`classify_locally`, `redact_locally`. Then a cloud assistant — Claude Code,
Copilot — can hand a privacy-sensitive chunk to the offline model on the stick
and get back only the result. The sensitive bytes never leave the machine; the
cloud model sees the summary it asked for.

That is on-brand and genuinely differentiated, but it is a separate product
surface aimed at a different user story ("I use a cloud assistant and want a
local escape hatch") from the one this document addresses ("I want my editor to
run entirely on my own hardware"). It belongs in its own design doc, and it
should not be conflated with the bridge.

---

## Invariants this plan does not relax

- Offline by default; the bridge does no network I/O beyond loopback.
- No background work, no phone-home; the bridge only acts on editor requests.
- RAM-fit gates every load, bridge-initiated ones included; Red still refuses.
- Nothing is written to the drive during a session except explicit user
  actions (the bridge toggle and token are user-initiated prefs writes, using
  `usbuddy_core::atomic`).
- No host footprint: no config files, no daemons, no registry keys. Discovery
  is by probe.
- Loopback binding only, token-gated, origin-checked.
