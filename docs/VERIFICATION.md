# Verification

USBuddy verifies managed downloads using SHA256 hashes published in trusted metadata:

- Runtime artifacts are checked against the release manifest.
- Catalog model entries require `sha256` and are verified after download.
- Catalog compatibility is validated before models are shown.
- Pointer state such as `current.json` is updated atomically to avoid half-applied installs.

Future releases will add signed attestations and generated SBOMs to accompany GitHub Releases assets.
