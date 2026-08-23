//! File-first project model and validation for TFM.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Occurrence {
    pub path: PathBuf,
    pub line: u32,
    pub column: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
}

pub type Catalog = BTreeMap<String, String>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckReport {
    pub message_count: usize,
    pub catalog_count: usize,
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
    }

    Ok(CheckReport {
        message_count: state.messages.len(),
        catalog_count: config.required_locales.len(),
    })
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
