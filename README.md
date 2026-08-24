# TFM

Git-native, AI-assisted localization tooling. English is the source text in
code; TFM creates only target-locale catalogs and tracks extraction state in
Git.

```sh
cargo run -p tfm -- init --locale uk --locale pl ./example
cargo run -p tfm -- check ./example
cargo run -p tfm -- extract --path ./example ./example/src/settings.ts
cargo run -p tfm -- extract --all --path ./example
cargo run -p tfm -- lsp-context --path ./example ./example/src/settings.ts
cargo run -p tfm -- implicit-candidates ./example
cargo run -p tfm -- fix --mark ./example
cargo run -p tfm -- translate-plan --locale uk ./example
cargo run -p tfm -- apply-translations --response translations.json ./example
```

`tfm init` produces:

```text
.l10n/config.yml
.l10n/state.yml
.l10n/plugins/
locales/uk.yml
locales/pl.yml
```

There is intentionally no `locales/en.yml`.

Rust applications can use the framework-neutral `tfm-runtime` crate. Load a
target catalog, activate its locale, and import its source-literal macro:

```rust
use tfm_runtime::{activate_locale, load_catalog_file, t};

load_catalog_file("uk", "locales/uk.yml")?;
activate_locale("uk");
let label = t!("Save");
```

`t!` returns the English source text until an active catalog contains a
translation, so a missing target entry remains visible instead of failing at
runtime.

Copy each analyzer WASM Component into `.l10n/plugins/`. `tfm extract` reads
their manifests and selects the one that declares support for the source file's
language; it fails if none or more than one plugin matches.
`tfm extract --all` recursively processes supported source files and skips
`.git`, `.l10n`, `node_modules`, and `target` directories.

`tfm lsp-context` resolves the plugin-owned semantic hints for one extracted
file through a local, read-only LSP session and prints JSON for a later ACP
translation prompt. It uses `rust-analyzer` for Rust and
`typescript-language-server --stdio` for JavaScript, TypeScript and TSX.

`tfm implicit-candidates` prints high-confidence UI strings found without an
explicit runtime marker. After review, `tfm fix --mark` wraps only the matching
plain Rust string literals in `t!(...)`. It refuses files outside the project
root and candidates whose source text changed since extraction; it never marks
raw string literals automatically.

`tfm translate-plan` does not change files. It prints JSON tasks only for
missing translations, including the source occurrence, prior source text and
translation (when available), and the Git provenance captured at extraction.
Each plan also embeds the exact versioned JSON Schema expected by
`apply-translations`; the same contract is checked into
[`schemas/translation-response.schema.json`](schemas/translation-response.schema.json).

`tfm apply-translations` accepts a versioned JSON response containing
`source`, `source_hash`, `locale`, and `translation`. It refuses stale source
text and conflicting existing translations, then writes target catalogs only
after every response entry validates. A response is not applied partially when
any entry is invalid.

For example, an LLM response has this shape (the source hash comes from its
task in the plan):

```json
{
  "version": 1,
  "translations": [
    {
      "source": "Save changes",
      "source_hash": "<hash from the plan>",
      "locale": "uk",
      "translation": "Зберегти зміни"
    }
  ]
}
```

The language extractors will be separate WASM Components governed by the WIT
contract in [`wit/tfm-plugin.wit`](wit/tfm-plugin.wit). The Rust host owns files,
Git, LSP and LLM calls; plugins receive documents, return analysis facts, and may
attach read-only semantic context hints for the host to resolve through LSP.
