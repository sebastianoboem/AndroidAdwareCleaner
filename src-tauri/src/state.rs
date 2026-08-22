use cloud_sync::CloudSync;
use reputation_db::ReputationDb;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct AppConfig {
    pub sync_folder: Option<String>,
    #[serde(default)]
    pub sync_provider: Option<String>,
    pub custom_adb_path: Option<String>,
}

pub struct AppState {
    pub db: Mutex<ReputationDb>,
    pub cloud_sync: Mutex<CloudSync>,
    pub config: Mutex<AppConfig>,
    pub config_path: PathBuf,
    pub platform_tools_dir: PathBuf,
    pub metadata_cache_path: PathBuf,
}

impl AppState {
    pub fn new() -> Result<Self, String> {
        let db = ReputationDb::open_default().map_err(|e| e.to_string())?;
        let config_dir = dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("AndroidAdwareCleaner");
        std::fs::create_dir_all(&config_dir).map_err(|e| e.to_string())?;
        let config_path = config_dir.join("config.json");

        let config = if config_path.exists() {
            let data = std::fs::read_to_string(&config_path).map_err(|e| e.to_string())?;
            serde_json::from_str(&data).unwrap_or_default()
        } else {
            AppConfig::default()
        };

        let sync_dir = config
            .sync_folder
            .as_ref()
            .map(PathBuf::from);

        let mut cloud_sync = CloudSync::new(sync_dir);
        let _ = cloud_sync.pull(&db);

        let platform_tools_dir = config_dir.join("platform-tools");
        let metadata_cache_path = config_dir.join("metadata-cache.json");
        std::env::set_var(
            "ANDROID_ADWARE_PLATFORM_TOOLS",
            platform_tools_dir.to_string_lossy().as_ref(),
        );
        if let Some(ref adb) = config.custom_adb_path {
            std::env::set_var("ANDROID_ADWARE_CUSTOM_ADB", adb);
        }

        Ok(Self {
            db: Mutex::new(db),
            cloud_sync: Mutex::new(cloud_sync),
            config: Mutex::new(config),
            config_path,
            platform_tools_dir,
            metadata_cache_path,
        })
    }

    pub fn save_config(&self) -> Result<(), String> {
        let config = self.config.lock().map_err(|e| e.to_string())?;
        let json = serde_json::to_string_pretty(&*config).map_err(|e| e.to_string())?;
        std::fs::write(&self.config_path, json).map_err(|e| e.to_string())?;
        Ok(())
    }
}
