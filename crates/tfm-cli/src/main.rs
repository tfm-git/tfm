use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use wasmtime::{
    Config, Engine, Store,
    component::{Component, Linker},
};
use wasmtime_wasi::{ResourceTable, WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

wasmtime::component::bindgen!({
    path: "../../wit",
    world: "analyzer",
});

#[derive(Debug, Parser)]
#[command(version, about = "Git-native AI localization tooling")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Create a target-only locale layout. English source text remains in code.
    Init {
        #[arg(long, value_name = "BCP47", required = true)]
        locale: Vec<String>,
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    /// Validate TFM state and target locale catalogs without modifying files.
    Check {
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    /// Run the matching local WASM analyzer and update project state plus target catalogs.
    Extract {
        #[arg(long, default_value = ".")]
        path: PathBuf,
        source: PathBuf,
    },
    /// Print a read-only JSON plan for translations that are still missing.
    TranslatePlan {
        #[arg(long, value_name = "BCP47")]
        locale: Vec<String>,
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    /// Apply a validated JSON response to a translation plan.
    ApplyTranslations {
        #[arg(long)]
        response: PathBuf,
        #[arg(default_value = ".")]
        path: PathBuf,
    },
}

struct PluginState {
    ctx: WasiCtx,
    table: ResourceTable,
}

impl WasiView for PluginState {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.ctx,
            table: &mut self.table,
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Init { locale, path } => {
            tfm_core::init_project(&path, &locale)?;
            println!("initialized TFM project at {}", path.display());
        }
        Command::Check { path } => {
            let report = tfm_core::check_project(&path)?;
            println!(
                "valid: {} messages, {} target catalogs",
                report.message_count, report.catalog_count
            );
        }
        Command::Extract { path, source } => run_plugin(&path, &source).await?,
        Command::TranslatePlan { locale, path } => {
            let plan = tfm_core::translation_plan(&path, &locale)?;
            println!("{}", serde_json::to_string_pretty(&plan)?);
        }
        Command::ApplyTranslations { response, path } => {
            let response: tfm_core::TranslationResponse =
                serde_json::from_str(&fs::read_to_string(response)?)?;
            let report = tfm_core::apply_translation_response(&path, &response)?;
            println!(
                "applied {} translations, {} already applied",
                report.applied, report.already_applied
            );
        }
    }
    Ok(())
}

async fn run_plugin(root: &PathBuf, source_path: &PathBuf) -> Result<()> {
    let source = fs::read_to_string(source_path)?;
    let language = language_for_path(source_path)?;
    let plugin_path = discover_plugin(root, &language).await?;
    run_plugin_at(&plugin_path, root, source_path, source, language).await
}

async fn discover_plugin(root: &Path, language: &str) -> Result<PathBuf> {
    let plugin_dir = root.join(tfm_core::PLUGIN_DIR);
    let paths = local_wasm_plugins(&plugin_dir)?;
    if paths.is_empty() {
        bail!(
            "no local WASM plugins in {}; copy a component there",
            plugin_dir.display()
        );
    }

    let mut matches = Vec::new();
    for path in paths {
        let manifest = plugin_manifest(&path)
            .await
            .with_context(|| format!("read plugin manifest from {}", path.display()))?;
        if manifest
            .languages
            .iter()
            .any(|supported| supported == language)
        {
            matches.push(path);
        }
    }
    match matches.as_slice() {
        [path] => Ok(path.clone()),
        [] => bail!(
            "no plugin in {} supports `{language}`",
            plugin_dir.display()
        ),
        _ => bail!(
            "multiple plugins in {} support `{language}`: {}",
            plugin_dir.display(),
            matches
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn local_wasm_plugins(plugin_dir: &Path) -> Result<Vec<PathBuf>> {
    let mut paths = fs::read_dir(plugin_dir)
        .with_context(|| format!("read local plugin directory {}", plugin_dir.display()))?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .extension()
                    .is_some_and(|extension| extension == "wasm")
        })
        .collect::<Vec<_>>();
    paths.sort();
    Ok(paths)
}

async fn plugin_manifest(plugin_path: &Path) -> Result<tfm::plugin::types::Manifest> {
    let mut config = Config::new();
    config.wasm_component_model(true);
    let engine = Engine::new(&config)?;
    let component = Component::from_file(&engine, plugin_path)?;
    let mut linker = Linker::new(&engine);
    wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
    wasmtime_wasi::p3::add_to_linker(&mut linker)?;
    let mut store = Store::new(
        &engine,
        PluginState {
            ctx: WasiCtxBuilder::new().build(),
            table: ResourceTable::new(),
        },
    );
    let bindings = Analyzer::instantiate_async(&mut store, &component, &linker).await?;
    let manifest = store
        .run_concurrent(async |accessor| bindings.call_manifest(accessor).await)
        .await??;
    Ok(manifest)
}

async fn run_plugin_at(
    plugin_path: &Path,
    root: &Path,
    source_path: &Path,
    source: String,
    language: String,
) -> Result<()> {
    let mut config = Config::new();
    config.wasm_component_model(true);
    let engine = Engine::new(&config)?;
    let component = Component::from_file(&engine, plugin_path)?;
    let mut linker = Linker::new(&engine);
    wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
    wasmtime_wasi::p3::add_to_linker(&mut linker)?;
    let mut store = Store::new(
        &engine,
        PluginState {
            ctx: WasiCtxBuilder::new().build(),
            table: ResourceTable::new(),
        },
    );
    let bindings = Analyzer::instantiate_async(&mut store, &component, &linker).await?;
    let manifest = store
        .run_concurrent(async |accessor| bindings.call_manifest(accessor).await)
        .await??;
    if !manifest
        .languages
        .iter()
        .any(|supported| supported == &language)
    {
        bail!(
            "plugin {} does not support `{language}`; it declares: {}",
            manifest.name,
            manifest.languages.join(", ")
        );
    }
    let document = tfm::plugin::types::Document {
        path: source_path.display().to_string(),
        language,
        text: source,
    };
    let analysis = store
        .run_concurrent(async move |accessor| bindings.call_analyze(accessor, document).await)
        .await??
        .map_err(anyhow::Error::msg)?;
    let extracted = analysis
        .messages
        .into_iter()
        .map(|message| tfm_core::ExtractedMessage {
            source: message.source,
            occurrences: message
                .occurrences
                .into_iter()
                .map(|occurrence| tfm_core::Occurrence {
                    path: source_path.to_path_buf(),
                    line: occurrence.range.start.line,
                    column: occurrence.range.start.column,
                    symbol: occurrence.symbol,
                    anchor: Some(occurrence.anchor),
                    context_hints: occurrence
                        .context_hints
                        .into_iter()
                        .map(|hint| tfm_core::ContextHint {
                            kind: match hint.kind {
                                tfm::plugin::types::ContextKind::Hover => {
                                    tfm_core::ContextKind::Hover
                                }
                            },
                            range: tfm_core::Range {
                                start: tfm_core::Position {
                                    line: hint.range.start.line,
                                    column: hint.range.start.column,
                                },
                                end: tfm_core::Position {
                                    line: hint.range.end.line,
                                    column: hint.range.end.column,
                                },
                            },
                        })
                        .collect(),
                })
                .collect(),
        })
        .collect();
    let report = tfm_core::apply_extraction(root, source_path, extracted)?;
    println!(
        "extracted {} new messages, removed {} stale messages",
        report.added, report.removed
    );
    for diagnostic in analysis.diagnostics {
        eprintln!("plugin: {}", diagnostic.message);
    }
    Ok(())
}

fn language_for_path(path: &Path) -> Result<String> {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("rs") => Ok("rust".into()),
        Some("js" | "jsx") => Ok("javascript".into()),
        Some("ts") => Ok("typescript".into()),
        Some("tsx") => Ok("tsx".into()),
        _ => bail!(
            "cannot infer a TFM language from {}; use a supported source extension",
            path.display()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::language_for_path;
    use std::path::Path;

    #[test]
    fn infers_plugin_languages_from_source_extensions() {
        assert_eq!(language_for_path(Path::new("lib.rs")).unwrap(), "rust");
        assert_eq!(
            language_for_path(Path::new("view.jsx")).unwrap(),
            "javascript"
        );
        assert_eq!(
            language_for_path(Path::new("view.ts")).unwrap(),
            "typescript"
        );
        assert_eq!(language_for_path(Path::new("view.tsx")).unwrap(), "tsx");
    }
}
