//! File-first project model and validation for TFM.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const CONFIG_PATH: &str = ".l10n/config.yml";
pub const STATE_PATH: &str = ".l10n/state.yml";
pub const PLUGIN_DIR: &str = ".l10n/plugins";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub version: u32,
    pub source_locale: String,
    pub required_locales: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct State {
    pub version: u32,
    #[serde(default)]
    pub messages: BTreeMap<String, Message>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Message {
    pub source: String,
    pub source_hash: String,
    #[serde(default)]
    pub occurrences: Vec<Occurrence>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history: Vec<PreviousSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_at: Option<GitProvenance>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitProvenance {
    pub revision: String,
    pub dirty: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreviousSource {
    pub source: String,
    pub source_hash: String,
    pub translations: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_at: Option<GitProvenance>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Occurrence {
    pub path: PathBuf,
    pub line: u32,
    pub column: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub context_hints: Vec<ContextHint>,
}

/// A language-plugin request for read-only semantic context from the host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextHint {
    pub kind: ContextKind,
    pub range: Range,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextKind {
    Hover,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Range {
    pub start: Position,
    pub end: Position,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Position {
    pub line: u32,
    pub column: u32,
}

pub type Catalog = BTreeMap<String, String>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckReport {
    pub message_count: usize,
    pub catalog_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedMessage {
    pub source: String,
    pub occurrences: Vec<Occurrence>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractionReport {
    pub added: usize,
    pub removed: usize,
}

/// A read-only hand-off item for a translation workflow or an LLM.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TranslationTask {
    pub source: String,
    pub source_hash: String,
    pub locale: String,
    pub occurrences: Vec<Occurrence>,
    pub history: Vec<PreviousSource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed_at: Option<GitProvenance>,
}

/// A deterministic, file-derived set of missing translations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TranslationPlan {
    pub version: u32,
    /// The exact JSON Schema an external translator must use for its response.
    pub response_schema: serde_json::Value,
    pub tasks: Vec<TranslationTask>,
}

/// A translation proposed by an external workflow for a task in a translation plan.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranslationResponseItem {
    pub source: String,
    pub source_hash: String,
    pub locale: String,
    pub translation: String,
}

/// A versioned, machine-readable response to a translation plan.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranslationResponse {
    pub version: u32,
    pub translations: Vec<TranslationResponseItem>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyTranslationsReport {
    pub applied: usize,
    pub already_applied: usize,
}

/// Return the versioned contract for [`TranslationResponse`].
///
/// This is embedded in each translation plan so an LLM or editor integration
/// can validate its response without knowing TFM's Rust types.
pub fn translation_response_schema() -> serde_json::Value {
    serde_json::from_str(include_str!(
        "../../../schemas/translation-response.schema.json"
    ))
    .expect("the bundled translation response schema must be valid JSON")
}

pub fn init_project(root: &Path, locales: &[String]) -> Result<()> {
    validate_requested_locales(locales)?;

    let config_path = root.join(CONFIG_PATH);
    if config_path.exists() {
        bail!(
            "{} already exists; refusing to overwrite a TFM project",
            config_path.display()
        );
    }

    fs::create_dir_all(config_path.parent().expect("config has a parent"))?;
    fs::create_dir_all(root.join(PLUGIN_DIR))?;
    fs::create_dir_all(root.join("locales"))?;

    let config = Config {
        version: 1,
        source_locale: "en".into(),
        required_locales: locales.to_vec(),
    };
    let state = State {
        version: 1,
        messages: BTreeMap::new(),
    };

    write_yaml(&config_path, &config)?;
    write_yaml(&root.join(STATE_PATH), &state)?;
    for locale in locales {
        write_yaml(&catalog_path(root, locale), &Catalog::new())?;
    }
    Ok(())
}

pub fn check_project(root: &Path) -> Result<CheckReport> {
    let config: Config = read_yaml(&root.join(CONFIG_PATH))?;
    if config.version != 1 {
        bail!("unsupported config schema version {}", config.version);
    }
    if config.source_locale != "en" {
        bail!("source_locale must be `en`; English source text lives in code, not a catalog");
    }
    validate_requested_locales(&config.required_locales)?;

    let state: State = read_yaml(&root.join(STATE_PATH))?;
    if state.version != 1 {
        bail!("unsupported state schema version {}", state.version);
    }

    let english_catalog = catalog_path(root, "en");
    if english_catalog.exists() {
        bail!(
            "{} must not exist; English is the source in code",
            english_catalog.display()
        );
    }

    let known_messages: BTreeSet<_> = state.messages.keys().collect();
    for locale in &config.required_locales {
        let catalog: Catalog = read_yaml(&catalog_path(root, locale))?;
        for key in catalog.keys() {
            if !known_messages.contains(key) {
                bail!(
                    "locales/{locale}.yml contains `{key}`, which is absent from .l10n/state.yml"
                );
            }
        }
        for (key, translation) in &catalog {
            if translation.trim().is_empty() {
                bail!("locales/{locale}.yml is missing a translation for `{key}`");
            }
        }
    }

    Ok(CheckReport {
        message_count: state.messages.len(),
        catalog_count: config.required_locales.len(),
    })
}

/// Return missing translations with source locations and prior phrasing context, without writes.
pub fn translation_plan(root: &Path, requested_locales: &[String]) -> Result<TranslationPlan> {
    let config: Config = read_yaml(&root.join(CONFIG_PATH))?;
    if config.version != 1 {
        bail!("unsupported config schema version {}", config.version);
    }
    validate_requested_locales(&config.required_locales)?;

    let locales = if requested_locales.is_empty() {
        config.required_locales.clone()
    } else {
        requested_locales.to_vec()
    };
    for locale in &locales {
        if !config.required_locales.contains(locale) {
            bail!("locale `{locale}` is not configured for this project");
        }
    }

    let state: State = read_yaml(&root.join(STATE_PATH))?;
    if state.version != 1 {
        bail!("unsupported state schema version {}", state.version);
    }

    let mut tasks = Vec::new();
    for locale in locales {
        let catalog: Catalog = read_yaml(&catalog_path(root, &locale))?;
        for message in state.messages.values() {
            if catalog
                .get(&message.source)
                .is_none_or(|translation| translation.trim().is_empty())
            {
                tasks.push(TranslationTask {
                    source: message.source.clone(),
                    source_hash: message.source_hash.clone(),
                    locale: locale.clone(),
                    occurrences: message.occurrences.clone(),
                    history: message.history.clone(),
                    observed_at: message.observed_at.clone(),
                });
            }
        }
    }

    Ok(TranslationPlan {
        version: 1,
        response_schema: translation_response_schema(),
        tasks,
    })
}

/// Validate an entire translation response before writing changed target catalogs.
pub fn apply_translation_response(
    root: &Path,
    response: &TranslationResponse,
) -> Result<ApplyTranslationsReport> {
    if response.version != 1 {
        bail!(
            "unsupported translation response version {}",
            response.version
        );
    }

    let config: Config = read_yaml(&root.join(CONFIG_PATH))?;
    let state: State = read_yaml(&root.join(STATE_PATH))?;
    let mut catalogs: BTreeMap<String, Catalog> = BTreeMap::new();
    for locale in &config.required_locales {
        catalogs.insert(locale.clone(), read_yaml(&catalog_path(root, locale))?);
    }

    let mut seen = BTreeSet::new();
    let mut changed_locales = BTreeSet::new();
    let mut applied = 0;
    let mut already_applied = 0;
    for item in &response.translations {
        if item.translation.trim().is_empty() {
            bail!(
                "translation for `{}` ({}) must not be empty",
                item.source,
                item.locale
            );
        }
        if !config.required_locales.contains(&item.locale) {
            bail!(
                "locale `{}` is not configured for this project",
                item.locale
            );
        }
        if !seen.insert((&item.locale, &item.source)) {
            bail!(
                "translation response has a duplicate entry for `{}` ({})",
                item.source,
                item.locale
            );
        }
        let message = state
            .messages
            .get(&item.source)
            .with_context(|| format!("source `{}` is no longer extracted", item.source))?;
        if message.source_hash != item.source_hash || hex_sha256(&item.source) != item.source_hash {
            bail!(
                "source `{}` changed since this translation response was created; request a new plan",
                item.source
            );
        }
        let catalog = catalogs
            .get_mut(&item.locale)
            .expect("configured catalogs are loaded");
        match catalog.get(&item.source) {
            Some(existing) if existing == &item.translation => already_applied += 1,
            Some(existing) if !existing.trim().is_empty() => bail!(
                "translation for `{}` ({}) already differs; refusing to overwrite it",
                item.source,
                item.locale
            ),
            Some(_) => {
                catalog.insert(item.source.clone(), item.translation.clone());
                changed_locales.insert(item.locale.clone());
                applied += 1;
            }
            None => bail!(
                "locales/{}.yml is missing source `{}`; run extraction before applying translations",
                item.locale,
                item.source
            ),
        }
    }

    for locale in changed_locales {
        let catalog = catalogs
            .get(&locale)
            .expect("changed locale is one of the loaded catalogs");
        write_yaml(&catalog_path(root, &locale), catalog)?;
    }
    Ok(ApplyTranslationsReport {
        applied,
        already_applied,
    })
}

/// Persist facts returned by a plugin and add untranslated entries to each target catalog.
pub fn apply_extraction(
    root: &Path,
    scanned_path: &Path,
    extracted: Vec<ExtractedMessage>,
) -> Result<ExtractionReport> {
    let config: Config = read_yaml(&root.join(CONFIG_PATH))?;
    let mut state: State = read_yaml(&root.join(STATE_PATH))?;
    let observed_at = git_provenance(root);
    let mut catalogs: BTreeMap<String, Catalog> = BTreeMap::new();
    for locale in &config.required_locales {
        catalogs.insert(locale.clone(), read_yaml(&catalog_path(root, locale))?);
    }
    let prior_by_anchor: BTreeMap<_, _> = state
        .messages
        .iter()
        .flat_map(|(source, message)| {
            message.occurrences.iter().filter_map(move |occurrence| {
                (occurrence.path == scanned_path)
                    .then(|| {
                        occurrence
                            .anchor
                            .clone()
                            .map(|anchor| (anchor, source.clone()))
                    })
                    .flatten()
            })
        })
        .collect();
    let scanned_paths = BTreeSet::from([scanned_path.to_path_buf()]);
    for message in state.messages.values_mut() {
        message
            .occurrences
            .retain(|occurrence| !scanned_paths.contains(&occurrence.path));
    }
    let stale: Vec<_> = state
        .messages
        .iter()
        .filter(|(_, message)| message.occurrences.is_empty())
        .map(|(source, _)| source.clone())
        .collect();
    for source in &stale {
        state.messages.remove(source);
    }
    let mut added = 0;
    for message in extracted {
        let source_hash = hex_sha256(&message.source);
        let mut history = state
            .messages
            .get(&message.source)
            .map(|existing| existing.history.clone())
            .unwrap_or_default();
        for anchor in message
            .occurrences
            .iter()
            .filter_map(|occurrence| occurrence.anchor.as_ref())
        {
            if let Some(previous_source) = prior_by_anchor.get(anchor) {
                if previous_source != &message.source
                    && !history.iter().any(|entry| entry.source == *previous_source)
                {
                    let translations = catalogs
                        .iter()
                        .filter_map(|(locale, catalog)| {
                            catalog
                                .get(previous_source)
                                .map(|value| (locale.clone(), value.clone()))
                        })
                        .collect();
                    history.push(PreviousSource {
                        source: previous_source.clone(),
                        source_hash: hex_sha256(previous_source),
                        translations,
                        observed_at: state
                            .messages
                            .get(previous_source)
                            .and_then(|message| message.observed_at.clone()),
                    });
                }
            }
        }
        if !state.messages.contains_key(&message.source) {
            added += 1;
        }
        state.messages.insert(
            message.source.clone(),
            Message {
                source: message.source,
                source_hash,
                occurrences: message.occurrences,
                history,
                observed_at: observed_at.clone(),
            },
        );
    }
    write_yaml(&root.join(STATE_PATH), &state)?;
    for locale in &config.required_locales {
        let path = catalog_path(root, locale);
        let mut catalog = catalogs.remove(locale).expect("catalog loaded from config");
        for source in &stale {
            catalog.remove(source);
        }
        for source in state.messages.keys() {
            catalog.entry(source.clone()).or_default();
        }
        write_yaml(&path, &catalog)?;
    }
    Ok(ExtractionReport {
        added,
        removed: stale.len(),
    })
}

fn hex_sha256(source: &str) -> String {
    format!("{:x}", Sha256::digest(source.as_bytes()))
}

fn git_provenance(root: &Path) -> Option<GitProvenance> {
    let repo = gix::discover(root).ok()?;
    let revision = repo.head_id().ok()?.to_string();
    // `status` includes untracked files, matching the former `git status --porcelain` behavior.
    let dirty = repo
        .status(gix::progress::Discard)
        .ok()
        .and_then(|status| status.into_iter(Vec::new()).ok())
        .and_then(|mut entries| entries.next().transpose().ok())
        .is_some();
    Some(GitProvenance { revision, dirty })
}

fn validate_requested_locales(locales: &[String]) -> Result<()> {
    if locales.is_empty() {
        bail!("at least one target locale is required (for example: --locale uk)");
    }
    let mut seen = BTreeSet::new();
    for locale in locales {
        if locale == "en" {
            bail!("`en` is source text in code and must not have locales/en.yml");
        }
        if locale.is_empty()
            || !locale
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || character == '-')
        {
            bail!("invalid locale `{locale}`; use BCP 47-style tags such as uk or pt-BR");
        }
        if !seen.insert(locale) {
            bail!("locale `{locale}` was specified more than once");
        }
    }
    Ok(())
}

fn catalog_path(root: &Path, locale: &str) -> PathBuf {
    root.join("locales").join(format!("{locale}.yml"))
}

fn read_yaml<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    let content = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    serde_yaml::from_str(&content).with_context(|| format!("parse {}", path.display()))
}

fn write_yaml<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let content = serde_yaml::to_string(value).context("serialize YAML")?;
    fs::write(path, content).with_context(|| format!("write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_english_as_a_target_catalog() {
        let error = validate_requested_locales(&["en".into()]).unwrap_err();
        assert!(error.to_string().contains("source text"));
    }

    #[test]
    fn accepts_a_bcp47_style_target_locale() {
        validate_requested_locales(&["uk".into(), "pt-BR".into()]).unwrap();
    }

    #[test]
    fn initializes_a_local_plugin_directory() {
        let root = tempfile::tempdir().unwrap();
        init_project(root.path(), &["uk".into()]).unwrap();

        assert!(root.path().join(PLUGIN_DIR).is_dir());
    }

    #[test]
    fn reads_head_provenance_with_gix() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let provenance = git_provenance(&root).expect("the workspace checkout has a HEAD commit");

        assert_eq!(provenance.revision.len(), 40);
        assert!(
            provenance
                .revision
                .chars()
                .all(|character| character.is_ascii_hexdigit())
        );
    }

    #[test]
    fn translation_plan_only_returns_missing_entries_with_history() {
        let root = tempfile::tempdir().unwrap();
        init_project(root.path(), &["uk".into()]).unwrap();

        let occurrence = Occurrence {
            path: PathBuf::from("src/settings.rs"),
            line: 12,
            column: 5,
            symbol: Some("save".into()),
            anchor: Some("save::t!#1".into()),
            context_hints: vec![ContextHint {
                kind: ContextKind::Hover,
                range: Range {
                    start: Position {
                        line: 12,
                        column: 7,
                    },
                    end: Position {
                        line: 12,
                        column: 11,
                    },
                },
            }],
        };
        let mut state = State::default();
        state.version = 1;
        state.messages.insert(
            "Save changes".into(),
            Message {
                source: "Save changes".into(),
                source_hash: hex_sha256("Save changes"),
                occurrences: vec![occurrence.clone()],
                history: vec![PreviousSource {
                    source: "Save".into(),
                    source_hash: "previous".into(),
                    translations: BTreeMap::from([("uk".into(), "Зберегти".into())]),
                    observed_at: Some(GitProvenance {
                        revision: "a".repeat(40),
                        dirty: false,
                    }),
                }],
                observed_at: Some(GitProvenance {
                    revision: "b".repeat(40),
                    dirty: true,
                }),
            },
        );
        state.messages.insert(
            "Cancel".into(),
            Message {
                source: "Cancel".into(),
                source_hash: hex_sha256("Cancel"),
                occurrences: vec![],
                history: vec![],
                observed_at: None,
            },
        );
        write_yaml(&root.path().join(STATE_PATH), &state).unwrap();
        let catalog: Catalog = BTreeMap::from([
            ("Save changes".into(), String::new()),
            ("Cancel".into(), "Скасувати".into()),
        ]);
        write_yaml(&catalog_path(root.path(), "uk"), &catalog).unwrap();
        let state_before = fs::read_to_string(root.path().join(STATE_PATH)).unwrap();
        let catalog_before = fs::read_to_string(catalog_path(root.path(), "uk")).unwrap();

        let plan = translation_plan(root.path(), &["uk".into()]).unwrap();

        assert_eq!(plan.version, 1);
        assert_eq!(
            plan.response_schema["$id"],
            "urn:tfm:translation-response:1"
        );
        assert_eq!(plan.response_schema["properties"]["version"]["const"], 1);
        assert_eq!(plan.tasks.len(), 1);
        assert_eq!(plan.tasks[0].source, "Save changes");
        assert_eq!(plan.tasks[0].source_hash, hex_sha256("Save changes"));
        assert_eq!(plan.tasks[0].locale, "uk");
        assert_eq!(plan.tasks[0].occurrences, vec![occurrence]);
        assert_eq!(plan.tasks[0].history[0].source, "Save");
        assert_eq!(
            plan.tasks[0].history[0].translations.get("uk"),
            Some(&"Зберегти".into())
        );
        assert_eq!(
            fs::read_to_string(root.path().join(STATE_PATH)).unwrap(),
            state_before
        );
        assert_eq!(
            fs::read_to_string(catalog_path(root.path(), "uk")).unwrap(),
            catalog_before
        );
    }

    #[test]
    fn applies_current_responses_but_rejects_stale_ones_without_writing() {
        let root = tempfile::tempdir().unwrap();
        init_project(root.path(), &["uk".into()]).unwrap();
        let source = "Save changes";
        let other_source = "Discard";
        let mut state = State::default();
        state.version = 1;
        state.messages.insert(
            source.into(),
            Message {
                source: source.into(),
                source_hash: hex_sha256(source),
                occurrences: vec![],
                history: vec![],
                observed_at: None,
            },
        );
        state.messages.insert(
            other_source.into(),
            Message {
                source: other_source.into(),
                source_hash: hex_sha256(other_source),
                occurrences: vec![],
                history: vec![],
                observed_at: None,
            },
        );
        write_yaml(&root.path().join(STATE_PATH), &state).unwrap();
        let catalog: Catalog = BTreeMap::from([
            (source.into(), String::new()),
            (other_source.into(), String::new()),
        ]);
        write_yaml(&catalog_path(root.path(), "uk"), &catalog).unwrap();

        let before_stale = fs::read_to_string(catalog_path(root.path(), "uk")).unwrap();
        let stale = TranslationResponse {
            version: 1,
            translations: vec![
                TranslationResponseItem {
                    source: source.into(),
                    source_hash: hex_sha256(source),
                    locale: "uk".into(),
                    translation: "Зберегти зміни".into(),
                },
                TranslationResponseItem {
                    source: other_source.into(),
                    source_hash: hex_sha256("Drop"),
                    locale: "uk".into(),
                    translation: "Відкинути".into(),
                },
            ],
        };
        let error = apply_translation_response(root.path(), &stale).unwrap_err();
        assert!(error.to_string().contains("changed since"));
        assert_eq!(
            fs::read_to_string(catalog_path(root.path(), "uk")).unwrap(),
            before_stale
        );

        let response = TranslationResponse {
            version: 1,
            translations: vec![TranslationResponseItem {
                source: source.into(),
                source_hash: hex_sha256(source),
                locale: "uk".into(),
                translation: "Зберегти зміни".into(),
            }],
        };
        let report = apply_translation_response(root.path(), &response).unwrap();
        assert_eq!(report.applied, 1);
        assert_eq!(report.already_applied, 0);
        let saved: Catalog = read_yaml(&catalog_path(root.path(), "uk")).unwrap();
        assert_eq!(saved.get(source), Some(&"Зберегти зміни".into()));
    }
}
