# Catalog format

The catalog defines the set of models a USBuddy installation considers
authoritative, along with their integrity hashes, license metadata, and
applicable advisories. This document specifies the v1 format. The JSON
schema lives at [`schemas/catalog.schema.json`](../schemas/catalog.schema.json)
and is enforced by `usbuddy-core` at parse time.

## Schema version

Catalogs declare their schema version at the root:

```json
{
  "schema": "usbuddy.catalog/v1",
  ...
}
```

`usbuddy-core` rejects any catalog whose schema string it does not
recognize. There is no fallback parsing. Breaking changes to the format
require a new schema version; backward-compatible additions do not.

`usbuddy.catalog/v1` is the only version defined.

## Root object

```json
{
  "schema": "usbuddy.catalog/v1",
  "runtime": { "min": "0.1.0", "max": "0.1.99" },
  "models": [],
  "advisories": [],
  "generated_at": "2026-06-01T00:00:00Z",
  "source": "https://github.com/skullzarmy/USBuddy"
}
```

| Field          | Required | Description                                                                                                              |
| -------------- | -------- | ------------------------------------------------------------------------------------------------------------------------ |
| `schema`       | yes      | Must equal `usbuddy.catalog/v1`.                                                                                         |
| `runtime.min`  | yes      | Minimum runtime semver that can load this catalog.                                                                       |
| `runtime.max`  | yes      | Maximum runtime semver that can load this catalog.                                                                       |
| `models`       | yes      | Array of model entries. May be empty.                                                                                    |
| `advisories`   | yes      | Array of advisory entries. May be empty.                                                                                 |
| `generated_at` | no       | ISO-8601 timestamp the snapshot was produced.                                                                            |
| `source`       | no       | URL identifying the catalog's origin. Used to distinguish custom catalogs from the official catalog in the UI.           |

A catalog is loadable if and only if `runtime.min ≤ current_runtime ≤
runtime.max` according to semver comparison. Out-of-range catalogs are
rejected.

## Model entries

Each downloadable artifact is one entry. Quant variants of the same
model are separate entries that share a `family_id`.

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
    "sha256": "8a0a8fb1b8c0a8fb1b8c0a8fb1b8c0a8fb1b8c0a8fb1b8c0a8fb1b8c0a8fb1b8",
    "requires_attribution": true
  },
  "source": {
    "kind": "huggingface",
    "url": "https://huggingface.co/Qwen/Qwen2.5-7B-Instruct-GGUF/resolve/main/qwen2.5-7b-instruct-q4_k_m.gguf"
  },
  "auth": null
}
```

| Field             | Description                                                                                                                                             |
| ----------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `id`              | Stable unique identifier. Used in URLs, CLI invocations, and license acceptance records. Must not be reassigned; rename via `aliases`.                  |
| `family_id`       | Grouping key for the picker. Non-unique. Quant variants of one model share this value.                                                                  |
| `display_name`    | User-facing label.                                                                                                                                      |
| `version`         | Upstream artifact version. Informational; not parsed as semver.                                                                                         |
| `file_name`       | Filename under `models/` on the drive.                                                                                                                  |
| `sha256`          | Required. Verified after download and on every launch.                                                                                                  |
| `size_bytes`      | Expected file size. Used in the picker and by the RAM-fit advisor.                                                                                      |
| `prompt_template` | Named template `llama-server` implements (`chatml`, `llama3`, `mistral`, etc.).                                                                         |
| `capabilities`    | Filterable tags: `chat`, `function_calling`, `json_mode`, `vision`, `code`, `long_context`.                                                             |
| `aliases`         | Prior `id` values that should still resolve to this entry. Used when renaming.                                                                          |
| `profile`         | Content profile. See below.                                                                                                                             |
| `license`         | License metadata. See below.                                                                                                                            |
| `source`          | Artifact origin. `kind` is `huggingface` or `direct`; `url` is the download URL.                                                                        |
| `auth`            | Required for gated models. `{ "type": "hf_token", "gate_url": "..." }` triggers token entry or manual gate acceptance.                                   |

### Content profiles

| Profile                 | Definition                                                                          | UI treatment                                          |
| ----------------------- | ----------------------------------------------------------------------------------- | ----------------------------------------------------- |
| `aligned`               | Standard instruct training plus safety training.                                    | Default. No confirmation.                             |
| `minimally-aligned`     | Instruct-tuned with refusal training reduced or removed.                            | One-time confirmation per model.                      |
| `base`                  | Pretrained foundation. No instruct tuning. Completes arbitrary input.               | One-time confirmation. Custom system prompt required. |
| `code`                  | Code-specialized.                                                                   | Default. No confirmation.                             |
| `vision`                | Multimodal. Reserved.                                                               | Currently rejected by the runtime.                    |
| `community-unverified`  | Reserved for runtime-discovered drop-in `.gguf` files. Not valid in catalog JSON.   | Persistent badge in the picker.                       |

The `community-unverified` profile is applied by the runtime to any
`.gguf` file in `models/` that does not match a catalog `id`. License
handling for these files is the user's responsibility.

### License metadata

| Field                  | Description                                                                                              |
| ---------------------- | -------------------------------------------------------------------------------------------------------- |
| `spdx`                 | SPDX identifier (e.g. `Apache-2.0`, `MIT`, `Llama-3.1-Community`).                                       |
| `title`                | Human-readable license name.                                                                             |
| `url`                  | URL to the license text.                                                                                 |
| `sha256`               | Hash of the license text the maintainer reviewed. Stored in acceptance records; re-prompts on change.    |
| `requires_attribution` | Whether the license requires attribution in the Credits screen.                                          |

Acceptance is recorded in `.usbuddy/license-acceptance.jsonl` as
`(model_id, license_sha256, accepted_at, host_at_accept)`. When the
upstream license text changes, the recorded `license_sha256` no longer
matches and the runtime re-prompts.

## Advisory entries

```json
{
  "id": "USB-2026-001",
  "severity": "high",
  "summary": "Llama 3.1 8B Q4_K_M affected by prompt-injection bypass.",
  "recommended_action": "Migrate to the Q5_K_M variant or a different model family.",
  "affects": {
    "models": ["llama-3.1-8b-instruct-q4_k_m"],
    "runtime_versions": [],
    "llama_server": []
  }
}
```

| Field                       | Description                                                                              |
| --------------------------- | ---------------------------------------------------------------------------------------- |
| `id`                        | Stable advisory identifier.                                                              |
| `severity`                  | One of `info`, `low`, `medium`, `high`, `critical`.                                      |
| `summary`                   | One-sentence user-facing description.                                                    |
| `recommended_action`        | Action the user should take. Free-form prose.                                            |
| `affects.models`            | Model IDs the advisory applies to.                                                       |
| `affects.runtime_versions`  | Semver versions or ranges of the USBuddy runtime the advisory applies to.                |
| `affects.llama_server`      | Upstream CVE identifiers in `llama.cpp` if the advisory tracks one.                      |

Advisories are surfaced at launch, filtered against the installed
runtime version and discovered models. They are informational. The
runtime does not delete models or downgrade itself based on advisory
content. Dismissals persist in `.usbuddy/advisories-seen.json`.

## Identifier uniqueness

Within a catalog:

- `id` values must be globally unique.
- Each `alias` must not collide with another model's `id` or alias.
- `family_id` is non-unique and is a presentation key only.

## Trust model

The catalog committed to this repository is the default trust root.
Transport and authorship are authenticated by GitHub; the git history
is the audit log. Integrity of model artifacts is established by
SHA256 per entry.

Users may configure additional catalog URLs. Each is a separate trust
decision. The UI labels custom-catalog entries distinctly so they are
not confused with official-catalog entries.

Drop-in `.gguf` files are discovered by the runtime and labeled
`community-unverified`. They are not catalog entries and have no
integrity contract.

## Catalog generation

The official `fixtures/catalog/official.catalog.json` is generated from
`fixtures/catalog/seed.toml` by the `xtask catalog-fetch` tool, which
fetches SHA256 and size from Hugging Face's LFS pointer API. No model
weights are downloaded during catalog generation.

```sh
cargo run -p xtask -- catalog-fetch
HF_TOKEN=hf_xxx cargo run -p xtask -- catalog-fetch
```
