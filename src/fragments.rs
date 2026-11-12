//! `conf.d/` fragment discovery and envelope validation.
//!
//! A fragment is one TOML file declaring exactly one object with a `kind` and
//! an `id`. Files load in lexical filename order. Duplicate `(kind, id)` pairs,
//! unknown kinds, missing or malformed ids, and non-table documents are hard
//! errors that name the offending file.
//!
//! AIDEV-NOTE: an empty or missing `conf.d` is valid and enables nothing. There
//! are no implicit objects and no defaults. Per-kind field validation and
//! cross-reference checks are layered on top of this envelope, not baked in
//! here, so the schema can evolve without rewriting discovery.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum FragmentKind {
    Slots,
    Device,
    Source,
    Theme,
    Route,
}

impl FragmentKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Slots => "slots",
            Self::Device => "device",
            Self::Source => "source",
            Self::Theme => "theme",
            Self::Route => "route",
        }
    }
}

const KINDS: &str = "slots, device, source, theme, route";

fn parse_kind(value: &toml::Value, path: &Path) -> Result<FragmentKind> {
    let Some(name) = value.as_str() else {
        bail!("{}: kind must be a string", path.display());
    };
    Ok(match name {
        "slots" => FragmentKind::Slots,
        "device" => FragmentKind::Device,
        "source" => FragmentKind::Source,
        "theme" => FragmentKind::Theme,
        "route" => FragmentKind::Route,
        other => bail!(
            "{}: unknown fragment kind {other:?}; expected one of {KINDS}",
            path.display()
        ),
    })
}

fn valid_id(id: &str) -> bool {
    let mut characters = id.chars();
    matches!(characters.next(), Some('a'..='z'))
        && characters.all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-'
        })
}

// toml::Value contains floats, so it is PartialEq but not Eq.
#[derive(Debug, Clone, PartialEq)]
pub struct Fragment {
    pub path: PathBuf,
    pub kind: FragmentKind,
    pub id: String,
    pub document: toml::Value,
}

impl Fragment {
    /// The fragment's top-level table, for per-kind validation.
    pub fn table(&self) -> Result<&toml::map::Map<String, toml::Value>> {
        self.document
            .as_table()
            .ok_or_else(|| anyhow!("{} must be a TOML table", self.path.display()))
    }
}

/// Load every `*.toml` in `dir`, in lexical filename order.
pub fn load_fragments(dir: &Path) -> Result<Vec<Fragment>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut paths = Vec::new();
    for entry in fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
        let entry = entry.with_context(|| format!("read entry in {}", dir.display()))?;
        let path = entry.path();
        let is_toml = path.extension().and_then(|extension| extension.to_str()) == Some("toml");
        if is_toml && entry.file_type()?.is_file() {
            paths.push(path);
        }
    }
    paths.sort();

    let mut seen: BTreeMap<(FragmentKind, String), PathBuf> = BTreeMap::new();
    let mut fragments = Vec::new();
    for path in paths {
        let contents =
            fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        let document: toml::Value = toml::from_str(&contents)
            .with_context(|| format!("parse TOML in {}", path.display()))?;
        let table = document
            .as_table()
            .ok_or_else(|| anyhow!("{} must be a TOML table", path.display()))?;
        let kind = parse_kind(
            table
                .get("kind")
                .ok_or_else(|| anyhow!("{} is missing kind", path.display()))?,
            &path,
        )?;
        let id = table
            .get("id")
            .and_then(toml::Value::as_str)
            .ok_or_else(|| anyhow!("{} is missing a string id", path.display()))?
            .to_string();
        if !valid_id(&id) {
            bail!(
                "{}: id {id:?} must use lowercase kebab-case",
                path.display()
            );
        }
        let key = (kind, id.clone());
        if let Some(previous) = seen.get(&key) {
            bail!(
                "duplicate {} fragment id {id:?} in {} and {}",
                kind.as_str(),
                previous.display(),
                path.display()
            );
        }
        seen.insert(key, path.clone());
        fragments.push(Fragment {
            path,
            kind,
            id,
            document,
        });
    }
    Ok(fragments)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(label: &str) -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "beckon-fragments-{}-{label}-{sequence}",
                std::process::id()
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn write(&self, name: &str, contents: &str) {
            fs::write(self.0.join(name), contents).unwrap();
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn fragment(id: &str, kind: &str) -> String {
        format!("kind = \"{kind}\"\nid = \"{id}\"\n")
    }

    #[test]
    fn a_missing_directory_enables_nothing() {
        let dir = TempDir::new("missing");
        let absent = dir.0.join("absent");
        assert!(load_fragments(&absent).unwrap().is_empty());
    }

    #[test]
    fn an_empty_directory_enables_nothing() {
        let dir = TempDir::new("empty");
        assert!(load_fragments(&dir.0).unwrap().is_empty());
    }

    #[test]
    fn files_load_in_lexical_filename_order() {
        let dir = TempDir::new("order");
        dir.write("30-third.toml", &fragment("third", "source"));
        dir.write("10-first.toml", &fragment("first", "source"));
        dir.write("20-second.toml", &fragment("second", "source"));
        let loaded = load_fragments(&dir.0).unwrap();
        let ids: Vec<_> = loaded.iter().map(|item| item.id.as_str()).collect();
        assert_eq!(ids, ["first", "second", "third"]);
    }

    #[test]
    fn non_toml_files_are_ignored() {
        let dir = TempDir::new("ignore");
        dir.write("README.md", "not a fragment");
        dir.write("one.toml", &fragment("one", "device"));
        assert_eq!(load_fragments(&dir.0).unwrap().len(), 1);
    }

    #[test]
    fn duplicate_kind_and_id_is_rejected_naming_both_files() {
        let dir = TempDir::new("duplicate");
        dir.write("a.toml", &fragment("rex", "source"));
        dir.write("b.toml", &fragment("rex", "source"));
        let error = load_fragments(&dir.0).unwrap_err().to_string();
        assert!(error.contains("duplicate"), "{error}");
        assert!(
            error.contains("a.toml") && error.contains("b.toml"),
            "{error}"
        );
    }

    #[test]
    fn the_same_id_in_different_kinds_is_allowed() {
        let dir = TempDir::new("kinds");
        dir.write("a.toml", &fragment("rex", "source"));
        dir.write("b.toml", &fragment("rex", "device"));
        assert_eq!(load_fragments(&dir.0).unwrap().len(), 2);
    }

    #[test]
    fn unknown_kind_names_the_file() {
        let dir = TempDir::new("kind");
        dir.write("bad.toml", "kind = \"widget\"\nid = \"one\"\n");
        let error = load_fragments(&dir.0).unwrap_err().to_string();
        assert!(error.contains("bad.toml"), "{error}");
        assert!(error.contains("unknown fragment kind"), "{error}");
    }

    #[test]
    fn missing_id_names_the_file() {
        let dir = TempDir::new("noid");
        dir.write("bad.toml", "kind = \"device\"\n");
        let error = load_fragments(&dir.0).unwrap_err().to_string();
        assert!(
            error.contains("bad.toml") && error.contains("missing"),
            "{error}"
        );
    }

    #[test]
    fn a_malformed_id_is_rejected() {
        let dir = TempDir::new("badid");
        dir.write("bad.toml", "kind = \"device\"\nid = \"Bad_ID\"\n");
        assert!(load_fragments(&dir.0).is_err());
    }

    #[test]
    fn a_non_table_document_is_rejected() {
        let dir = TempDir::new("non-table");
        dir.write("bad.toml", "just a string\n");
        assert!(load_fragments(&dir.0).is_err());
    }

    #[test]
    fn the_document_is_retained_for_per_kind_validation() {
        let dir = TempDir::new("document");
        dir.write(
            "one.toml",
            "kind = \"slots\"\nid = \"main\"\nids = [\"s1\"]\n",
        );
        let loaded = load_fragments(&dir.0).unwrap();
        assert_eq!(loaded[0].kind, FragmentKind::Slots);
        assert_eq!(loaded[0].table().unwrap()["ids"][0].as_str(), Some("s1"));
    }
}
