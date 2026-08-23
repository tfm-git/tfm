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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreviousSource {
    pub source: String,
    pub source_hash: String,
    pub translations: BTreeMap<String, String>,
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

/// Persist facts returned by a plugin and add untranslated entries to each target catalog.
pub fn apply_extraction(
    root: &Path,
    scanned_path: &Path,
    extracted: Vec<ExtractedMessage>,
) -> Result<ExtractionReport> {
    let config: Config = read_yaml(&root.join(CONFIG_PATH))?;
    let mut state: State = read_yaml(&root.join(STATE_PATH))?;
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
}
