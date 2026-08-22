use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// Metadata cached per package. `apk_path` is the freshness key: an app
/// update installs into a new directory, which invalidates the entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedMetadata {
    pub apk_path: Option<String>,
    pub label: Option<String>,
    pub author: Option<String>,
    pub icon_url: Option<String>,
    /// Old entries default to false and are treated as a miss so they can be
    /// re-enriched after label/icon quality fixes.
    #[serde(default)]
    pub complete: bool,
}

pub struct MetadataCache {
    path: Option<PathBuf>,
    entries: HashMap<String, CachedMetadata>,
    dirty: bool,
}

impl MetadataCache {
    pub fn load(path: Option<PathBuf>) -> Self {
        let entries = path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|data| serde_json::from_str(&data).ok())
            .unwrap_or_default();
        Self {
            path,
            entries,
            dirty: false,
        }
    }

    pub fn get(&self, package: &str, apk_path: Option<&str>) -> Option<&CachedMetadata> {
        let current = apk_path?;
        self.entries.get(package).filter(|e| {
            e.complete && e.apk_path.as_deref() == Some(current)
        })
    }

    pub fn put(&mut self, package: &str, entry: CachedMetadata) {
        self.entries.insert(package.to_string(), entry);
        self.dirty = true;
    }

    pub fn save(&self) {
        let Some(path) = self.path.as_ref() else {
            return;
        };
        if !self.dirty {
            return;
        }
        if let Ok(json) = serde_json::to_string(&self.entries) {
            let _ = std::fs::write(path, json);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_cache_path(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("aac-cache-test-{}-{}.json", tag, std::process::id()))
    }

    #[test]
    fn cache_roundtrip_persists_entries() {
        let path = temp_cache_path("roundtrip");
        let _ = std::fs::remove_file(&path);
        let mut cache = MetadataCache::load(Some(path.clone()));
        cache.put(
            "com.foo",
            CachedMetadata {
                apk_path: Some("/data/app/x/base.apk".into()),
                label: Some("Foo".into()),
                author: Some("Foo Inc".into()),
                icon_url: None,
                complete: true,
            },
        );
        cache.save();
        let reloaded = MetadataCache::load(Some(path.clone()));
        let hit = reloaded
            .get("com.foo", Some("/data/app/x/base.apk"))
            .unwrap();
        assert_eq!(hit.label.as_deref(), Some("Foo"));
        assert_eq!(hit.author.as_deref(), Some("Foo Inc"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn cache_misses_when_apk_path_changes() {
        let mut cache = MetadataCache::load(None);
        cache.put(
            "com.foo",
            CachedMetadata {
                apk_path: Some("/old/base.apk".into()),
                label: Some("Foo".into()),
                author: None,
                icon_url: None,
                complete: true,
            },
        );
        assert!(cache.get("com.foo", Some("/new/base.apk")).is_none());
    }

    #[test]
    fn cache_never_hits_without_current_apk_path() {
        let mut cache = MetadataCache::load(None);
        cache.put(
            "com.foo",
            CachedMetadata {
                apk_path: None,
                label: Some("Foo".into()),
                author: None,
                icon_url: None,
                complete: true,
            },
        );
        assert!(cache.get("com.foo", None).is_none());
    }

    #[test]
    fn cache_misses_incomplete_legacy_entries() {
        let mut cache = MetadataCache::load(None);
        cache.put(
            "com.foo",
            CachedMetadata {
                apk_path: Some("/data/app/x/base.apk".into()),
                label: Some("Google".into()),
                author: None,
                icon_url: None,
                complete: false,
            },
        );
        assert!(cache.get("com.foo", Some("/data/app/x/base.apk")).is_none());
    }
}
