use std::{fs, path::PathBuf};

use anyhow::Result;
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
    /// Run a WASM analyzer and update project state plus target catalogs.
    Extract {
        #[arg(long)]
        plugin: PathBuf,
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
        Command::Extract {
            plugin,
            path,
            source,
        } => run_plugin(&plugin, &path, &source).await?,
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

async fn run_plugin(plugin_path: &PathBuf, root: &PathBuf, source_path: &PathBuf) -> Result<()> {
    let source = fs::read_to_string(source_path)?;
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
    let document = tfm::plugin::types::Document {
        path: source_path.display().to_string(),
        language: "rust".into(),
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
                    path: source_path.clone(),
                    line: occurrence.range.start.line,
                    column: occurrence.range.start.column,
                    symbol: occurrence.symbol,
                    anchor: Some(occurrence.anchor),
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
