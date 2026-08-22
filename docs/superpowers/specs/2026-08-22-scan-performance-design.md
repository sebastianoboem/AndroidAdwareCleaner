# Scan performance

Date: 2026-08-22
Status: approved design (approved in chat: "tutte")

## Goal

Make the package scan substantially faster, both on first scan and on re-scans,
without changing what the scan produces (`PackageRow` fields, progress events,
frontend behavior stay identical).

## Current bottlenecks

1. **ADB calls are serialized.** `PackageScanner` holds the bridge in
   `Arc<Mutex<AdbBridge>>` and keeps the lock for the whole duration of every
   adb command inside `enrich_package`. `rayon` parallelism only helps the
   Play Store HTTP fetches; all adb work runs one command at a time.
2. **Every adb command spawns two processes.** `AdbBridge::shell`/`exec_out`
   call `get_serial()`, which runs `adb devices -l` before every actual
   command — even when the serial is already known.
3. **Up to 3 adb round-trips per package, each re-running `pm path`.**
   Manifest, `resources.arsc` and icon extraction are separate device scripts
   that each resolve the APK path again.
4. **No cache across scans.** `PackageScanner` (and its in-memory Play cache)
   is recreated on every scan, so a re-scan repeats all adb and HTTP work,
   including Play Store fetches with an 8s timeout each.
5. **Unbounded `resources.arsc | strings` fallback** can push megabytes
   through `adb shell`; the rayon pool (sized to CPU cores) is small for
   I/O-bound work.

## Design

### 1. Cache the resolved serial in `AdbBridge`

- Add `resolved_serial: OnceLock<String>` to `AdbBridge`.
- `get_serial()` returns the cached value when present; otherwise it resolves
  as today (validate explicit serial / pick the single authorized device) and
  stores the result.
- Selection logic is extracted into a pure `select_serial(devices, want)`
  function (unit-tested).
- Lifetime = one `AdbBridge` instance = one scan/uninstall command, so
  staleness is not a concern: if the device drops mid-scan, commands fail the
  same way they do today.

### 2. Fetch APK paths in bulk with `pm list packages -f`

- `list_packages` adds `-f` to both invocations (all + `-s` system list).
- New pure parser handles `package:<path>=<name>` (split at the **last** `=`,
  paths may contain `==`) and falls back to the plain `package:<name>` form.
- `PackageInfo` gains `apk_path: Option<String>`.
- Per-package device scripts use the known APK path directly; `pm path` runs
  only as fallback when `apk_path` is `None`.

### 3. Truly parallel adb during enrichment

- `PackageScanner.bridge` becomes `Arc<AdbBridge>` (no `Mutex`; all bridge
  methods take `&self` and the struct is stateless, so it is `Sync`).
- `scan_with_progress` runs the per-package loop on a dedicated rayon pool of
  16 threads (I/O-bound work; also caps concurrent adb commands at 16).

### 4. One merged exec-out per package for manifest + icon

- Single device script outputs both blobs base64-encoded with line-prefix
  framing (`M:` for manifest, `I:` for icon), so binary data survives shell
  command substitution and one process spawn replaces two/three:

  ```sh
  APK="<path>"
  echo "M:$(unzip -p "$APK" AndroidManifest.xml 2>/dev/null | base64)"
  ICON=<same mipmap selection as today via unzip -l + awk>
  [ -n "$ICON" ] && echo "I:$(unzip -p "$APK" "$ICON" 2>/dev/null | base64)"
  ```

- Host-side pure parser `parse_manifest_icon_output` collects the sections
  (base64 may be line-wrapped), decodes them ignoring whitespace, and returns
  `(Option<Vec<u8>>, Option<Vec<u8>>)` (unit-tested).
- If the merged script returns nothing (e.g. device without `base64`,
  Android < 6), fall back to today's separate fetches.
- `resources.arsc | strings` fallback stays a separate call (only runs when no
  label was found) but is capped with `| head -c 262144`.

### 5. Persistent metadata cache across scans

- New module `crates/package-scanner/src/cache.rs`:
  - `CachedMetadata { apk_path, label, author, icon_url }`
  - `MetadataCache::load(Option<PathBuf>)`, `get(package, apk_path)` (hit only
    when the current `apk_path` is present and equals the stored one — an APK
    update changes its path, which invalidates the entry; packages without a
    known path are never served from cache), `put(...)`, `save()` (JSON,
    written only if dirty).
- `PackageScanner::with_cache_path(PathBuf)` opts in; on cache hit
  `enrich_package` is skipped entirely (label/author/icon come from the cache;
  `installer`/`is_device_admin` still come from the per-scan bulk dumpsys,
  which stays: 2 bulk calls per scan).
- Cache file: `<config_dir>/metadata-cache.json` (same dir as `config.json`),
  exposed as `AppState.metadata_cache_path` and passed to the scanner in
  `scan_packages`. Local-only: NOT part of the reputation DB, never synced
  (icon data-URLs would bloat cloud sync).
- Staleness policy: entries refresh only when the APK path changes. Play
  metadata fetched while offline may cache a degraded label/icon until the app
  updates — acceptable for this tool's scan → uninstall → re-scan loop.

## Implementation surface

| File | Change |
|---|---|
| `crates/adb-bridge/src/bridge.rs` | serial cache, `select_serial`, `-f` list + parser, `PackageInfo.apk_path` |
| `crates/package-scanner/src/lib.rs` | `Arc<AdbBridge>`, dedicated pool, merged fetch + parser, arsc cap, cache integration |
| `crates/package-scanner/src/cache.rs` | new: persistent metadata cache |
| `crates/package-scanner/Cargo.toml` | add `serde_json` (workspace dep) |
| `src-tauri/src/state.rs` | `metadata_cache_path` field |
| `src-tauri/src/commands.rs` | pass cache path to scanner |

No frontend, DB schema, or sync changes. Progress events and `PackageRow` are
unchanged.

## Out of scope

- Parsing `resources.arsc` properly to resolve label references
- Configurable pool size / concurrency limits
- Caching or batching for the uninstall flow
- Persisting the Play HTML cache separately (subsumed by the metadata cache)

## Acceptance criteria

1. `cargo test` passes across the workspace with the new unit tests
   (serial selection, `-f` parser, merged-output parser, cache roundtrip and
   invalidation).
2. During a scan, adb commands run concurrently (no bridge mutex), and each
   `shell`/`exec_out` spawns exactly one adb process after the first call.
3. A package with a known APK path triggers at most 2 adb calls (merged
   manifest+icon, plus arsc fallback only when the label is missing).
4. Re-scanning with an unchanged device skips manifest/icon/Play work for all
   cached packages and still emits complete rows.
5. Scan output (labels, authors, icons, flags) is unchanged for uncached
   scans, modulo the arsc `strings` cap.
