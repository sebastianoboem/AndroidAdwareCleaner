# Scan Performance Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the package scan run adb work in parallel, halve process spawns, cut per-package round-trips, and skip re-enrichment on re-scans via a persistent cache.

**Architecture:** All changes live in `crates/adb-bridge`, `crates/package-scanner`, and the Tauri glue (`src-tauri/src/state.rs`, `commands.rs`). Progress events and `PackageRow` are unchanged; no frontend changes.

**Tech Stack:** Rust (rayon, serde_json, base64, std `OnceLock`). Spec: `docs/superpowers/specs/2026-08-22-scan-performance-design.md`.

## Global Constraints

- `PackageRow`, `ScanProgressEvent`, and the frontend contract must not change.
- Metadata cache is local-only (`<config_dir>/metadata-cache.json`), never synced.
- Enrichment thread pool: exactly 16 threads.
- Arsc strings fallback capped at 262144 bytes.
- Cache hit requires a present, matching `apk_path`.
- Commit after each task; run `cargo test --workspace` before each commit.

---

### Task 1: Cache the resolved adb serial

**Files:**
- Modify: `crates/adb-bridge/src/bridge.rs`

**Interfaces:**
- Produces: `select_serial(devices: &[AdbDevice], want: Option<&str>) -> Result<String, AdbError>`; `AdbBridge.resolved_serial: OnceLock<String>`; `get_serial()` resolves once per bridge instance.

- [x] **Step 1: Write failing tests** (in `mod tests` of `bridge.rs`)

```rust
fn dev(serial: &str, state: DeviceState) -> AdbDevice {
    AdbDevice { serial: serial.into(), state, model: None, product: None }
}

#[test]
fn select_serial_returns_explicit_serial_when_listed() {
    let devices = vec![dev("abc", DeviceState::Unauthorized)];
    assert_eq!(select_serial(&devices, Some("abc")).unwrap(), "abc");
}

#[test]
fn select_serial_errors_when_explicit_serial_missing() {
    let devices = vec![dev("other", DeviceState::Device)];
    assert!(matches!(select_serial(&devices, Some("abc")), Err(AdbError::NoDevice)));
}

#[test]
fn select_serial_picks_single_authorized_device() {
    let devices = vec![dev("un1", DeviceState::Unauthorized), dev("ok1", DeviceState::Device)];
    assert_eq!(select_serial(&devices, None).unwrap(), "ok1");
}

#[test]
fn select_serial_reports_unauthorized_when_no_authorized() {
    let devices = vec![dev("un1", DeviceState::Unauthorized)];
    assert!(matches!(select_serial(&devices, None), Err(AdbError::Unauthorized)));
}

#[test]
fn select_serial_errors_on_multiple_authorized() {
    let devices = vec![dev("a", DeviceState::Device), dev("b", DeviceState::Device)];
    assert!(matches!(select_serial(&devices, None), Err(AdbError::CommandFailed(_))));
}
```

- [x] **Step 2: Run tests, verify they fail** — `cargo test -p adb-bridge` → compile error: `select_serial` not found.

- [x] **Step 3: Implement**

Add `use std::sync::OnceLock;`. Extract selection from `get_serial` into a free function:

```rust
fn select_serial(devices: &[AdbDevice], want: Option<&str>) -> Result<String, AdbError> {
    if let Some(s) = want {
        if devices.iter().any(|d| d.serial == s) {
            return Ok(s.to_string());
        }
        return Err(AdbError::NoDevice);
    }
    let authorized: Vec<_> = devices.iter().filter(|d| d.state == DeviceState::Device).collect();
    match authorized.len() {
        0 => {
            if devices.iter().any(|d| d.state == DeviceState::Unauthorized) {
                return Err(AdbError::Unauthorized);
            }
            Err(AdbError::NoDevice)
        }
        1 => Ok(authorized[0].serial.clone()),
        _ => Err(AdbError::CommandFailed("multiple devices connected; specify serial".into())),
    }
}
```

Add field + cache in `get_serial` (both constructors set `resolved_serial: OnceLock::new()`):

```rust
pub struct AdbBridge {
    adb: PathBuf,
    serial: Option<String>,
    resolved_serial: OnceLock<String>,
}

pub fn get_serial(&self) -> Result<String, AdbError> {
    if let Some(s) = self.resolved_serial.get() {
        return Ok(s.clone());
    }
    let devices = self.list_devices()?;
    let serial = select_serial(&devices, self.serial.as_deref())?;
    let _ = self.resolved_serial.set(serial.clone());
    Ok(serial)
}
```

- [x] **Step 4: Run tests, verify pass** — `cargo test -p adb-bridge` → all green.
- [x] **Step 5: Commit** — `git commit -m "Cache resolved adb serial per bridge instance."`

---

### Task 2: Bulk APK paths via `pm list packages -f`

**Files:**
- Modify: `crates/adb-bridge/src/bridge.rs`
- Modify: `crates/package-scanner/src/lib.rs` (struct literal in tests if any; `PackageInfo` consumers)

**Interfaces:**
- Produces: `parse_package_line(line: &str) -> Option<(String, Option<String>)>` (name, apk_path); `PackageInfo.apk_path: Option<String>`.

- [x] **Step 1: Write failing tests**

```rust
#[test]
fn parses_package_line_with_path() {
    assert_eq!(
        parse_package_line("package:/data/app/~~Xq==/com.foo-Yz==/base.apk=com.foo"),
        Some(("com.foo".into(), Some("/data/app/~~Xq==/com.foo-Yz==/base.apk".into())))
    );
}

#[test]
fn parses_package_line_without_path() {
    assert_eq!(parse_package_line("package:com.foo"), Some(("com.foo".into(), None)));
}

#[test]
fn rejects_non_package_lines() {
    assert_eq!(parse_package_line(""), None);
    assert_eq!(parse_package_line("package:"), None);
}
```

- [x] **Step 2: Run, verify fail** — compile error: `parse_package_line` not found.

- [x] **Step 3: Implement**

```rust
fn parse_package_line(line: &str) -> Option<(String, Option<String>)> {
    let rest = line.trim().strip_prefix("package:")?;
    if rest.is_empty() {
        return None;
    }
    match rest.rfind('=') {
        Some(idx) => {
            let name = rest[idx + 1..].trim();
            if name.is_empty() {
                return None;
            }
            Some((name.to_string(), Some(rest[..idx].trim().to_string())))
        }
        None => Some((rest.trim().to_string(), None)),
    }
}
```

`PackageInfo` gains `pub apk_path: Option<String>`. `list_packages` adds `-f` to both invocations and builds entries via `parse_package_line`; the `-s` system set still collects names only.

- [x] **Step 4: Run, verify pass** — `cargo test --workspace` (scanner compiles against new field).
- [x] **Step 5: Commit** — `git commit -m "Fetch APK paths in bulk via pm list packages -f."`

---

### Task 3: Truly parallel adb enrichment

**Files:**
- Modify: `crates/package-scanner/src/lib.rs`

**Interfaces:**
- Produces: `PackageScanner.bridge: Arc<AdbBridge>`; `enrich_package(bridge: &AdbBridge, ...)`; `const SCAN_THREADS: usize = 16`.

Pure refactor: no behavior change, existing tests must stay green (TDD refactor phase).

- [x] **Step 1: Refactor**
  - `bridge: Arc<Mutex<AdbBridge>>` → `Arc<AdbBridge>`; drop all `bridge.lock()` sites (call methods directly).
  - `scan_with_progress` runs the per-package loop inside a dedicated pool:

```rust
const SCAN_THREADS: usize = 16;

let pool = rayon::ThreadPoolBuilder::new()
    .num_threads(SCAN_THREADS)
    .build()
    .expect("failed to build scan thread pool");
pool.install(|| {
    packages.par_iter().for_each(|pkg| { /* unchanged body, bridge: &AdbBridge */ });
});
```

- [x] **Step 2: Verify green** — `cargo test --workspace`.
- [x] **Step 3: Commit** — `git commit -m "Run per-package adb enrichment in parallel."`

---

### Task 4: Merged manifest+icon exec-out, arsc cap

**Files:**
- Modify: `crates/package-scanner/src/lib.rs`

**Interfaces:**
- Produces: `parse_manifest_icon_output(text: &str) -> (Option<Vec<u8>>, Option<Vec<u8>>)`; `fetch_manifest_and_icon(bridge, apk_path) -> (Option<Vec<u8>>, Option<Vec<u8>>)`; `device_has_base64(bridge) -> bool`; arsc scripts capped with `| head -c 262144`.

- [x] **Step 1: Write failing tests**

```rust
#[test]
fn parses_merged_manifest_and_icon_sections() {
    let manifest = b"\x03\x00\x08\x00manifest-bytes";
    let icon = b"\x89PNGicon-bytes";
    let text = format!(
        "M:{}\nI:{}\n",
        STANDARD.encode(manifest),
        STANDARD.encode(icon)
    );
    let (m, i) = parse_manifest_icon_output(&text);
    assert_eq!(m.as_deref(), Some(manifest.as_slice()));
    assert_eq!(i.as_deref(), Some(icon.as_slice()));
}

#[test]
fn parses_merged_output_with_wrapped_base64_lines() {
    let manifest = vec![7u8; 100];
    let encoded = STANDARD.encode(&manifest);
    let (head, tail) = encoded.split_at(76);
    let text = format!("M:{head}\n{tail}\n");
    let (m, i) = parse_manifest_icon_output(&text);
    assert_eq!(m.as_deref(), Some(manifest.as_slice()));
    assert!(i.is_none());
}

#[test]
fn merged_output_empty_yields_none() {
    assert_eq!(parse_manifest_icon_output(""), (None, None));
    assert_eq!(parse_manifest_icon_output("M:\nI:\n"), (None, None));
}
```

- [x] **Step 2: Run, verify fail** — compile error: `parse_manifest_icon_output` not found.

- [x] **Step 3: Implement**

```rust
fn parse_manifest_icon_output(text: &str) -> (Option<Vec<u8>>, Option<Vec<u8>>) {
    let mut manifest_b64 = String::new();
    let mut icon_b64 = String::new();
    let mut current: Option<&mut String> = None;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("M:") {
            manifest_b64.push_str(rest);
            current = Some(&mut manifest_b64);
        } else if let Some(rest) = line.strip_prefix("I:") {
            icon_b64.push_str(rest);
            current = Some(&mut icon_b64);
        } else if let Some(buf) = current.as_deref_mut() {
            buf.push_str(line);
        }
    }
    (decode_b64(&manifest_b64), decode_b64(&icon_b64))
}

fn decode_b64(s: &str) -> Option<Vec<u8>> {
    let compact: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    if compact.is_empty() {
        return None;
    }
    STANDARD.decode(compact).ok().filter(|b| !b.is_empty())
}
```

Device script (single exec-out, `{path}` = known APK path; icon selection identical to today's `fetch_apk_icon_data_url`):

```rust
fn fetch_manifest_and_icon(bridge: &AdbBridge, apk_path: &str) -> (Option<Vec<u8>>, Option<Vec<u8>>) {
    let cmd = format!(
        r#"APK="{apk_path}"; \
echo "M:$(unzip -p "$APK" AndroidManifest.xml 2>/dev/null | base64)"; \
ICON=$(unzip -l "$APK" 2>/dev/null | awk '/res\/mipmap/ && /\.(png|webp)$/ && /ic_launcher|launcher_icon|app_icon|icon_round/ {{print $4}}' | sort -r | head -1); \
if [ -z "$ICON" ]; then ICON=$(unzip -l "$APK" 2>/dev/null | awk '/res\/mipmap-xxhdpi/ && /\.(png|webp)$/ {{print $4}}' | head -1); fi; \
[ -n "$ICON" ] && echo "I:$(unzip -p "$APK" "$ICON" 2>/dev/null | base64)""#
    );
    match bridge.exec_out(&cmd) {
        Ok(bytes) => parse_manifest_icon_output(&String::from_utf8_lossy(&bytes)),
        Err(_) => (None, None),
    }
}

fn device_has_base64(bridge: &AdbBridge) -> bool {
    bridge
        .shell("command -v base64 >/dev/null 2>&1 && echo yes")
        .map(|o| o.contains("yes"))
        .unwrap_or(false)
}
```

`scan_with_progress` probes `device_has_base64` once. `enrich_package` uses the merged fetch when the probe succeeded and `apk_path` is `Some`; otherwise it keeps today's separate `fetch_manifest_bytes` / `fetch_apk_icon_data_url` (which now take the apk path when known and only fall back to `pm path` when it is `None`). Icon precedence unchanged: Play icon first for non-system, else apk icon bytes → data URL. Arsc scripts (both variants) get `| head -c 262144` after `strings`.

- [x] **Step 4: Run, verify pass** — `cargo test --workspace`.
- [x] **Step 5: Commit** — `git commit -m "Merge manifest and icon extraction into one exec-out."`

---

### Task 5: Persistent metadata cache

**Files:**
- Create: `crates/package-scanner/src/cache.rs`
- Modify: `crates/package-scanner/src/lib.rs`, `crates/package-scanner/Cargo.toml`
- Modify: `src-tauri/src/state.rs`, `src-tauri/src/commands.rs`

**Interfaces:**
- Consumes: `PackageInfo.apk_path` (Task 2).
- Produces: `MetadataCache::{load, get, put, save}`, `CachedMetadata`; `PackageScanner::with_cache_path(PathBuf)`; `AppState.metadata_cache_path: PathBuf`.

- [x] **Step 1: Write failing tests** (in `cache.rs`)

```rust
fn temp_cache_path(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("aac-cache-test-{}-{}.json", tag, std::process::id()))
}

#[test]
fn cache_roundtrip_persists_entries() {
    let path = temp_cache_path("roundtrip");
    let _ = std::fs::remove_file(&path);
    let mut cache = MetadataCache::load(Some(path.clone()));
    cache.put("com.foo", CachedMetadata {
        apk_path: Some("/data/app/x/base.apk".into()),
        label: Some("Foo".into()),
        author: Some("Foo Inc".into()),
        icon_url: None,
    });
    cache.save();
    let reloaded = MetadataCache::load(Some(path.clone()));
    let hit = reloaded.get("com.foo", Some("/data/app/x/base.apk")).unwrap();
    assert_eq!(hit.label.as_deref(), Some("Foo"));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn cache_misses_when_apk_path_changes() {
    let mut cache = MetadataCache::load(None);
    cache.put("com.foo", CachedMetadata {
        apk_path: Some("/old/base.apk".into()),
        label: Some("Foo".into()),
        author: None,
        icon_url: None,
    });
    assert!(cache.get("com.foo", Some("/new/base.apk")).is_none());
}

#[test]
fn cache_never_hits_without_current_apk_path() {
    let mut cache = MetadataCache::load(None);
    cache.put("com.foo", CachedMetadata { apk_path: None, label: Some("Foo".into()), author: None, icon_url: None });
    assert!(cache.get("com.foo", None).is_none());
}
```

- [x] **Step 2: Run, verify fail** — compile error: module/types not found.

- [x] **Step 3: Implement `cache.rs`**

```rust
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedMetadata {
    pub apk_path: Option<String>,
    pub label: Option<String>,
    pub author: Option<String>,
    pub icon_url: Option<String>,
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
        Self { path, entries, dirty: false }
    }

    pub fn get(&self, package: &str, apk_path: Option<&str>) -> Option<&CachedMetadata> {
        let current = apk_path?;
        self.entries
            .get(package)
            .filter(|e| e.apk_path.as_deref() == Some(current))
    }

    pub fn put(&mut self, package: &str, entry: CachedMetadata) {
        self.entries.insert(package.to_string(), entry);
        self.dirty = true;
    }

    pub fn save(&self) {
        let (Some(path), true) = (self.path.as_ref(), self.dirty) else { return };
        if let Ok(json) = serde_json::to_string(&self.entries) {
            let _ = std::fs::write(path, json);
        }
    }
}
```

Add `serde_json = { workspace = true }` to `crates/package-scanner/Cargo.toml`, `mod cache;` + `pub use cache::{CachedMetadata, MetadataCache};` in `lib.rs`.

- [x] **Step 4: Run, verify pass** — `cargo test -p package-scanner`.

- [x] **Step 5: Integrate into scanner and Tauri**

`PackageScanner` gains `cache_path: Option<PathBuf>` (default `None`) and:

```rust
pub fn with_cache_path(mut self, path: PathBuf) -> Self {
    self.cache_path = Some(path);
    self
}
```

In `scan_with_progress`: load the cache before the loop, share as `Arc<Mutex<MetadataCache>>`; per package, on `cache.get(&pkg.package_name, pkg.apk_path.as_deref())` hit, build `ScannedPackage` from cached label/author/icon plus per-scan `installer`/`is_device_admin`; on miss, run `enrich_package` then `cache.put(...)` with the enriched values and current `apk_path`. After the loop, `cache.lock().save()`.

`src-tauri/src/state.rs`: `AppState.metadata_cache_path = config_dir.join("metadata-cache.json")`.

`src-tauri/src/commands.rs`: `scan_packages` clones `state.metadata_cache_path` before `spawn_blocking`, passes it through `scan_packages_streaming` into `PackageScanner::new(bridge).with_cache_path(cache_path)`.

- [x] **Step 6: Run, verify pass** — `cargo test --workspace`.
- [x] **Step 7: Commit** — `git commit -m "Add persistent package metadata cache across scans."`

---

### Final verification

- [x] `cargo test --workspace` — all green.
- [x] `cargo build --workspace` (or `cargo check`) — no warnings introduced.
- [x] `graft build` to refresh the context graph.
