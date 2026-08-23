use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use serde::Serialize;
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{ChildStderr, ChildStdin, ChildStdout, Command as TokioCommand},
    time::timeout,
};
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
    /// Resolve plugin-owned read-only semantic hints through a local language server.
    LspContext {
        #[arg(long, default_value = ".")]
        path: PathBuf,
        source: PathBuf,
    },
    /// Print pre-extracted UI strings that still need explicit runtime instrumentation.
    ImplicitCandidates {
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    /// Apply a narrowly scoped source transformation.
    Fix {
        /// Wrap implicit Rust UI strings in t!(...) markers.
        #[arg(long)]
        mark: bool,
        #[arg(default_value = ".")]
        path: PathBuf,
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
        Command::LspContext { path, source } => {
            let report = resolve_lsp_context(&path, &source).await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        Command::ImplicitCandidates { path } => {
            println!(
                "{}",
                serde_json::to_string_pretty(&tfm_core::implicit_candidates(&path)?)?
            );
        }
        Command::Fix { mark, path } => {
            if !mark {
                bail!("choose a fix mode, for example `tfm fix --mark`");
            }
            let changed = mark_implicit_candidates(&path)?;
            println!("marked {changed} implicit UI strings");
        }
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

fn mark_implicit_candidates(root: &Path) -> Result<usize> {
    let root = root.canonicalize()?;
    let mut by_path: BTreeMap<PathBuf, Vec<tfm_core::ImplicitCandidate>> = BTreeMap::new();
    for candidate in tfm_core::implicit_candidates(&root)? {
        let path = candidate.occurrence.path.canonicalize().with_context(|| {
            format!(
                "resolve implicit candidate path {}",
                candidate.occurrence.path.display()
            )
        })?;
        if !path.starts_with(&root) {
            bail!(
                "refusing to mark {} because it is outside project root {}",
                path.display(),
                root.display()
            );
        }
        by_path.entry(path).or_default().push(candidate);
    }

    let mut changed = 0;
    for (path, occurrences) in by_path {
        let source = fs::read_to_string(&path)?;
        let (updated, count) = mark_source_literals(&source, &occurrences)?;
        if count > 0 {
            fs::write(&path, updated)?;
            changed += count;
        }
    }
    Ok(changed)
}

fn mark_source_literals(
    source: &str,
    candidates: &[tfm_core::ImplicitCandidate],
) -> Result<(String, usize)> {
    let mut edits = candidates
        .iter()
        .map(|candidate| {
            let start = offset_for_position(
                source,
                candidate.occurrence.line,
                candidate.occurrence.column,
            )?;
            Ok((start, candidate))
        })
        .collect::<Result<Vec<_>>>()?;
    // Implicit Rust anchors identify a literal span, but the core occurrence only
    // persists its start position. Resolve the token conservatively from there.
    edits.sort_by_key(|(start, _)| std::cmp::Reverse(*start));
    let mut updated = source.to_owned();
    let mut changed = 0;
    let mut previously_marked = None;
    for (start, candidate) in edits {
        if previously_marked == Some(start) {
            continue;
        }
        let end = rust_string_literal_end(&updated[start..]).with_context(|| {
            format!(
                "expected a plain Rust string literal at {}:{}",
                candidate.occurrence.line, candidate.occurrence.column
            )
        })? + start;
        let literal = &updated[start..end];
        let current_source = syn::parse_str::<syn::LitStr>(literal)
            .context("implicit candidate no longer points at a valid Rust string literal")?
            .value();
        if current_source != candidate.source {
            bail!(
                "implicit candidate at {}:{} changed since extraction; run `tfm extract` and review again",
                candidate.occurrence.line,
                candidate.occurrence.column
            );
        }
        updated.replace_range(start..end, &format!("t!({literal})"));
        changed += 1;
        previously_marked = Some(start);
    }
    Ok((updated, changed))
}

fn offset_for_position(source: &str, line: u32, column: u32) -> Result<usize> {
    let line_start = source
        .split_inclusive('\n')
        .take(line.saturating_sub(1) as usize)
        .map(str::len)
        .sum::<usize>();
    let offset = line_start + column as usize;
    if offset > source.len() || !source.is_char_boundary(offset) {
        bail!("invalid source position {line}:{column}");
    }
    Ok(offset)
}

fn rust_string_literal_end(source: &str) -> Option<usize> {
    let bytes = source.as_bytes();
    if bytes.first() != Some(&b'"') {
        return None;
    }
    let mut escaped = false;
    for (index, byte) in bytes.iter().enumerate().skip(1) {
        if *byte == b'"' && !escaped {
            return Some(index + 1);
        }
        escaped = *byte == b'\\' && !escaped;
        if *byte != b'\\' {
            escaped = false;
        }
    }
    None
}

#[derive(Debug, Serialize)]
struct LspContextReport {
    version: u32,
    language: String,
    server: String,
    contexts: Vec<ResolvedContext>,
}

#[derive(Debug, Serialize)]
struct ResolvedContext {
    source: String,
    anchor: Option<String>,
    hint: tfm_core::ContextHint,
    value: Option<serde_json::Value>,
}

async fn resolve_lsp_context(root: &Path, source_path: &Path) -> Result<LspContextReport> {
    let language = language_for_path(source_path)?;
    let (server, args, language_id) = lsp_server(&language)?;
    let requests = tfm_core::context_requests(root, source_path)?;
    if requests.is_empty() {
        bail!(
            "no plugin semantic context hints for {}; run tfm extract after installing an ABI 0.3 plugin",
            source_path.display()
        );
    }

    let source = fs::read_to_string(source_path)?;
    let mut client = LspClient::start(server, args, root).await?;
    client.initialize(root).await?;
    client.did_open(source_path, language_id, &source).await?;

    let mut contexts = Vec::new();
    for request in requests {
        for hint in request.occurrence.context_hints {
            let value = match hint.kind {
                tfm_core::ContextKind::Hover => client.hover(source_path, &hint.range).await?,
            };
            contexts.push(ResolvedContext {
                source: request.source.clone(),
                anchor: request.occurrence.anchor.clone(),
                hint,
                value,
            });
        }
    }
    client.shutdown().await?;
    Ok(LspContextReport {
        version: 1,
        language,
        server: server.into(),
        contexts,
    })
}

fn lsp_server(language: &str) -> Result<(&'static str, &'static [&'static str], &'static str)> {
    match language {
        "rust" => Ok(("rust-analyzer", &[], "rust")),
        "javascript" => Ok(("typescript-language-server", &["--stdio"], "javascript")),
        "typescript" => Ok(("typescript-language-server", &["--stdio"], "typescript")),
        "tsx" => Ok((
            "typescript-language-server",
            &["--stdio"],
            "typescriptreact",
        )),
        _ => bail!("no local LSP server configured for `{language}`"),
    }
}

struct LspClient {
    child: tokio::process::Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    stderr: ChildStderr,
    next_id: u64,
}

impl LspClient {
    async fn start(server: &str, args: &[&str], root: &Path) -> Result<Self> {
        let mut child = TokioCommand::new(server)
            .args(args)
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("start local LSP server `{server}`"))?;
        let stdin = child
            .stdin
            .take()
            .context("LSP server stdin is unavailable")?;
        let stdout = child
            .stdout
            .take()
            .context("LSP server stdout is unavailable")?;
        let stderr = child
            .stderr
            .take()
            .context("LSP server stderr is unavailable")?;
        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            stderr,
            next_id: 1,
        })
    }

    async fn initialize(&mut self, root: &Path) -> Result<()> {
        let root_uri = file_uri(root.canonicalize()?);
        self.request(
            "initialize",
            serde_json::json!({
                "processId": null,
                "rootUri": root_uri,
                "capabilities": {}
            }),
        )
        .await?;
        self.notify("initialized", serde_json::json!({})).await
    }

    async fn did_open(&mut self, path: &Path, language_id: &str, text: &str) -> Result<()> {
        self.notify(
            "textDocument/didOpen",
            serde_json::json!({
                "textDocument": {
                    "uri": file_uri(path.canonicalize()?),
                    "languageId": language_id,
                    "version": 1,
                    "text": text
                }
            }),
        )
        .await
    }

    async fn hover(
        &mut self,
        path: &Path,
        range: &tfm_core::Range,
    ) -> Result<Option<serde_json::Value>> {
        let response = self
            .request(
                "textDocument/hover",
                serde_json::json!({
                    "textDocument": { "uri": file_uri(path.canonicalize()?) },
                    "position": {
                        "line": range.start.line.saturating_sub(1),
                        "character": range.start.column
                    }
                }),
            )
            .await?;
        Ok((!response.is_null()).then_some(response))
    }

    async fn shutdown(&mut self) -> Result<()> {
        self.request("shutdown", serde_json::json!(null)).await?;
        self.notify("exit", serde_json::json!(null)).await?;
        timeout(Duration::from_secs(2), self.child.wait())
            .await
            .context("wait for LSP server shutdown")??;
        Ok(())
    }

    async fn request(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let id = self.next_id;
        self.next_id += 1;
        self.send(serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        }))
        .await?;
        loop {
            let message = timeout(Duration::from_secs(10), self.read_message())
                .await
                .context("LSP request timed out")??;
            if message.get("id") == Some(&serde_json::json!(id)) {
                if let Some(error) = message.get("error") {
                    bail!("LSP `{method}` failed: {error}");
                }
                return Ok(message
                    .get("result")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null));
            }
        }
    }

    async fn notify(&mut self, method: &str, params: serde_json::Value) -> Result<()> {
        self.send(serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params
        }))
        .await
    }

    async fn send(&mut self, message: serde_json::Value) -> Result<()> {
        let body = serde_json::to_vec(&message)?;
        self.stdin
            .write_all(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes())
            .await?;
        self.stdin.write_all(&body).await?;
        self.stdin.flush().await?;
        Ok(())
    }

    async fn read_message(&mut self) -> Result<serde_json::Value> {
        let mut content_length = None;
        loop {
            let mut line = String::new();
            if self.stdout.read_line(&mut line).await? == 0 {
                let status = self.child.wait().await?;
                let mut stderr = String::new();
                self.stderr.read_to_string(&mut stderr).await?;
                bail!("LSP server closed stdout with {status}: {}", stderr.trim());
            }
            let line = line.trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                break;
            }
            if let Some(value) = line.strip_prefix("Content-Length:") {
                content_length = Some(value.trim().parse::<usize>()?);
            }
        }
        let length = content_length.context("LSP response is missing Content-Length")?;
        let mut body = vec![0_u8; length];
        self.stdout.read_exact(&mut body).await?;
        Ok(serde_json::from_slice(&body)?)
    }
}

fn file_uri(path: PathBuf) -> String {
    format!("file://{}", path.to_string_lossy().replace(' ', "%20"))
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
    use super::{language_for_path, mark_source_literals};
    use std::path::{Path, PathBuf};
    use tfm_core::{ImplicitCandidate, Occurrence};

    fn implicit_candidate(source: &str, column: u32) -> ImplicitCandidate {
        ImplicitCandidate {
            source: source.into(),
            occurrence: Occurrence {
                path: PathBuf::from("src/view.rs"),
                line: 1,
                column,
                symbol: None,
                anchor: Some("implicit::gpui".into()),
                context_hints: Vec::new(),
            },
        }
    }

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

    #[test]
    fn marks_each_plain_string_literal_once() {
        let source = "fn view() { ui.child(\"Settings\").label(\"Save\"); }\n";
        let settings = source.find("\"Settings\"").unwrap() as u32;
        let save = source.find("\"Save\"").unwrap() as u32;
        let (updated, changed) = mark_source_literals(
            source,
            &[
                implicit_candidate("Settings", settings),
                implicit_candidate("Save", save),
                implicit_candidate("Settings", settings),
            ],
        )
        .unwrap();

        assert_eq!(changed, 2);
        assert_eq!(
            updated,
            "fn view() { ui.child(t!(\"Settings\")).label(t!(\"Save\")); }\n"
        );
    }

    #[test]
    fn preserves_escaped_quotes_in_a_string_literal() {
        let source = "let label = \"Say \\\"hello\\\"\";\n";
        let (updated, changed) = mark_source_literals(
            source,
            &[implicit_candidate(
                "Say \"hello\"",
                source.find('"').unwrap() as u32,
            )],
        )
        .unwrap();

        assert_eq!(changed, 1);
        assert_eq!(updated, "let label = t!(\"Say \\\"hello\\\"\");\n");
    }

    #[test]
    fn refuses_a_literal_changed_since_extraction() {
        let source = "let label = \"Cancel\";\n";
        let error = mark_source_literals(
            source,
            &[implicit_candidate("Save", source.find('"').unwrap() as u32)],
        )
        .unwrap_err();

        assert!(error.to_string().contains("changed since extraction"));
    }
}
