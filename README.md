# TFM

Git-native, AI-assisted localization tooling. English is the source text in
code; TFM creates only target-locale catalogs and tracks extraction state in
Git.

```sh
cargo run -p tfm -- init --locale uk --locale pl ./example
cargo run -p tfm -- check ./example
cargo run -p tfm -- translate-plan --locale uk ./example
```

`tfm init` produces:

```text
.l10n/config.yml
.l10n/state.yml
locales/uk.yml
locales/pl.yml
```

There is intentionally no `locales/en.yml`.

`tfm translate-plan` does not change files. It prints JSON tasks only for
missing translations, including the source occurrence, prior source text and
translation (when available), and the Git provenance captured at extraction.

The language extractors will be separate WASM Components governed by the WIT
contract in [`wit/tfm-plugin.wit`](wit/tfm-plugin.wit). The Rust host owns files,
Git, LSP and LLM calls; plugins receive documents and return analysis facts.
