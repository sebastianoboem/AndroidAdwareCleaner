use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuideStep {
    pub title: String,
    pub body: String,
    pub phase: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceGuide {
    pub brand: String,
    pub models: Vec<String>,
    pub build_number_taps: u32,
    pub developer_menu_path: String,
    pub safe_mode_steps: String,
    pub steps: Vec<GuideStep>,
    #[serde(default)]
    pub tips: Vec<GuideStep>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceGuides {
    pub generic: Vec<GuideStep>,
    #[serde(default)]
    pub generic_tips: Vec<GuideStep>,
    pub brands: Vec<DeviceGuide>,
}

pub fn load_embedded_guides() -> DeviceGuides {
    serde_json::from_str(include_str!("../resources/device-guides.json"))
        .unwrap_or_else(|_| default_guides())
}

fn default_guides() -> DeviceGuides {
    DeviceGuides {
        generic: vec![
            GuideStep {
                title: "Opzioni sviluppatore".into(),
                body: "Impostazioni → Info telefono → 7 tap su Numero build.".into(),
                phase: "developer".into(),
            },
            GuideStep {
                title: "Debug USB".into(),
                body: "Opzioni sviluppatore → abilita Debug USB.".into(),
                phase: "debug".into(),
            },
            GuideStep {
                title: "Collega il cavo USB".into(),
                body: "Cavo dati al PC; sul telefono scegli Trasferimento file / MTP se richiesto.".into(),
                phase: "cable".into(),
            },
            GuideStep {
                title: "Autorizza il PC".into(),
                body: "Sblocca lo schermo e accetta «Consenti debug USB?» (spunta «Consenti sempre»).".into(),
                phase: "authorize".into(),
            },
        ],
        generic_tips: vec![],
        brands: vec![],
    }
}

pub fn find_guide<'a>(guides: &'a DeviceGuides, brand: &str, model: &str) -> Option<&'a DeviceGuide> {
    let brand_lower = brand.to_lowercase();
    let model_lower = model.to_lowercase();

    guides.brands.iter().find(|g| {
        g.brand.to_lowercase() == brand_lower
            && (model_lower.is_empty()
                || g.models.is_empty()
                || g.models
                    .iter()
                    .any(|m| model_lower.contains(&m.to_lowercase())))
    })
}

/// Trova la guida solo dal modello (es. "SM-A536" → Samsung).
pub fn find_guide_by_model<'a>(guides: &'a DeviceGuides, model: &str) -> Option<&'a DeviceGuide> {
    let model_lower = model.trim().to_lowercase();
    if model_lower.is_empty() {
        return None;
    }

    guides.brands.iter().find(|g| {
        g.models
            .iter()
            .any(|m| model_lower.contains(&m.to_lowercase()))
    })
}

/// Risolve marca + modello: se la marca è vuota prova il match sul modello.
pub fn resolve_guide<'a>(
    guides: &'a DeviceGuides,
    brand: &str,
    model: &str,
) -> Option<&'a DeviceGuide> {
    if !brand.trim().is_empty() {
        return find_guide(guides, brand, model).or_else(|| {
            guides
                .brands
                .iter()
                .find(|g| g.brand.eq_ignore_ascii_case(brand.trim()))
        });
    }
    find_guide_by_model(guides, model)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samsung_sm_prefix_matches() {
        let guides = load_embedded_guides();
        let g = resolve_guide(&guides, "Samsung", "SM-A53B");
        assert!(g.is_some(), "SM-A53B should match Samsung");
        assert_eq!(g.unwrap().brand, "Samsung");
    }

    #[test]
    fn brand_only_fallback() {
        let guides = load_embedded_guides();
        let g = resolve_guide(&guides, "Samsung", "unknown-xyz");
        assert!(g.is_some(), "Samsung brand alone should match");
    }

    #[test]
    fn embedded_guides_load_all_brands() {
        let guides = load_embedded_guides();
        assert!(guides.brands.len() >= 18, "expected 18+ brands");
        assert!(guides.generic.len() >= 4);
    }

    #[test]
    fn oppo_model_from_code() {
        let guides = load_embedded_guides();
        let g = resolve_guide(&guides, "", "CPH2565");
        assert_eq!(g.map(|x| x.brand.as_str()), Some("OPPO"));
    }

    #[test]
    fn honor_split_from_huawei() {
        let guides = load_embedded_guides();
        assert_eq!(
            resolve_guide(&guides, "", "REA-NX9").map(|g| g.brand.as_str()),
            Some("Honor")
        );
    }
}
