mod arsc;
mod cache;

pub use cache::{CachedMetadata, MetadataCache, METADATA_CACHE_VERSION};

use adb_bridge::{AdbBridge, PackageInfo};
use arsc::{lookup_all_strings, lookup_string, parse_resource_ref};
use axmldecoder::{Node, parse as parse_manifest};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ScanError {
    #[error(transparent)]
    Adb(#[from] adb_bridge::AdbError),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScannedPackage {
    pub package_name: String,
    pub label: Option<String>,
    pub author: Option<String>,
    pub icon_url: Option<String>,
    pub is_system: bool,
    pub installer: Option<String>,
    pub is_device_admin: bool,
}

#[derive(Debug, Clone)]
pub enum ScanProgress {
    Started { total: usize },
    Package(ScannedPackage),
}

#[derive(Debug, Clone, Default)]
struct PackageDumpInfo {
    installer: Option<String>,
    signing_org: Option<String>,
}

enum LabelValue {
    Literal(String),
    Reference(u32),
}

struct AppMeta {
    label: Option<LabelValue>,
    icon_res: Option<u32>,
}

/// Enrichment is I/O-bound (adb round-trips), so the pool is larger than the
/// CPU count; it also caps concurrent adb commands.
const SCAN_THREADS: usize = 16;

pub struct PackageScanner {
    bridge: Arc<AdbBridge>,
    cache_path: Option<PathBuf>,
}

impl PackageScanner {
    pub fn new(bridge: AdbBridge) -> Self {
        Self {
            bridge: Arc::new(bridge),
            cache_path: None,
        }
    }

    /// Enable the persistent metadata cache backed by the given JSON file.
    pub fn with_cache_path(mut self, path: PathBuf) -> Self {
        self.cache_path = Some(path);
        self
    }

    pub fn scan(&self, user_only: bool) -> Result<Vec<ScannedPackage>, ScanError> {
        let mut results = Vec::new();
        self.scan_with_progress(user_only, |event| {
            if let ScanProgress::Package(pkg) = event {
                results.push(pkg);
            }
        })?;
        Ok(results)
    }

    pub fn scan_with_progress<F>(&self, user_only: bool, mut on_progress: F) -> Result<(), ScanError>
    where
        F: FnMut(ScanProgress) + Send + Sync,
    {
        let packages = self.bridge.list_packages(user_only)?;
        let bulk_dumpsys = self.bridge.shell("dumpsys package").unwrap_or_default();
        let dump_info = parse_bulk_dumpsys(&bulk_dumpsys);
        let admins = self.load_admin_packages()?;

        let total = packages.len();
        on_progress(ScanProgress::Started { total });

        let metadata_cache = Arc::new(Mutex::new(MetadataCache::load(self.cache_path.clone())));

        let on_progress = Arc::new(Mutex::new(on_progress));
        let bridge = Arc::clone(&self.bridge);
        let dump_info = Arc::new(dump_info);
        let admins = Arc::new(admins);

        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(SCAN_THREADS)
            .build()
            .expect("failed to build scan thread pool");
        pool.install(|| {
            packages.par_iter().for_each(|pkg| {
                let cached = metadata_cache
                    .lock()
                    .ok()
                    .and_then(|c| c.get(&pkg.package_name, pkg.apk_path.as_deref()).cloned());
                let scanned = match cached {
                    Some(hit) => ScannedPackage {
                        package_name: pkg.package_name.clone(),
                        label: known_label(&pkg.package_name).or(hit.label),
                        author: hit.author,
                        icon_url: hit.icon_url,
                        is_system: pkg.is_system,
                        installer: dump_info
                            .get(&pkg.package_name)
                            .and_then(|i| i.installer.clone()),
                        is_device_admin: admins.contains(&pkg.package_name),
                    },
                    None => {
                        let (scanned, cacheable) =
                            enrich_package(&bridge, pkg, &dump_info, &admins);
                        if pkg.apk_path.is_some() {
                            if let Ok(mut c) = metadata_cache.lock() {
                                c.put(
                                    &pkg.package_name,
                                    CachedMetadata {
                                        apk_path: pkg.apk_path.clone(),
                                        label: scanned.label.clone(),
                                        author: scanned.author.clone(),
                                        icon_url: scanned.icon_url.clone(),
                                        complete: cacheable,
                                        v: METADATA_CACHE_VERSION,
                                    },
                                );
                            }
                        }
                        scanned
                    }
                };
                if let Ok(mut cb) = on_progress.lock() {
                    cb(ScanProgress::Package(scanned));
                }
            });
        });

        if let Ok(c) = metadata_cache.lock() {
            c.save();
        }

        Ok(())
    }

    fn load_admin_packages(&self) -> Result<HashSet<String>, ScanError> {
        let output = self.bridge.shell("dumpsys device_policy").unwrap_or_default();
        let mut admins = HashSet::new();
        for line in output.lines() {
            if line.contains("admin=ComponentInfo{") {
                if let Some(start) = line.find('{') {
                    if let Some(end) = line.find('}') {
                        let inner = &line[start + 1..end];
                        if let Some(slash) = inner.find('/') {
                            let pkg = &inner[..slash];
                            admins.insert(pkg.to_string());
                        }
                    }
                }
            }
        }
        Ok(admins)
    }
}

fn enrich_package(
    bridge: &AdbBridge,
    pkg: &PackageInfo,
    dump_info: &HashMap<String, PackageDumpInfo>,
    admins: &HashSet<String>,
) -> (ScannedPackage, bool) {
    let dumpsys_info = dump_info
        .get(&pkg.package_name)
        .cloned()
        .unwrap_or_default();
    let installer = dumpsys_info.installer.clone();
    let author = dumpsys_info.signing_org.clone();

    let manifest_bytes =
        fetch_apk_entry(bridge, &pkg.package_name, pkg.apk_path.as_deref(), "AndroidManifest.xml");
    let meta = manifest_bytes
        .as_deref()
        .map(parse_application_meta)
        .unwrap_or(AppMeta {
            label: None,
            icon_res: None,
        });

    let needs_arsc = matches!(meta.label, Some(LabelValue::Reference(_))) || meta.icon_res.is_some();
    let arsc_bytes = if needs_arsc {
        fetch_apk_entry(
            bridge,
            &pkg.package_name,
            pkg.apk_path.as_deref(),
            "resources.arsc",
        )
    } else {
        None
    };

    let apk_label = resolve_apk_label(&meta, arsc_bytes.as_deref());
    let label = apk_label
        .clone()
        .or_else(|| known_label(&pkg.package_name))
        .or_else(|| infer_label_from_package(&pkg.package_name));
    let icon_url = resolve_icon(
        bridge,
        &pkg.package_name,
        pkg.apk_path.as_deref(),
        &meta,
        arsc_bytes.as_deref(),
    );
    let cacheable =
        apk_label.is_some() || known_label(&pkg.package_name).is_some() || icon_url.is_some();

    (
        ScannedPackage {
            package_name: pkg.package_name.clone(),
            label,
            author,
            icon_url,
            is_system: pkg.is_system,
            installer,
            is_device_admin: admins.contains(&pkg.package_name),
        },
        cacheable,
    )
}

fn resolve_apk_label(meta: &AppMeta, arsc: Option<&[u8]>) -> Option<String> {
    match &meta.label {
        Some(LabelValue::Literal(s)) => normalize_label(s).filter(|l| !is_weak_label(l)),
        Some(LabelValue::Reference(id)) => arsc
            .and_then(|data| lookup_string(data, *id))
            .and_then(|s| normalize_label(&s))
            .filter(|s| !is_weak_label(s)),
        None => None,
    }
}

#[cfg(test)]
fn resolve_label(meta: &AppMeta, arsc: Option<&[u8]>, package_name: &str) -> Option<String> {
    resolve_apk_label(meta, arsc)
        .or_else(|| known_label(package_name))
        .or_else(|| infer_label_from_package(package_name))
}

fn resolve_icon(
    bridge: &AdbBridge,
    package_name: &str,
    apk_path: Option<&str>,
    meta: &AppMeta,
    arsc: Option<&[u8]>,
) -> Option<String> {
    let apk_paths = candidate_apk_paths(apk_path);
    let paths = meta
        .icon_res
        .and_then(|id| arsc.map(|data| lookup_all_strings(data, id)))
        .unwrap_or_default();
    let mut paths = paths;
    if pick_icon_path(&paths).is_none() {
        extend_paths_from_xml(
            bridge,
            package_name,
            apk_path,
            &apk_paths,
            arsc,
            &[],
            &mut paths,
        );
    }
    if pick_icon_path(&paths).is_none() {
        let extra_arsc = load_split_arscs(bridge, package_name, &apk_paths);
        if let Some(id) = meta.icon_res {
            for table in &extra_arsc {
                for s in lookup_all_strings(table, id) {
                    if !paths.contains(&s) {
                        paths.push(s);
                    }
                }
            }
        }
        extend_paths_from_xml(
            bridge,
            package_name,
            apk_path,
            &apk_paths,
            arsc,
            &extra_arsc,
            &mut paths,
        );
    }
    let picked = pick_icon_path(&paths);
    let mut url = picked
        .as_ref()
        .and_then(|path| fetch_icon_data_url(bridge, package_name, &apk_paths, path));
    if url.is_none() {
        for fallback in FALLBACK_LAUNCHER_ENTRIES {
            if paths.iter().any(|p| p == fallback) {
                continue;
            }
            if let Some(found) = fetch_icon_data_url(bridge, package_name, &apk_paths, fallback) {
                url = Some(found);
                break;
            }
        }
    }
    url
}

fn load_split_arscs(
    bridge: &AdbBridge,
    package_name: &str,
    apk_paths: &[String],
) -> Vec<Vec<u8>> {
    apk_paths
        .iter()
        .skip(1)
        .filter_map(|path| {
            let bytes = fetch_apk_entry(bridge, package_name, Some(path), "resources.arsc")?;
            (bytes.len() > 256).then_some(bytes)
        })
        .collect()
}

fn extend_paths_from_xml(
    bridge: &AdbBridge,
    package_name: &str,
    apk_path: Option<&str>,
    apk_paths: &[String],
    base_arsc: Option<&[u8]>,
    extra_arsc: &[Vec<u8>],
    paths: &mut Vec<String>,
) {
    let mut seen = HashSet::new();
    for _ in 0..4 {
        let xml_paths: Vec<String> = paths
            .iter()
            .filter(|p| p.ends_with(".xml") && seen.insert((*p).clone()))
            .cloned()
            .collect();
        if xml_paths.is_empty() {
            break;
        }
        for xml_path in xml_paths {
            let Some(xml) = fetch_apk_entry(bridge, package_name, apk_path, &xml_path).or_else(|| {
                apk_paths
                    .iter()
                    .find_map(|p| fetch_apk_entry(bridge, package_name, Some(p), &xml_path))
            }) else {
                continue;
            };
            for resid in xml_drawable_refs(&xml) {
                if let Some(base) = base_arsc {
                    for s in lookup_all_strings(base, resid) {
                        if !paths.contains(&s) {
                            paths.push(s);
                        }
                    }
                }
                for table in extra_arsc {
                    for s in lookup_all_strings(table, resid) {
                        if !paths.contains(&s) {
                            paths.push(s);
                        }
                    }
                }
            }
        }
    }
}

fn xml_drawable_refs(xml: &[u8]) -> Vec<u32> {
    let Some(doc) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| parse_manifest(xml)))
        .ok()
        .and_then(Result::ok)
    else {
        return Vec::new();
    };
    let Some(root) = doc.get_root() else {
        return Vec::new();
    };
    let mut foreground = Vec::new();
    let mut other = Vec::new();
    collect_xml_drawables(root, false, &mut foreground, &mut other);
    if foreground.is_empty() {
        other
    } else {
        foreground
    }
}

fn collect_xml_drawables(
    node: &Node,
    in_foreground: bool,
    foreground: &mut Vec<u32>,
    other: &mut Vec<u32>,
) {
    let Node::Element(el) = node else {
        return;
    };
    let tag = el.get_tag();
    let now_fg = in_foreground || tag == "foreground";
    for (key, value) in el.get_attributes() {
        if key == "android:drawable" || key == "drawable" || key.ends_with(":drawable") {
            if let Some(id) = parse_resource_ref(value) {
                if id >> 24 == 0x7f && !foreground.contains(&id) && !other.contains(&id) {
                    if now_fg {
                        foreground.push(id);
                    } else {
                        other.push(id);
                    }
                }
            }
        }
    }
    for child in el.get_children() {
        collect_xml_drawables(child, now_fg, foreground, other);
    }
}

const FALLBACK_LAUNCHER_ENTRIES: &[&str] = &[
    "res/mipmap-xxhdpi-v4/ic_launcher.png",
    "res/mipmap-xxxhdpi-v4/ic_launcher.png",
    "res/mipmap-xxhdpi-v4/ic_launcher.webp",
    "res/mipmap-xxxhdpi-v4/ic_launcher.webp",
    "res/mipmap-xhdpi-v4/ic_launcher.png",
    "res/mipmap-hdpi-v4/ic_launcher.png",
];

fn fetch_icon_data_url(
    bridge: &AdbBridge,
    package_name: &str,
    apk_paths: &[String],
    entry: &str,
) -> Option<String> {
    if apk_paths.is_empty() {
        let bytes = fetch_apk_entry(bridge, package_name, None, entry)?;
        return is_image_bytes(&bytes).then(|| icon_bytes_to_data_url(&bytes));
    }
    for path in apk_paths {
        if let Some(bytes) = fetch_apk_entry(bridge, package_name, Some(path), entry) {
            if is_image_bytes(&bytes) {
                return Some(icon_bytes_to_data_url(&bytes));
            }
        }
    }
    None
}

fn candidate_apk_paths(apk_path: Option<&str>) -> Vec<String> {
    let Some(base) = apk_path else {
        return Vec::new();
    };
    let mut paths = vec![base.to_string()];
    if let Some((dir, file)) = base.rsplit_once('/') {
        if file == "base.apk" {
            for split in [
                "split_config.xxxhdpi.apk",
                "split_config.xxhdpi.apk",
                "split_config.xhdpi.apk",
            ] {
                paths.push(format!("{dir}/{split}"));
            }
        }
    }
    paths
}

fn pick_icon_path(paths: &[String]) -> Option<String> {
    paths
        .iter()
        .filter(|p| is_apk_image_path(p))
        .max_by_key(|p| icon_density_score(p))
        .cloned()
}

fn icon_density_score(path: &str) -> i32 {
    let p = path.to_ascii_lowercase();
    let mut score = 1;
    if p.contains("xxxhdpi") {
        score += 40;
    } else if p.contains("xxhdpi") {
        score += 30;
    } else if p.contains("xhdpi") {
        score += 20;
    } else if p.contains("hdpi") {
        score += 10;
    }
    if p.ends_with(".png") {
        score += 1;
    }
    if p.contains("1x1") {
        score -= 50;
    }
    score
}

fn normalize_label(raw: &str) -> Option<String> {
    let s = raw
        .replace('\u{a0}', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    (!s.is_empty()).then_some(s)
}

fn is_apk_image_path(path: &str) -> bool {
    let p = path.trim();
    if !(p.starts_with("res/") || p.starts_with("/res/")) || p.contains("..") {
        return false;
    }
    let name = p.rsplit('/').next().unwrap_or("");
    if name.is_empty() || name.ends_with(".xml") {
        return false;
    }
    name.ends_with(".png") || name.ends_with(".webp") || !name.contains('.')
}

fn parse_application_meta(manifest: &[u8]) -> AppMeta {
    let Ok(doc) = parse_manifest(manifest) else {
        return AppMeta {
            label: None,
            icon_res: None,
        };
    };
    let Some(root) = doc.get_root().as_ref() else {
        return AppMeta {
            label: None,
            icon_res: None,
        };
    };
    find_application_meta(root).unwrap_or(AppMeta {
        label: None,
        icon_res: None,
    })
}

fn find_application_meta(node: &Node) -> Option<AppMeta> {
    match node {
        Node::Element(el) => {
            if el.get_tag() == "application" {
                let attrs = el.get_attributes();
                let label = attrs
                    .get("android:label")
                    .or_else(|| attrs.get("label"))
                    .and_then(|v| parse_label_value(v));
                let icon_res = attrs
                    .get("android:icon")
                    .or_else(|| attrs.get("icon"))
                    .and_then(|v| parse_resource_ref(v));
                return Some(AppMeta { label, icon_res });
            }
            for child in el.get_children() {
                if let Some(meta) = find_application_meta(child) {
                    return Some(meta);
                }
            }
            None
        }
        _ => None,
    }
}

fn parse_label_value(raw: &str) -> Option<LabelValue> {
    if let Some(id) = parse_resource_ref(raw) {
        return Some(LabelValue::Reference(id));
    }
    let value = raw.trim();
    if value.is_empty()
        || value.starts_with("ResourceValueType::")
        || value.starts_with('@')
    {
        return None;
    }
    Some(LabelValue::Literal(value.to_string()))
}

fn parse_bulk_dumpsys(output: &str) -> HashMap<String, PackageDumpInfo> {
    let mut map = HashMap::new();
    let mut current_pkg: Option<String> = None;

    for line in output.lines() {
        if let Some(pkg) = parse_package_header(line) {
            current_pkg = Some(pkg);
            continue;
        }
        if let Some(ref pkg) = current_pkg {
            let entry: &mut PackageDumpInfo = map.entry(pkg.clone()).or_default();
            if entry.installer.is_none() {
                if let Some(inst) = extract_installer_from_line(line) {
                    entry.installer = Some(inst);
                }
            }
            if line.contains("certificate DN:") || line.contains("Signer #") {
                if let Some(org) = extract_signing_from_line(line) {
                    if !is_generic_org(&org) {
                        entry.signing_org = Some(org);
                    }
                }
            }
        }
    }

    map
}

fn parse_package_header(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if !trimmed.starts_with("Package [") {
        return None;
    }
    let start = trimmed.find('[')? + 1;
    let end = trimmed[start..].find(']')? + start;
    let pkg = trimmed[start..end].trim();
    if pkg.is_empty() {
        None
    } else {
        Some(pkg.to_string())
    }
}

fn extract_installer_from_line(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    if !trimmed.starts_with("installerPackageName=") {
        return None;
    }
    trimmed
        .split('=')
        .nth(1)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty() && s != "null")
}

fn extract_signing_from_line(line: &str) -> Option<String> {
    extract_dn_field(line, "O=")
}

/// Shell snippet that leaves the APK path in `$APK`: uses the known path when
/// available, otherwise resolves it on-device via `pm path`.
fn apk_path_prelude(package_name: &str, apk_path: Option<&str>) -> String {
    match apk_path {
        Some(p) => format!("APK=\"{p}\""),
        None => format!(
            "APK=$(pm path {package_name} 2>/dev/null | head -1 | cut -d: -f2 | tr -d '\\r')"
        ),
    }
}

fn fetch_apk_entry(
    bridge: &AdbBridge,
    package_name: &str,
    apk_path: Option<&str>,
    entry: &str,
) -> Option<Vec<u8>> {
    let safe_entry = entry.replace('"', "");
    if safe_entry.is_empty() || safe_entry.contains("..") {
        return None;
    }
    let cmd = format!(
        "{}; [ -n \"$APK\" ] && unzip -p \"$APK\" \"{safe_entry}\"",
        apk_path_prelude(package_name, apk_path)
    );
    let bytes = bridge.exec_out(&cmd).ok()?;
    is_plausible_apk_entry(&bytes).then_some(bytes)
}

fn is_plausible_apk_entry(bytes: &[u8]) -> bool {
    !bytes.is_empty() && !bytes.starts_with(b"unzip:")
}

fn is_image_bytes(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\x89PNG")
        || (bytes.starts_with(b"RIFF") && bytes.len() > 12 && bytes[8..12] == *b"WEBP")
}

fn icon_bytes_to_data_url(bytes: &[u8]) -> String {
    let mime = if bytes.starts_with(b"\x89PNG") {
        "image/png"
    } else {
        "image/webp"
    };
    format!("data:{mime};base64,{}", STANDARD.encode(bytes))
}

fn is_weak_label(label: &str) -> bool {
    let trimmed = label.trim();
    if trimmed.contains('.') || trimmed.contains(':') || trimmed.contains('/') {
        return true;
    }
    matches!(
        trimmed,
        "Google" | "Android" | "google" | "android" | "Theme"
    )
}

fn known_label(package_name: &str) -> Option<String> {
    match package_name {
        "com.google.android.gm" => Some("Gmail".into()),
        _ => None,
    }
}

fn infer_label_from_package(package_name: &str) -> Option<String> {
    if let Some(label) = known_label(package_name) {
        return Some(label);
    }
    const SKIP: &[&str] = &[
        "com", "org", "net", "android", "app", "mobile", "client", "ipn", "mshop", "shopping",
    ];
    for segment in package_name.split('.').rev() {
        if SKIP.contains(&segment.to_lowercase().as_str()) || segment.len() < 3 {
            continue;
        }
        return Some(title_case_segment(segment));
    }
    None
}

fn title_case_segment(segment: &str) -> String {
    if segment.eq_ignore_ascii_case("chatgpt") {
        return "ChatGPT".into();
    }
    let mut chars = segment.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
    }
}

#[cfg(test)]
fn extract_signing_organization(text: &str) -> Option<String> {
    for line in text.lines() {
        if line.contains("certificate DN:") || line.contains("Signer #") {
            if let Some(org) = extract_dn_field(line, "O=") {
                if !is_generic_org(&org) {
                    return Some(org);
                }
            }
        }
    }
    None
}

fn extract_dn_field(line: &str, key: &str) -> Option<String> {
    let idx = line.find(key)?;
    let rest = &line[idx + key.len()..];
    let end = rest.find(',').unwrap_or(rest.len());
    let value = rest[..end].trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

fn is_generic_org(org: &str) -> bool {
    matches!(
        org,
        "Android" | "Android Debug" | "unknown" | "Unknown" | "CN" | "Google Inc."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_signing_organization() {
        let dumpsys = "    Signer #1 certificate DN: CN=App, O=Meta Platforms Inc., C=US";
        assert_eq!(
            extract_signing_organization(dumpsys).as_deref(),
            Some("Meta Platforms Inc.")
        );
    }

    #[test]
    fn parses_bulk_dumpsys_sections() {
        let output = r#"
Package [com.example.app] (abc):
    installerPackageName=com.android.vending
    Signer #1 certificate DN: CN=App, O=Example Corp, C=US
Package [com.other.app] (def):
    installerPackageName=null
"#;
        let map = parse_bulk_dumpsys(output);
        assert_eq!(
            map.get("com.example.app").and_then(|i| i.installer.as_deref()),
            Some("com.android.vending")
        );
        assert_eq!(
            map.get("com.example.app")
                .and_then(|i| i.signing_org.as_deref()),
            Some("Example Corp")
        );
    }

    #[test]
    fn parse_label_value_treats_resource_as_reference() {
        match parse_label_value("ResourceValueType::Reference/2131755036") {
            Some(LabelValue::Reference(id)) => assert_eq!(id, 0x7f10001c),
            _ => panic!("expected resource reference"),
        }
    }

    #[test]
    fn parse_label_value_keeps_literal() {
        match parse_label_value("ChatGPT") {
            Some(LabelValue::Literal(s)) => assert_eq!(s, "ChatGPT"),
            _ => panic!("expected literal"),
        }
        assert!(parse_label_value("ResourceValueType::String").is_none());
    }

    #[test]
    fn resolve_label_uses_manifest_literal() {
        let meta = AppMeta {
            label: Some(LabelValue::Literal("HTML Viewer".into())),
            icon_res: None,
        };
        assert_eq!(
            resolve_label(&meta, None, "com.android.htmlviewer").as_deref(),
            Some("HTML Viewer")
        );
    }

    #[test]
    fn resolve_label_skips_weak_literal() {
        let meta = AppMeta {
            label: Some(LabelValue::Literal("Google".into())),
            icon_res: None,
        };
        assert_eq!(
            resolve_label(&meta, None, "com.google.android.gm").as_deref(),
            Some("Gmail")
        );
    }

    #[test]
    fn is_apk_image_path_accepts_png_under_res() {
        assert!(is_apk_image_path("res/mipmap-xxhdpi/ic_launcher.png"));
        assert!(is_apk_image_path("res/raw/btD"));
        assert!(!is_apk_image_path("res/raw/btt.xml"));
        assert!(!is_apk_image_path("res/mipmap-anydpi-v26/ic_launcher.xml"));
        assert!(!is_apk_image_path("whatsapp_icon.png"));
        assert!(!is_apk_image_path("res/../ic.png"));
    }

    #[test]
    fn known_label_names_gmail() {
        assert_eq!(
            known_label("com.google.android.gm").as_deref(),
            Some("Gmail")
        );
        assert!(known_label("com.google.android.gms").is_none());
    }

    #[test]
    fn infer_label_does_not_turn_gmail_into_google() {
        assert_eq!(
            infer_label_from_package("com.google.android.gm").as_deref(),
            Some("Gmail")
        );
    }

    #[test]
    fn rejects_weak_arsc_labels() {
        assert!(is_weak_label("Theme.AppCompat"));
        assert!(is_weak_label("google.com:youtube-android"));
        assert!(is_weak_label("Google"));
        assert!(is_weak_label("Android"));
        assert!(!is_weak_label("YouTube Music"));
        assert!(!is_weak_label("Gmail"));
    }

    #[test]
    fn does_not_use_google_play_as_author() {
        let dumpsys = "installerPackageName=com.android.vending";
        assert!(extract_signing_organization(dumpsys).is_none());
    }

    #[test]
    fn normalize_label_replaces_nbsp() {
        assert_eq!(
            normalize_label("WhatsApp\u{a0}Business").as_deref(),
            Some("WhatsApp Business")
        );
    }

    #[test]
    fn pick_icon_path_prefers_png_over_xml() {
        let paths = [
            "res/mipmap-anydpi-v26/ic_launcher.xml".into(),
            "res/mipmap-xxhdpi-v4/ic_launcher.png".into(),
            "res/lbs.png".into(),
        ];
        assert_eq!(
            pick_icon_path(&paths).as_deref(),
            Some("res/mipmap-xxhdpi-v4/ic_launcher.png")
        );
    }

    #[test]
    fn pick_icon_path_skips_xml_when_names_are_obfuscated() {
        let paths = [
            "res/fHq.png".into(),
            "res/ilG.png".into(),
            "res/BWP.xml".into(),
        ];
        assert_eq!(pick_icon_path(&paths).as_deref(), Some("res/ilG.png"));
    }

    #[test]
    fn pick_icon_path_accepts_extensionless_webp_in_res_raw() {
        let paths = [
            "res/raw/bto".into(),
            "res/raw/btD".into(),
            "res/raw/btt.xml".into(),
        ];
        assert_eq!(pick_icon_path(&paths).as_deref(), Some("res/raw/btD"));
    }

    #[test]
    fn rejects_unzip_error_as_apk_entry() {
        assert!(!is_plausible_apk_entry(b""));
        assert!(!is_plausible_apk_entry(
            b"unzip: couldn't open /data/app/x/split_config.xxxhdpi.apk: I/O error\n"
        ));
        assert!(is_plausible_apk_entry(&[0x03, 0x00, 0x08, 0x00, 0x10]));
    }

    #[test]
    fn pick_icon_path_skips_1x1_placeholder() {
        let paths = ["res/1x1.png".into(), "res/kp.png".into()];
        assert_eq!(pick_icon_path(&paths).as_deref(), Some("res/kp.png"));
    }

    #[test]
    fn xml_drawable_refs_reads_adaptive_foreground() {
        let xml = include_bytes!("testdata/camera_adaptive.bin");
        assert_eq!(xml_drawable_refs(xml), vec![2131231199]);
    }

    #[test]
    fn candidate_apk_paths_includes_density_splits() {
        let paths = candidate_apk_paths(Some(
            "/data/app/~~x/com.whatsapp.w4b-y/base.apk",
        ));
        assert_eq!(paths[0], "/data/app/~~x/com.whatsapp.w4b-y/base.apk");
        assert!(paths.iter().any(|p| p.ends_with("split_config.xxhdpi.apk")));
    }
}
