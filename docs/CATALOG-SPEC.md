# USBuddy Catalog Specification

## Schema

Catalog files use `schema: "usbuddy.catalog/v1"` at the root. Unsupported schema versions must be rejected before any model is shown or installed.

## Root object

Required fields:

- `schema`
- `runtime.min`
- `runtime.max`
- `models[]`
- `advisories[]`

Optional fields:

- `generated_at`
- `source`

## Model entries

Each downloadable artifact is one flat entry with these core fields:

- `id`: stable unique identifier.
- `family_id`: grouping key used by pickers.
- `display_name`: user-facing name.
- `version`: upstream artifact version string.
- `file_name`: preferred stored filename.
- `sha256`: mandatory integrity hash.
- `size_bytes`: expected size.
- `prompt_template`: named prompt template reference.
- `capabilities[]`: filterable capability tags.
- `aliases[]`: legacy names accepted for backwards compatibility.
- `profile`: one of `aligned`, `minimally-aligned`, `base`, `code`, `vision`, `community-unverified`.
- `license`: SPDX identifier, title, URL, full text hash, and whether attribution is required.
- `source`: official or custom catalog provenance metadata.
- `auth`: optional gated-model requirements.

## Advisory entries

Advisories live in `advisories[]` and contain:

- `id`
- `severity`
- `summary`
- `recommended_action`
- `affects.models[]`
- `affects.runtime_versions[]`
- `affects.llama_server[]`

Advisories are informational and never force deletion or downgrade.

## Compatibility rules

- Runtime compatibility is modeled as a semver range using `runtime.min` and `runtime.max`.
- Catalogs outside the supported runtime range fail closed.
- Aliases must not collide with another model's `id` or alias.
- `family_id` is a presentation key only; storage remains flat.

## Trust model

- The repository copy of `catalog.json` is the default trust root.
- Additional user-supplied catalog URLs are separate trust decisions and remain visibly distinct.
- Drop-in `.gguf` files are not catalog entries; they are discovered locally and labeled `community-unverified`.

## Prompt templates

Named templates reference the model-facing prompt syntax supported by llama-server, such as `chatml`, `llama3`, or `mistral`. Runtime code may add an inline override field later for truly custom prompt formats without breaking v1 readers.

## v0.1.0 implementation boundary

v0.1.0 validates the schema, compatibility range, alias uniqueness, and required hashes. It does not yet implement remote catalog merging policies beyond loading one official catalog plus optional separately-trusted custom catalogs.
