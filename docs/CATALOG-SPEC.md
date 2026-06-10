# Catalog format

The catalog is the integrity root of every USBuddy install. It tells the
runtime which models are blessed, their SHA256s for verification, their
licenses, and which advisories apply. If you're forking USBuddy's catalog,
mirroring it for an internal deployment, or contributing a model entry to
the official catalog, this is the document you need.

The JSON schema lives at [`schemas/catalog.schema.json`](../schemas/catalog.schema.json)
and is enforced at parse time by `usbuddy-core`. This page is the prose
explanation.

## Versioning

Every catalog declares its schema version at the root:

```json
{
  "schema": "usbuddy.catalog/v1",
  ...
}
```

USBuddy refuses to load a catalog whose schema it doesn't know. There is
no graceful degradation — an unknown schema is a hard error with a
"please upgrade USBuddy" message. The point is to make breaking changes
visible instead of silently dropping fields the catalog assumed would be
honored.

When the schema changes incompatibly, the version string changes. v1 is
the only version that exists today.

## Root object

```json
{
  "schema": "usbuddy.catalog/v1",
  "runtime": { "min": "0.1.0", "max": "0.1.99" },
  "models": [ /* ... */ ],
  "advisories": [ /* ... */ ],
  "generated_at": "2026-06-01T00:00:00Z",
  "source": "https://github.com/skullzarmy/USBuddy"
}
```

| Field          | Required | Meaning                                                                                                                      |
| -------------- | -------- | ---------------------------------------------------------------------------------------------------------------------------- |
| `schema`       | yes      | Must be `usbuddy.catalog/v1`.                                                                                                |
| `runtime.min`  | yes      | Minimum runtime version that can use this catalog (semver). Older runtimes refuse to load it.                                |
| `runtime.max`  | yes      | Maximum runtime version that can use this catalog (semver). Newer runtimes treat that as a signal to upgrade the catalog.    |
| `models`       | yes      | Flat array of model entries. May be empty.                                                                                   |
| `advisories`   | yes      | Array of security or compatibility advisories. May be empty.                                                                 |
| `generated_at` | no       | ISO-8601 timestamp the catalog snapshot was produced. Informational.                                                         |
| `source`       | no       | URL where this catalog originated. Used by the UI to label custom catalogs as separate trust decisions.                      |

## Model entries

Each downloadable artifact is one flat entry. A model family (e.g. Qwen
2.5 7B at Q4_K_M and Q5_K_M) is two entries sharing a `family_id`, not
one entry with quant variants.

```json
{
  "id": "qwen2.5-7b-instruct-q4_k_m",
  "family_id": "qwen2.5-7b-instruct",
  "display_name": "Qwen 2.5 7B Instruct (Q4_K_M)",
  "version": "v1.0",
  "file_name": "qwen2.5-7b-instruct-q4_k_m.gguf",
  "sha256": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
  "size_bytes": 4683074272,
  "prompt_template": "chatml",
  "capabilities": ["chat", "function_calling"],
  "aliases": [],
  "profile": "aligned",
  "license": {
    "spdx": "Apache-2.0",
    "title": "Apache License 2.0",
    "url": "https://huggingface.co/Qwen/Qwen2.5-7B-Instruct/blob/main/LICENSE",
    "sha256": "8a0a8fb...",
    "requires_attribution": true
  },
  "source": {
    "kind": "huggingface",
    "url": "https://huggingface.co/Qwen/Qwen2.5-7B-Instruct-GGUF/resolve/main/qwen2.5-7b-instruct-q4_k_m.gguf"
  },
  "auth": null
}
```

### Field reference

| Field                   | Meaning                                                                                                                                                   |
| ----------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `id`                    | Stable unique identifier. Used in URLs, CLI commands, license acceptance records. Renaming requires an `aliases` entry, not editing this field.           |
| `family_id`             | Grouping key the picker uses to collapse quant variants under one family name. Not unique.                                                                 |
| `display_name`          | User-facing name shown in the picker. Free-form.                                                                                                          |
| `version`               | Upstream artifact version string. Informational; not parsed as semver.                                                                                    |
| `file_name`             | Filename on disk under `models/`. SHA256-keyed is fine but human-readable is encouraged for drop-in compatibility.                                          |
| `sha256`                | Mandatory. Verified after download AND on every launch — USB corruption is real.                                                                          |
| `size_bytes`            | Expected file size. Used by the picker before download to show how big the model is, and used by the RAM-fit advisor.                                     |
| `prompt_template`       | One of `chatml`, `llama3`, `mistral`, or any other template name `llama-server` implements natively. Future: inline override field for custom templates.   |
| `capabilities`          | Filterable tags: `chat`, `function_calling`, `json_mode`, `vision`, `code`, `long_context`. The picker uses these for filtering.                           |
| `aliases`               | Legacy `id` values that should still resolve to this entry. Empty for new models. Renaming a model means adding the old id here, not changing `id`.       |
| `profile`               | Content profile. See table below.                                                                                                                         |
| `license`               | SPDX identifier, human-readable title, URL to the license text, SHA256 of that text, and whether attribution is required.                                  |
| `source`                | Where to fetch the artifact from. `kind` is currently `huggingface` or `direct`; `url` is the download URL.                                                |
| `auth`                  | Optional. Present for gated models. `{ "type": "hf_token", "gate_url": "..." }` — the runtime walks the user through token entry or manual gate acceptance. |

### Content profiles

The `profile` field is one of:

| Profile                 | Meaning                                                                                | UI treatment                                              |
| ----------------------- | -------------------------------------------------------------------------------------- | --------------------------------------------------------- |
| `aligned`               | Standard instruct + safety training (Qwen-Instruct, Llama-Instruct, Mistral-Instruct). | Default. No warning.                                      |
| `minimally-aligned`     | Instruct-tuned, refusal training reduced or removed (Dolphin, Hermes, Nous).           | One-time confirmation per model.                          |
| `base`                  | Pretrained foundation, no instruct tuning. Will continue anything.                     | One-time confirmation + custom system prompt required.    |
| `code`                  | Code-specialized (Qwen Coder, DeepSeek Coder).                                         | No warning.                                               |
| `vision`                | Multimodal. Reserved.                                                                  | Future; the runtime currently rejects vision models.      |
| `community-unverified`  | Drop-in `.gguf` files. Not catalog entries. Auto-applied by the runtime.               | Persistent badge in the picker.                           |

The `community-unverified` profile is never set in catalog JSON — it's
applied by the runtime to `.gguf` files dropped into `models/` that
don't match any catalog `id`. License handling for these is the user's
responsibility.

### License records

The `license` block records the exact license text the maintainer
reviewed. When a user accepts a license, the runtime stores
`(model_id, license_sha256, accepted_at, host_at_accept)` in
`.usbuddy/license-acceptance.jsonl`. If the upstream license changes
later, `license_sha256` changes, and the runtime re-prompts on next
launch. This is how we keep license acceptance honest across model
updates.

## Advisory entries

Advisories are how the maintainer communicates security or
compatibility issues to users who already have a model or runtime
installed.

```json
{
  "id": "USB-2026-001",
  "severity": "high",
  "summary": "Llama 3.1 8B Q4_K_M is affected by prompt-injection bypass CVE-2026-XXXX",
  "recommended_action": "Upgrade to the Q5_K_M variant or migrate to a different family.",
  "affects": {
    "models": ["llama-3.1-8b-instruct-q4_k_m"],
    "runtime_versions": [],
    "llama_server": []
  }
}
```

| Field                           | Meaning                                                                            |
| ------------------------------- | ---------------------------------------------------------------------------------- |
| `id`                            | Stable advisory identifier. Format is up to the maintainer; `USB-YYYY-NNN` is fine. |
| `severity`                      | `info`, `low`, `medium`, `high`, `critical`. Used for sort and icon tinting.        |
| `summary`                       | One-sentence user-facing description.                                              |
| `recommended_action`            | What the user should do. Plain prose.                                              |
| `affects.models`                | Model IDs the advisory applies to.                                                 |
| `affects.runtime_versions`      | Semver versions or ranges of the USBuddy runtime affected.                         |
| `affects.llama_server`          | Upstream CVE identifiers in `llama.cpp` if relevant.                               |

The runtime surfaces advisories at launch, filtered against what's
actually installed. They're informational only — USBuddy never
auto-deletes models or downgrades runtimes based on an advisory.
"Dismiss" persists to `.usbuddy/advisories-seen.json`.

## Compatibility rules

A catalog is loadable if and only if all of the following hold:

- `schema` equals a known version.
- `runtime.min ≤ current_runtime ≤ runtime.max` as semver.
- No two models share an `id`, and no alias collides with another
  model's `id` or alias.

`family_id` is a presentation key only. Two models can share it. Storage
remains flat.

## Trust model

The catalog committed to this repo is the default trust root. GitHub
authenticates transport and authorship; git history is the audit log.
There is no separate Sigstore / cosign infrastructure — the SHA256 per
model entry is integrity, the repo is authenticity.

Users can add custom catalog URLs (their own mirror, an org's internal
list, a community catalog). Each is a separate trust decision and is
visibly labeled as such in the picker. Drop-in `.gguf` files are not
catalog entries; they're discovered locally and tagged
`community-unverified`.

## Maintaining the official catalog

The shipped `fixtures/catalog/official.catalog.json` is regenerated from
`fixtures/catalog/seed.toml` by the `xtask catalog-fetch` tool, which
fetches each entry's SHA256 and size from Hugging Face's LFS pointer
endpoint. **No model bytes are downloaded** during catalog regeneration
— it's metadata only.

```sh
cargo run -p xtask -- catalog-fetch
HF_TOKEN=hf_xxx cargo run -p xtask -- catalog-fetch   # gated models
```
