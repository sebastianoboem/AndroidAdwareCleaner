use adb_bridge::AdbBridge;
use serde::Serialize;
use std::collections::{HashMap, HashSet};

pub const LARGE_FILE_MIN_BYTES: u64 = 100 * 1024 * 1024;
const TRIM_CACHES: &str = "pm trim-caches 999999999999";

const ROOTS: &[&str] = &["/sdcard", "/storage/emulated/0", "/storage/self/primary"];
const AD_DIR_NAMES: &[&str] = &[
    "admob",
    ".admob",
    "vungle",
    ".vungle",
    "vungle_cache",
    "applovin",
    "chartboost",
    "mopub",
    "ironsource",
    "unityads",
    "adcolony",
    "tapjoy",
    "inmobi",
    "startapp",
    "appnext",
    "fyber",
    "mintegral",
    "mbridge",
    "moloco",
    "pangle",
];

pub const SYSTEM_CACHE: &str = "system_cache";
pub const RESIDUAL: &str = "residual";
pub const AD_JUNK: &str = "ad_junk";
pub const APK: &str = "apk";
pub const APP_CACHE: &str = "app_cache";

const SCAN_STAGES: &[(Option<&str>, &str)] = &[
    (
        None,
        "pm list packages 2>/dev/null; echo '---END PACKAGES---'",
    ),
    (
        Some("/sdcard/Android/data/"),
        "ls -1 /sdcard/Android/data 2>/dev/null; echo '---END DATA---'",
    ),
    (
        Some("/sdcard/Android/obb/"),
        "ls -1 /sdcard/Android/obb 2>/dev/null; echo '---END OBB---'",
    ),
    (
        None,
        "root=/sdcard; [ -d \"$root\" ] || root=/storage/emulated/0; find -H \"$root\" \\( -path \"$root/Android/data\" -o -path \"$root/Android/obb\" -o -path \"$root/Android/media\" \\) -prune -o -type f -print0 2>/dev/null | xargs -0 -r -s 4096 stat -c '%s\t%n'",
    ),
    (
        None,
        "root=/sdcard; [ -d \"$root\" ] || root=/storage/emulated/0; find -H \"$root/Android/data\" \"$root/Android/obb\" \"$root/Android/media\" -type f -print0 2>/dev/null | xargs -0 -r -s 4096 stat -c '%s\t%n'; echo '---END FILES---'",
    ),
    (
        None,
        "dumpsys package 2>/dev/null | grep cacheSize; dumpsys diskstats 2>/dev/null | grep 'App Cache Size:'; echo '---END CACHESIZE---'; du -k -s /cache /data/cache /data/user/*/cache /data/user/*/code_cache /data/user_de/*/cache /data/user_de/*/code_cache /data/data/*/cache /data/data/*/code_cache 2>/dev/null; echo '---END CACHE---'",
    ),
];

#[derive(Debug, Clone, Serialize)]
pub struct StorageTarget {
    pub path: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct OptimizeCategory {
    pub id: String,
    pub bytes: u64,
    pub count: u64,
    pub estimated: bool,
    pub targets: Vec<StorageTarget>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LargeFile {
    pub path: String,
    pub bytes: u64,
}

#[derive(Debug, Clone)]
pub struct OptimizeSnapshot {
    pub scan_id: u64,
    pub categories: Vec<OptimizeCategory>,
    pub large_files: Vec<LargeFile>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OptimizeScan {
    pub scan_id: u64,
    pub categories: Vec<OptimizeCategory>,
    pub large_files: Vec<LargeFile>,
}

#[derive(Debug, Clone)]
pub enum DeleteOp {
    TrimCaches,
    Remove { path: String, bytes: u64 },
}

#[derive(Debug, Clone, Serialize)]
pub struct CleanResult {
    pub freed_bytes: u64,
    pub trimmed_system_cache: bool,
    pub errors: Vec<String>,
}

#[derive(Debug)]
struct ScanInput {
    installed: HashSet<String>,
    data_dirs: Vec<String>,
    obb_dirs: Vec<String>,
    files: Vec<(String, u64)>,
    system_cache: Vec<u64>,
}

pub fn scan(bridge: &AdbBridge, mut on_path: impl FnMut(&str)) -> Result<OptimizeSnapshot, String> {
    let mut output = String::new();
    let mut last_sent = std::time::Instant::now() - std::time::Duration::from_secs(1);
    for (prefix, command) in SCAN_STAGES {
        let chunk = bridge
            .shell_lines(command, |line| {
                let Some(path) = scanned_path(line, *prefix) else {
                    return;
                };
                let now = std::time::Instant::now();
                if now.duration_since(last_sent) < std::time::Duration::from_millis(80) {
                    return;
                }
                last_sent = now;
                on_path(&path);
            })
            .map_err(|e| e.to_string())?;
        output.push_str(&chunk);
        if !chunk.ends_with('\n') {
            output.push('\n');
        }
    }
    Ok(classify(&parse_scan_output(&output)))
}

fn scanned_path(line: &str, prefix: Option<&str>) -> Option<String> {
    let line = line.trim().trim_end_matches('\r');
    if line.is_empty() || line.starts_with("---") {
        return None;
    }
    if let Some((size, path)) = line.split_once('\t').or_else(|| line.split_once(' ')) {
        if size.trim().parse::<u64>().is_ok() && path.trim_start().starts_with('/') {
            return Some(path.trim().to_string());
        }
    }
    let prefix = prefix?;
    if line.contains('/') || line.contains(' ') {
        return None;
    }
    Some(format!("{prefix}{line}"))
}

fn classify(input: &ScanInput) -> OptimizeSnapshot {
    let root = input
        .files
        .iter()
        .find_map(|(path, _)| storage_root(path))
        .unwrap_or("/sdcard")
        .to_string();

    let mut residual: HashMap<String, u64> = HashMap::new();
    let mut app_cache: HashMap<String, u64> = HashMap::new();
    let mut ad_junk: HashMap<String, u64> = HashMap::new();
    let mut apks: HashMap<String, u64> = HashMap::new();
    let mut large = Vec::new();

    for (parent, names) in [("data", &input.data_dirs), ("obb", &input.obb_dirs)] {
        for name in names {
            if !is_package_dir(name) || input.installed.contains(name) {
                continue;
            }
            let path = format!("{root}/Android/{parent}/{name}");
            if validate_deletable(&path).is_ok() {
                residual.entry(path).or_insert(0);
            }
        }
    }

    for (path, bytes) in &input.files {
        let Some(path) = normalize_storage_path(path) else {
            continue;
        };
        if let Some(dir) = residual_dir(&path, &input.installed) {
            *residual.entry(dir).or_insert(0) += bytes;
        } else if let Some(dir) = ad_dir(&path) {
            *ad_junk.entry(dir).or_insert(0) += bytes;
        } else if let Some(dir) = app_cache_dir(&path, &input.installed) {
            *app_cache.entry(dir).or_insert(0) += bytes;
        } else if is_apk(&path) {
            apks.insert(path.clone(), *bytes);
        }
        if *bytes >= LARGE_FILE_MIN_BYTES {
            large.push(LargeFile {
                path,
                bytes: *bytes,
            });
        }
    }

    large.sort_by(|a, b| b.bytes.cmp(&a.bytes).then(a.path.cmp(&b.path)));

    OptimizeSnapshot {
        scan_id: 0,
        categories: vec![
            OptimizeCategory {
                id: SYSTEM_CACHE.to_string(),
                bytes: input.system_cache.iter().sum(),
                count: input.system_cache.len() as u64,
                estimated: false,
                targets: Vec::new(),
            },
            category(RESIDUAL, targets_from(residual), false),
            category(AD_JUNK, targets_from(ad_junk), false),
            category(APK, targets_from(apks), false),
            category(APP_CACHE, targets_from(app_cache), false),
        ],
        large_files: large,
    }
}

pub fn plan(
    snapshot: &OptimizeSnapshot,
    categories: &[String],
    large_files: &[String],
) -> Result<Vec<DeleteOp>, String> {
    let known: HashSet<&str> = categories_by_id(snapshot);
    let mut selected = HashSet::new();
    for id in categories {
        if !known.contains(id.as_str()) {
            return Err(format!("categoria sconosciuta: {id}"));
        }
        selected.insert(id.as_str());
    }

    let allowed: HashSet<&str> = snapshot.large_files.iter().map(|f| f.path.as_str()).collect();
    let mut ops = Vec::new();
    if selected.contains(SYSTEM_CACHE) {
        ops.push(DeleteOp::TrimCaches);
    }

    let mut removals: Vec<(String, u64)> = Vec::new();
    for category in &snapshot.categories {
        if category.id == SYSTEM_CACHE || !selected.contains(category.id.as_str()) {
            continue;
        }
        for target in &category.targets {
            validate_deletable(&target.path)?;
            removals.push((target.path.clone(), target.bytes));
        }
    }

    for path in large_files {
        if !allowed.contains(path.as_str()) {
            return Err(format!("file non presente nella scansione: {path}"));
        }
        validate_deletable(path)?;
        let bytes = snapshot
            .large_files
            .iter()
            .find(|f| f.path == *path)
            .map(|f| f.bytes)
            .unwrap_or(0);
        removals.push((path.clone(), bytes));
    }

    removals.sort_by(|a, b| a.0.len().cmp(&b.0.len()).then(a.0.cmp(&b.0)));
    let mut kept: Vec<(String, u64)> = Vec::new();
    for (path, bytes) in removals {
        if kept.iter().any(|(parent, _)| is_under(parent, &path)) {
            continue;
        }
        kept.push((path, bytes));
    }
    for (path, bytes) in kept {
        ops.push(DeleteOp::Remove { path, bytes });
    }

    if ops.is_empty() {
        return Err("Niente da eliminare".into());
    }
    Ok(ops)
}

pub fn execute<F, P>(ops: &[DeleteOp], mut run: F, mut progress: P) -> CleanResult
where
    F: FnMut(&str) -> Result<String, String>,
    P: FnMut(usize, usize, &str, bool, &str),
{
    let total = ops.len();
    let mut freed_bytes = 0;
    let mut trimmed_system_cache = false;
    let mut errors = Vec::new();
    for (index, op) in ops.iter().enumerate() {
        let current = index + 1;
        let (label, command, bytes, trim) = match op {
            DeleteOp::TrimCaches => (
                "System and user cache".to_string(),
                TRIM_CACHES.to_string(),
                0,
                true,
            ),
            DeleteOp::Remove { path, bytes } => {
                (path.clone(), format!("rm -rf -- {}", shell_quote(path)), *bytes, false)
            }
        };
        match run(&command) {
            Ok(_) => {
                freed_bytes += bytes;
                trimmed_system_cache |= trim;
                progress(current, total, &label, true, "");
            }
            Err(err) => {
                errors.push(format!("{label}: {err}"));
                progress(current, total, &label, false, &err);
            }
        }
    }
    CleanResult {
        freed_bytes,
        trimmed_system_cache,
        errors,
    }
}

pub fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn categories_by_id(snapshot: &OptimizeSnapshot) -> HashSet<&str> {
    snapshot.categories.iter().map(|c| c.id.as_str()).collect()
}

fn category(id: &str, targets: Vec<StorageTarget>, estimated: bool) -> OptimizeCategory {
    let bytes = targets.iter().map(|t| t.bytes).sum();
    let count = targets.len() as u64;
    OptimizeCategory {
        id: id.to_string(),
        bytes,
        count,
        estimated,
        targets,
    }
}

fn targets_from(map: HashMap<String, u64>) -> Vec<StorageTarget> {
    let mut targets: Vec<_> = map
        .into_iter()
        .map(|(path, bytes)| StorageTarget { path, bytes })
        .collect();
    targets.sort_by(|a, b| b.bytes.cmp(&a.bytes).then(a.path.cmp(&b.path)));
    targets
}

fn parse_scan_output(output: &str) -> ScanInput {
    let mut section = "packages";
    let mut input = ScanInput {
        installed: HashSet::new(),
        data_dirs: Vec::new(),
        obb_dirs: Vec::new(),
        files: Vec::new(),
        system_cache: Vec::new(),
    };
    let mut cache_sizes = Vec::new();
    let mut disk_total: Option<u64> = None;
    let mut cache_du = Vec::new();
    let mut seen = HashSet::new();
    for line in output.lines() {
        let line = line.trim_end_matches('\r');
        match line {
            "---END PACKAGES---" => section = "data",
            "---END DATA---" => section = "obb",
            "---END OBB---" => section = "files",
            "---END FILES---" => section = "cachesize",
            "---END CACHESIZE---" => section = "cachedu",
            "---END CACHE---" => break,
            _ => match section {
                "packages" => {
                    if let Some(name) = line.trim().strip_prefix("package:") {
                        if !name.is_empty() {
                            input.installed.insert(name.to_string());
                        }
                    }
                }
                "data" | "obb" => {
                    let name = line.trim();
                    if name.is_empty() || name.contains('/') {
                        continue;
                    }
                    if section == "data" {
                        input.data_dirs.push(name.to_string());
                    } else {
                        input.obb_dirs.push(name.to_string());
                    }
                }
                "files" => {
                    let Some((size, path)) = line.split_once('\t') else {
                        continue;
                    };
                    let Ok(bytes) = size.trim().parse::<u64>() else {
                        continue;
                    };
                    let path = path.trim();
                    if path.is_empty() || !seen.insert(path.to_string()) {
                        continue;
                    }
                    input.files.push((path.to_string(), bytes));
                }
                "cachesize" => {
                    if let Some(bytes) = parse_app_cache_total(line) {
                        disk_total = Some(bytes);
                    } else if let Some(bytes) = parse_cache_size_line(line) {
                        cache_sizes.push(bytes);
                    }
                }
                "cachedu" => {
                    if let Some(entry) = parse_du_line(line) {
                        cache_du.push(entry);
                    }
                }
                _ => {}
            },
        }
    }
    input.system_cache = system_cache_sizes(&cache_sizes, disk_total, &cache_du);
    input
}

fn parse_app_cache_total(line: &str) -> Option<u64> {
    let rest = line.trim().strip_prefix("App Cache Size:")?.trim();
    let number: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    let bytes = number.parse::<u64>().ok()?;
    (bytes > 0).then_some(bytes)
}

fn parse_cache_size_line(line: &str) -> Option<u64> {
    let rest = line
        .split_once("cacheSize=")
        .or_else(|| line.split_once("cacheSize:"))?
        .1
        .trim();
    let number: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    let bytes = number.parse::<u64>().ok()?;
    (bytes > 0).then_some(bytes)
}

fn parse_du_line(line: &str) -> Option<(String, u64)> {
    let (size, path) = line.trim().split_once('\t').or_else(|| line.trim().split_once(' '))?;
    let kb = size.trim().parse::<u64>().ok()?;
    let path = path.trim();
    if kb == 0 || path.is_empty() {
        return None;
    }
    Some((path.to_string(), kb.saturating_mul(1024)))
}

fn system_cache_sizes(cache_sizes: &[u64], disk_total: Option<u64>, du: &[(String, u64)]) -> Vec<u64> {
    let mut sizes: Vec<u64> = if let Some(total) = disk_total {
        vec![total]
    } else {
        cache_sizes.iter().copied().filter(|bytes| *bytes > 0).collect()
    };
    let apps_known = !sizes.is_empty();
    for (path, bytes) in du {
        if *bytes == 0 {
            continue;
        }
        if apps_known && path != "/cache" && path != "/data/cache" {
            continue;
        }
        sizes.push(*bytes);
    }
    sizes
}

fn residual_dir(path: &str, installed: &HashSet<String>) -> Option<String> {
    let (root, parts) = split_storage(path)?;
    if parts.len() < 3 || parts[0] != "Android" || (parts[1] != "data" && parts[1] != "obb") {
        return None;
    }
    let pkg = parts[2];
    if !is_package_dir(pkg) || installed.contains(pkg) {
        return None;
    }
    Some(format!("{root}/Android/{}/{pkg}", parts[1]))
}

fn app_cache_dir(path: &str, installed: &HashSet<String>) -> Option<String> {
    let (root, parts) = split_storage(path)?;
    if parts.len() < 4 || parts[0] != "Android" || parts[1] != "data" || parts[3] != "cache" {
        return None;
    }
    let pkg = parts[2];
    if !is_package_dir(pkg) || !installed.contains(pkg) {
        return None;
    }
    Some(format!("{root}/Android/data/{pkg}/cache"))
}

fn ad_dir(path: &str) -> Option<String> {
    let (root, parts) = split_storage(path)?;
    let mut acc = root.to_string();
    for part in parts {
        acc.push('/');
        acc.push_str(part);
        let name = part.to_ascii_lowercase();
        if AD_DIR_NAMES.iter().any(|token| name.contains(token)) && validate_deletable(&acc).is_ok() {
            return Some(acc);
        }
    }
    None
}

fn is_apk(path: &str) -> bool {
    path.rsplit('/').next().unwrap_or("").to_ascii_lowercase().ends_with(".apk")
}

fn is_package_dir(name: &str) -> bool {
    let mut parts = name.split('.');
    let Some(first) = parts.next() else {
        return false;
    };
    if first.is_empty()
        || !first.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        || !first.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return false;
    }
    let mut has_rest = false;
    for part in parts {
        has_rest = true;
        if part.is_empty() || !part.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return false;
        }
    }
    has_rest
}

fn split_storage(path: &str) -> Option<(&str, Vec<&str>)> {
    let root = storage_root(path)?;
    let rel = path[root.len()..].trim_start_matches('/');
    if rel.is_empty() {
        return None;
    }
    Some((root, rel.split('/').filter(|p| !p.is_empty()).collect()))
}

fn storage_root(path: &str) -> Option<&str> {
    ROOTS.iter().copied().find(|root| path == *root || path.starts_with(&format!("{root}/")))
}

fn normalize_storage_path(path: &str) -> Option<String> {
    let path = validate_deletable(path).ok()?;
    Some(path)
}

pub fn validate_deletable(path: &str) -> Result<String, String> {
    if path.contains('\0') || path.contains('\n') || path.contains('\r') {
        return Err("percorso non valido".into());
    }
    if !path.starts_with('/') {
        return Err("percorso non valido".into());
    }
    let mut parts = Vec::new();
    for part in path.split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            return Err("percorso non valido".into());
        }
        parts.push(part);
    }
    let normalized = format!("/{}", parts.join("/"));
    let Some(root) = storage_root(&normalized) else {
        return Err("percorso fuori dalla memoria condivisa".into());
    };
    if normalized == root {
        return Err("percorso non eliminabile".into());
    }
    let rel = normalized[root.len()..].trim_start_matches('/');
    if matches!(rel, "Android" | "Android/data" | "Android/obb") {
        return Err("percorso non eliminabile".into());
    }
    Ok(normalized)
}

fn is_under(parent: &str, path: &str) -> bool {
    path == parent || path.starts_with(&format!("{parent}/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(installed: &[&str], data: &[&str], obb: &[&str], files: &[(&str, u64)]) -> ScanInput {
        ScanInput {
            installed: installed.iter().map(|s| (*s).to_string()).collect(),
            data_dirs: data.iter().map(|s| (*s).to_string()).collect(),
            obb_dirs: obb.iter().map(|s| (*s).to_string()).collect(),
            files: files.iter().map(|(p, n)| ((*p).to_string(), *n)).collect(),
            system_cache: Vec::new(),
        }
    }

    fn cat<'a>(snapshot: &'a OptimizeSnapshot, id: &str) -> &'a OptimizeCategory {
        snapshot.categories.iter().find(|c| c.id == id).unwrap()
    }

    #[test]
    fn classifies_residual_cache_ads_and_apks() {
        let snapshot = classify(&input(
            &["com.live"],
            &["com.live", "com.gone", "notes"],
            &["com.gone.obb"],
            &[
                ("/sdcard/Android/data/com.gone/files/left.cfg", 30),
                ("/sdcard/Android/data/com.live/cache/thumb.jpg", 20),
                ("/sdcard/Android/data/com.live/files/vungle/ad.bin", 7),
                ("/sdcard/Android/data/com.live/cache/UnityAdsCache/video.mp4", 9),
                ("/sdcard/Android/data/com.live/files/mb/res/.mbridge700/m.tar", 3),
                ("/sdcard/Download/app.apk", 11),
                ("/sdcard/Download/video.mp4", LARGE_FILE_MIN_BYTES),
                ("/sdcard/Download/small.mp4", LARGE_FILE_MIN_BYTES - 1),
            ],
        ));

        let residual = cat(&snapshot, RESIDUAL);
        assert_eq!(residual.count, 2);
        assert_eq!(residual.bytes, 30);
        assert!(residual.targets.iter().any(|t| t.path.ends_with("/com.gone.obb") && t.bytes == 0));

        let cache = cat(&snapshot, APP_CACHE);
        assert_eq!(cache.targets.len(), 1);
        assert_eq!(cache.targets[0].path, "/sdcard/Android/data/com.live/cache");
        assert_eq!(cache.bytes, 20);

        let ads = cat(&snapshot, AD_JUNK);
        assert!(ads.targets.iter().any(|t| t.path.ends_with("/vungle")));
        assert!(ads.targets.iter().any(|t| t.path.ends_with("/UnityAdsCache")));
        assert!(ads.targets.iter().any(|t| t.path.ends_with("/.mbridge700")));
        assert_eq!(ads.bytes, 19);

        let apks = cat(&snapshot, APK);
        assert_eq!(apks.targets[0].path, "/sdcard/Download/app.apk");
        assert_eq!(cat(&snapshot, SYSTEM_CACHE).count, 0);
        assert!(!cat(&snapshot, SYSTEM_CACHE).estimated);

        assert_eq!(snapshot.large_files.len(), 1);
        assert_eq!(snapshot.large_files[0].path, "/sdcard/Download/video.mp4");
    }

    #[test]
    fn does_not_count_private_or_active_apk_paths() {
        let snapshot = classify(&input(
            &["com.live"],
            &[],
            &[],
            &[
                ("/data/app/com.live/base.apk", 500),
                ("/sdcard/../etc/passwd", 500),
                ("/sdcard/Android/data/com.live/cache/log.txt", 4),
            ],
        ));
        assert_eq!(cat(&snapshot, APK).count, 0);
        assert_eq!(cat(&snapshot, APP_CACHE).bytes, 4);
        assert!(snapshot.large_files.is_empty());
    }

    #[test]
    fn selected_bytes_drop_a_large_file_inside_a_selected_directory() {
        let snapshot = classify(&input(
            &[],
            &["com.gone"],
            &[],
            &[("/sdcard/Android/data/com.gone/movie.mp4", LARGE_FILE_MIN_BYTES)],
        ));
        let ops = plan(
            &snapshot,
            &[RESIDUAL.into()],
            &["/sdcard/Android/data/com.gone/movie.mp4".into()],
        )
        .unwrap();
        assert!(matches!(
            ops.as_slice(),
            [DeleteOp::Remove { path, bytes }]
                if path == "/sdcard/Android/data/com.gone" && *bytes == LARGE_FILE_MIN_BYTES
        ));
    }

    #[test]
    fn rejects_paths_outside_the_scan() {
        let snapshot = classify(&input(&[], &[], &[], &[]));
        let err = plan(&snapshot, &[], &["/sdcard/Download/other.mp4".into()]).unwrap_err();
        assert!(err.contains("non presente"));
        assert!(validate_deletable("/sdcard/Android/data").is_err());
        assert!(validate_deletable("/sdcard/foo/../../data").is_err());
        assert!(validate_deletable("/sdcard/ok\nfile").is_err());
    }

    #[test]
    fn quotes_and_parses_shell_output() {
        assert_eq!(shell_quote("a'b"), "'a'\\''b'");
        let parsed = parse_scan_output(
            "package:com.live\n---END PACKAGES---\ncom.gone\n---END DATA---\n---END OBB---\n12\t/sdcard/a.apk\n12\t/sdcard/a.apk\n---END FILES---\n",
        );
        assert!(parsed.installed.contains("com.live"));
        assert_eq!(parsed.data_dirs, vec!["com.gone".to_string()]);
        assert_eq!(parsed.files.len(), 1);
        let caches = parse_scan_output(
            "---END FILES---\ncacheSize=100\ncacheSize=0\n    cacheSize=50\n---END CACHESIZE---\n4\t/cache\n8\t/data/user/0/com.foo/cache\n0\t/data/cache\n---END CACHE---\n",
        );
        assert_eq!(caches.system_cache, vec![100, 50, 4 * 1024]);
        let only_du = parse_scan_output(
            "---END FILES---\n---END CACHESIZE---\n3 /data/user/0/com.foo/cache\n---END CACHE---\n",
        );
        assert_eq!(only_du.system_cache, vec![3 * 1024]);
        let disk = parse_scan_output(
            "---END FILES---\nApp Cache Size: 700\ncacheSize=50\n---END CACHESIZE---\n4\t/cache\n---END CACHE---\n",
        );
        assert_eq!(disk.system_cache, vec![700, 4 * 1024]);
    }

    #[test]
    fn scanned_path_reads_stat_lines_and_dir_names() {
        assert_eq!(scanned_path("12\t/sdcard/a b", None).as_deref(), Some("/sdcard/a b"));
        assert_eq!(scanned_path("---END DATA---", Some("/sdcard/Android/data/")), None);
        assert_eq!(
            scanned_path("com.foo", Some("/sdcard/Android/data/")).as_deref(),
            Some("/sdcard/Android/data/com.foo")
        );
        assert_eq!(scanned_path("package:com.foo", None), None);
    }

    #[test]
    fn execute_counts_only_successful_deletes() {
        let ops = vec![
            DeleteOp::TrimCaches,
            DeleteOp::Remove {
                path: "/sdcard/Download/a.apk".into(),
                bytes: 10,
            },
            DeleteOp::Remove {
                path: "/sdcard/Download/b.apk".into(),
                bytes: 5,
            },
        ];
        let mut commands = Vec::new();
        let result = execute(
            &ops,
            |command| {
                commands.push(command.to_string());
                if command.contains("b.apk") {
                    Err("permesso negato".into())
                } else {
                    Ok(String::new())
                }
            },
            |_, _, _, _, _| {},
        );
        assert_eq!(commands[0], TRIM_CACHES);
        assert!(commands[1].starts_with("rm -rf -- "));
        assert!(result.trimmed_system_cache);
        assert_eq!(result.freed_bytes, 10);
        assert_eq!(result.errors.len(), 1);
    }
}
