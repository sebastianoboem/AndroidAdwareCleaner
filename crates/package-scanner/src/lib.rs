use adb_bridge::{AdbBridge, PackageInfo};
use axmldecoder::{Node, parse as parse_manifest};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;
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

#[derive(Debug, Clone, Default)]
struct PlayMetadata {
    title: Option<String>,
    developer: Option<String>,
    icon_url: Option<String>,
}

pub struct PackageScanner {
    bridge: AdbBridge,
    play_cache: Mutex<HashMap<String, PlayMetadata>>,
}

impl PackageScanner {
    pub fn new(bridge: AdbBridge) -> Self {
        Self {
            bridge,
            play_cache: Mutex::new(HashMap::new()),
        }
    }

    pub fn scan(&self, user_only: bool) -> Result<Vec<ScannedPackage>, ScanError> {
        let packages = self.bridge.list_packages(user_only)?;
        let admin_components = self.load_admin_packages()?;

        let mut result = Vec::with_capacity(packages.len());
        for pkg in packages {
            let meta = self.enrich_package(&pkg, &admin_components);
            result.push(meta);
        }
        Ok(result)
    }

    fn load_admin_packages(&self) -> Result<std::collections::HashSet<String>, ScanError> {
        let output = self.bridge.shell("dumpsys device_policy").unwrap_or_default();
        let mut admins = std::collections::HashSet::new();
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

    fn enrich_package(
        &self,
        pkg: &PackageInfo,
        admins: &std::collections::HashSet<String>,
    ) -> ScannedPackage {
        let dumpsys = self
            .bridge
            .shell(&format!("dumpsys package {}", pkg.package_name))
            .unwrap_or_default();

        let installer = extract_installer(&dumpsys);
        let manifest_label = fetch_manifest_bytes(&self.bridge, &pkg.package_name)
            .as_deref()
            .and_then(extract_label_from_manifest);
        let arsc_strings = fetch_arsc_strings(&self.bridge, &pkg.package_name);
        let play = if pkg.is_system {
            PlayMetadata::default()
        } else {
            self.play_metadata(&pkg.package_name)
        };

        let label = manifest_label
            .or(play.title.clone())
            .or_else(|| extract_label_from_arsc(&arsc_strings, &pkg.package_name))
            .or_else(|| infer_label_from_package(&pkg.package_name));

        let author = if pkg.is_system {
            extract_signing_organization(&dumpsys)
        } else {
            play.developer
                .clone()
                .or_else(|| extract_signing_organization(&dumpsys))
        };

        let icon_url = play
            .icon_url
            .or_else(|| fetch_apk_icon_data_url(&self.bridge, &pkg.package_name));

        ScannedPackage {
            package_name: pkg.package_name.clone(),
            label,
            author,
            icon_url,
            is_system: pkg.is_system,
            installer,
            is_device_admin: admins.contains(&pkg.package_name),
        }
    }

    fn play_metadata(&self, package_name: &str) -> PlayMetadata {
        if let Some(cached) = self.play_cache.lock().unwrap().get(package_name) {
            return cached.clone();
        }
        let meta = fetch_play_metadata(package_name).unwrap_or_default();
        self.play_cache
            .lock()
            .unwrap()
            .insert(package_name.to_string(), meta.clone());
        meta
    }
}

fn fetch_manifest_bytes(bridge: &AdbBridge, package_name: &str) -> Option<Vec<u8>> {
    let cmd = format!(
        "APK=$(pm path {package_name} 2>/dev/null | head -1 | cut -d: -f2 | tr -d '\\r'); [ -n \"$APK\" ] && unzip -p \"$APK\" AndroidManifest.xml"
    );
    let bytes = bridge.exec_out(&cmd).ok()?;
    if bytes.len() < 8 {
        None
    } else {
        Some(bytes)
    }
}

fn extract_label_from_manifest(manifest: &[u8]) -> Option<String> {
    let doc = parse_manifest(manifest).ok()?;
    let root = doc.get_root().as_ref()?;
    find_application_label(root)
}

fn find_application_label(node: &Node) -> Option<String> {
    match node {
        Node::Element(el) => {
            if el.get_tag() == "application" {
                return el
                    .get_attributes()
                    .get("android:label")
                    .or_else(|| el.get_attributes().get("label"))
                    .and_then(|value| parse_manifest_label_value(value));
            }
            for child in el.get_children() {
                if let Some(label) = find_application_label(child) {
                    return Some(label);
                }
            }
            None
        }
        _ => None,
    }
}

fn parse_manifest_label_value(raw: &str) -> Option<String> {
    let value = raw.trim();
    if value.is_empty() {
        return None;
    }
    if value.starts_with("ResourceValueType::")
        || value.starts_with('@')
        || value.contains("Reference/")
    {
        return None;
    }
    Some(value.to_string())
}

fn fetch_arsc_strings(bridge: &AdbBridge, package_name: &str) -> String {
    let cmd = format!(
        "APK=$(pm path {package_name} 2>/dev/null | head -1 | cut -d: -f2 | tr -d '\\r'); [ -n \"$APK\" ] && unzip -p \"$APK\" resources.arsc 2>/dev/null | strings"
    );
    bridge.shell(&cmd).unwrap_or_default()
}

fn fetch_play_metadata(package_name: &str) -> Option<PlayMetadata> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(8))
        .user_agent("Mozilla/5.0 (compatible; AndroidAdwareCleaner/0.1)")
        .build()
        .ok()?;
    let url = format!(
        "https://play.google.com/store/apps/details?id={package_name}&hl=en"
    );
    let html = client.get(&url).send().ok()?.text().ok()?;
    if html.contains("We're sorry, the requested URL was not found") {
        return None;
    }

    let title = extract_play_title(&html);
    let developer = extract_play_developer(&html);
    let icon_url = extract_play_icon(&html);
    if title.is_none() && developer.is_none() && icon_url.is_none() {
        return None;
    }
    Some(PlayMetadata {
        title,
        developer,
        icon_url,
    })
}

fn extract_play_title(html: &str) -> Option<String> {
    let marker = "itemprop=\"name\">";
    let rest = html.find(marker)?;
    let value = html[rest + marker.len()..].split('<').next()?.trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn extract_play_developer(html: &str) -> Option<String> {
    let marker = "href=\"/store/apps/dev";
    let start = html.find(marker)?;
    let rest = &html[start..];
    let span_start = rest.find("<span>")? + "<span>".len();
    let value = rest[span_start..].split('<').next()?.trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn extract_play_icon(html: &str) -> Option<String> {
    for marker in [
        "property=\"og:image\" content=\"",
        "property='og:image' content='",
        "itemprop=\"image\" content=\"",
    ] {
        if let Some(start) = html.find(marker) {
            let rest = &html[start + marker.len()..];
            let url = rest.split(['"', '\'']).next()?.trim();
            if url.starts_with("https://") {
                return Some(url.to_string());
            }
        }
    }

    let marker = "https://play-lh.googleusercontent.com/";
    let start = html.find(marker)?;
    let rest = &html[start..];
    let end = rest.find(['"', '\'', ' ', '<']).unwrap_or(rest.len());
    let url = rest[..end].trim();
    (!url.is_empty()).then(|| url.to_string())
}

fn fetch_apk_icon_data_url(bridge: &AdbBridge, package_name: &str) -> Option<String> {
    let cmd = format!(
        r#"APK=$(pm path {package_name} 2>/dev/null | head -1 | cut -d: -f2 | tr -d '\r'); \
if [ -z "$APK" ]; then exit 1; fi; \
ICON=$(unzip -l "$APK" 2>/dev/null | awk '/res\/mipmap/ && /\.(png|webp)$/ && /ic_launcher|launcher_icon|app_icon|icon_round/ {{print $4}}' | sort -r | head -1); \
if [ -z "$ICON" ]; then \
  ICON=$(unzip -l "$APK" 2>/dev/null | awk '/res\/mipmap-xxhdpi/ && /\.(png|webp)$/ {{print $4}}' | head -1); \
fi; \
if [ -z "$ICON" ]; then exit 1; fi; \
unzip -p "$APK" "$ICON" 2>/dev/null"#
    );
    let bytes = bridge.exec_out(&cmd).ok()?;
    if !is_image_bytes(&bytes) {
        return None;
    }
    Some(icon_bytes_to_data_url(&bytes))
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

fn extract_label_from_arsc(strings: &str, package_name: &str) -> Option<String> {
    let mut freq: HashMap<String, usize> = HashMap::new();
    for line in strings.lines() {
        let normalized = normalize_label_candidate(line);
        if is_label_candidate(&normalized, package_name) {
            *freq.entry(normalized).or_default() += 1;
        }
    }

    freq.into_iter()
        .max_by(|a, b| {
            score_label(&a.0, package_name)
                .cmp(&score_label(&b.0, package_name))
                .then_with(|| a.1.cmp(&b.1))
        })
        .map(|(label, _)| label)
}

fn normalize_label_candidate(s: &str) -> String {
    let s = s.trim();
    if let Some(idx) = s.to_lowercase().find(" to ") {
        let after = s[idx + 4..].trim();
        if !after.is_empty()
            && after.split_whitespace().count() <= 3
            && after.chars().next().is_some_and(|c| c.is_uppercase())
        {
            return after.to_string();
        }
    }
    s.to_string()
}

fn is_label_candidate(s: &str, package_name: &str) -> bool {
    let len = s.chars().count();
    if len < 2 || len > 32 {
        return false;
    }
    if !s.is_char_boundary(0) {
        return false;
    }
    let lower = s.to_lowercase();
    if lower.contains("http")
        || lower.contains("%1$s")
        || lower.contains("%d")
        || lower.contains("please ")
        || lower.contains("error")
        || lower.contains("version")
    {
        return false;
    }
    if s.split_whitespace().count() > 4 {
        return false;
    }
    if s.chars().any(|c| c.is_control()) {
        return false;
    }
    const BLOCK: &[&str] = &[
        "Settings",
        "Privacy",
        "Learn more",
        "Cancel",
        "OK",
        "Warning",
        "Delete",
        "Version",
        "About",
        "Welcome",
        "more",
        "More",
        "next",
        "Next",
        "back",
        "done",
        "edit",
        "view",
        "open",
        "close",
        "skip",
        "yes",
        "no",
    ];
    if BLOCK.iter().any(|b| s.eq_ignore_ascii_case(b)) {
        return false;
    }
    if s == package_name {
        return false;
    }
    true
}

fn score_label(label: &str, package_name: &str) -> i32 {
    let mut score = 0;
    let words = label.split_whitespace().count();
    if words <= 2 {
        score += 10;
    }
    if words == 1 {
        score += 8;
    }
    if label.chars().next().is_some_and(|c| c.is_uppercase()) {
        score += 6;
    }
    if label.chars().all(|c| c.is_ascii_lowercase()) && label.len() < 6 {
        score -= 12;
    }
    if label.len() <= 16 {
        score += 3;
    }
    for segment in package_name.split('.').filter(|s| s.len() > 2) {
        if label.eq_ignore_ascii_case(segment) {
            score += 12;
        } else if label.to_lowercase().contains(&segment.to_lowercase()) {
            score += 6;
        }
    }
    score
}

fn infer_label_from_package(package_name: &str) -> Option<String> {
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

fn extract_installer(text: &str) -> Option<String> {
    for line in text.lines() {
        if line.trim_start().starts_with("installerPackageName=") {
            return line
                .split('=')
                .nth(1)
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty() && s != "null");
        }
    }
    None
}

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
    fn picks_chatgpt_label_from_arsc() {
        let strings = "more\nmore\nmore\nChatGPT\nChatGPT Plus\nPrivacy\nLearn more\n";
        assert_eq!(
            extract_label_from_arsc(strings, "com.openai.chatgpt").as_deref(),
            Some("ChatGPT")
        );
    }

    #[test]
    fn parses_plain_manifest_label() {
        let manifest = br#"not xml"#;
        assert!(extract_label_from_manifest(manifest).is_none());
    }

    #[test]
    fn ignores_manifest_resource_reference() {
        assert!(parse_manifest_label_value(
            "ResourceValueType::Reference/2132017286"
        )
        .is_none());
        assert_eq!(
            parse_manifest_label_value("ChatGPT").as_deref(),
            Some("ChatGPT")
        );
    }

    #[test]
    fn picks_amazon_label_from_arsc() {
        let strings = "Amazon\nAmazon\nPlease upgrade\nShopping cart\n";
        assert_eq!(
            extract_label_from_arsc(strings, "com.amazon.mShop.android.shopping").as_deref(),
            Some("Amazon")
        );
    }

    #[test]
    fn extracts_play_metadata_from_html() {
        let html = r#"<meta property="og:image" content="https://play-lh.googleusercontent.com/icon.png"><span itemprop="name">ChatGPT</span><a href="/store/apps/dev?id=1"><span>OpenAI</span></a>"#;
        assert_eq!(extract_play_title(html).as_deref(), Some("ChatGPT"));
        assert_eq!(extract_play_developer(html).as_deref(), Some("OpenAI"));
        assert_eq!(
            extract_play_icon(html).as_deref(),
            Some("https://play-lh.googleusercontent.com/icon.png")
        );
    }

    #[test]
    fn does_not_use_google_play_as_author() {
        let dumpsys = "installerPackageName=com.android.vending";
        assert!(extract_signing_organization(dumpsys).is_none());
    }
}
