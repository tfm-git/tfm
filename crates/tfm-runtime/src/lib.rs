//! Runtime support for source-text TFM translations.

use std::{
    collections::BTreeMap,
    error::Error as StdError,
    fmt, fs,
    path::{Path, PathBuf},
    sync::{OnceLock, RwLock},
};

/// A target-locale catalog, keyed by the English source text in code.
pub type Catalog = BTreeMap<String, String>;

#[derive(Default)]
struct RuntimeState {
    active_locale: Option<String>,
    catalogs: BTreeMap<String, Catalog>,
}

fn state() -> &'static RwLock<RuntimeState> {
    static STATE: OnceLock<RwLock<RuntimeState>> = OnceLock::new();
    STATE.get_or_init(|| RwLock::new(RuntimeState::default()))
}

/// Replace the catalog for a locale. The active locale is unchanged.
pub fn replace_catalog(locale: impl Into<String>, catalog: Catalog) {
    state()
        .write()
        .expect("TFM runtime state lock poisoned")
        .catalogs
        .insert(locale.into(), catalog);
}

/// Parse and install one target-locale YAML catalog.
pub fn load_catalog_yaml(locale: impl Into<String>, yaml: &str) -> Result<(), CatalogError> {
    let catalog = serde_yaml::from_str(yaml).map_err(CatalogError::Parse)?;
    replace_catalog(locale, catalog);
    Ok(())
}

/// Read and install one target-locale YAML catalog from the local filesystem.
pub fn load_catalog_file(
    locale: impl Into<String>,
    path: impl AsRef<Path>,
) -> Result<(), CatalogError> {
    let path = path.as_ref();
    let yaml = fs::read_to_string(path).map_err(|source| CatalogError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    load_catalog_yaml(locale, &yaml)
}

/// Select the catalog that `t!` and [`translate`] will use.
pub fn activate_locale(locale: impl Into<String>) {
    state()
        .write()
        .expect("TFM runtime state lock poisoned")
        .active_locale = Some(locale.into());
}

/// Return a translation from the active catalog, or the English source fallback.
pub fn translate(source: &str) -> String {
    let state = state().read().expect("TFM runtime state lock poisoned");
    state
        .active_locale
        .as_ref()
        .and_then(|locale| state.catalogs.get(locale))
        .and_then(|catalog| catalog.get(source))
        .cloned()
        .unwrap_or_else(|| source.into())
}

/// Translate a source template and substitute named values in `{name}` placeholders.
///
/// Unknown placeholders are deliberately left visible so a catalog entry cannot silently
/// lose data when its source changes.
pub fn translate_with_args(source: &str, arguments: &[(&str, String)]) -> String {
    let mut translated = translate(source);
    for (name, value) in arguments {
        translated = translated.replace(&format!("{{{name}}}"), value);
    }
    translated
}

/// Translate an English source literal using the active target-locale catalog.
#[macro_export]
macro_rules! t {
    ($source:literal, $($name:ident = $value:expr),+ $(,)?) => {{
        $crate::translate_with_args(
            $source,
            &[$((stringify!($name), format!("{}", $value))),+],
        )
    }};
    ($source:literal) => {{ $crate::translate($source) }};
}

#[derive(Debug)]
pub enum CatalogError {
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    Parse(serde_yaml::Error),
}

impl fmt::Display for CatalogError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read { path, source } => write!(formatter, "read {}: {source}", path.display()),
            Self::Parse(source) => write!(formatter, "parse target locale catalog: {source}"),
        }
    }
}

impl StdError for CatalogError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::Read { source, .. } => Some(source),
            Self::Parse(source) => Some(source),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{activate_locale, load_catalog_yaml, translate, translate_with_args};

    #[test]
    fn translates_from_the_active_catalog_and_falls_back_to_source() {
        load_catalog_yaml("uk", "Save: Зберегти\n").unwrap();
        activate_locale("uk");

        assert_eq!(translate("Save"), "Зберегти");
        assert_eq!(crate::t!("Save"), "Зберегти");
        assert_eq!(translate("Cancel"), "Cancel");
    }

    #[test]
    fn substitutes_named_template_arguments_after_translation() {
        load_catalog_yaml("uk", "\"YAML: {name}\": \"YAML: {name}\"\n").unwrap();
        activate_locale("uk");

        assert_eq!(
            translate_with_args("YAML: {name}", &[("name", "config.yml".into())]),
            "YAML: config.yml"
        );
        assert_eq!(
            crate::t!("YAML: {name}", name = "config.yml"),
            "YAML: config.yml"
        );
    }
}
