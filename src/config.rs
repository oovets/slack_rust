use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workspace {
    pub name: String,
    // Support both 'token' (from Python client) and 'bot_token' (legacy)
    #[serde(alias = "bot_token")]
    pub token: String,
    pub app_token: String, // For Socket Mode
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub workspaces: Vec<Workspace>,
    pub active_workspace: usize,

    #[serde(default)]
    pub settings: Settings,

    #[serde(skip)]
    pub config_dir: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default = "default_true")]
    pub show_reactions: bool,

    #[serde(default = "default_true")]
    pub show_notifications: bool,

    #[serde(default)]
    pub compact_mode: bool,

    #[serde(default = "default_true")]
    pub show_emojis: bool,

    #[serde(default)]
    pub show_line_numbers: bool,

    #[serde(default = "default_true")]
    pub show_timestamps: bool,

    #[serde(default = "default_true")]
    pub show_chat_list: bool,

    #[serde(default = "default_true")]
    pub show_user_colors: bool,

    #[serde(default = "default_true")]
    pub show_borders: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            show_reactions: true,
            show_notifications: true,
            compact_mode: false,
            show_emojis: true,
            show_line_numbers: false,
            show_timestamps: true,
            show_chat_list: true,
            show_user_colors: true,
            show_borders: true,
        }
    }
}

fn default_true() -> bool {
    true
}

impl Config {
    pub fn load() -> Result<Self> {
        let config_dir = Self::get_config_dir();
        let config_path = config_dir.join("slack_config.json");

        // If Rust config exists, use it
        if config_path.exists() {
            let content = fs::read_to_string(&config_path)?;
            
            // Try to load as new multi-workspace format first
            if let Ok(mut config) = serde_json::from_str::<Config>(&content) {
                config.config_dir = config_dir;
                // Ensure active_workspace is within bounds
                if config.active_workspace >= config.workspaces.len() {
                    config.active_workspace = 0;
                }
                return Ok(config);
            }
            
            // If that fails, try to load old format and convert it
            #[derive(Deserialize)]
            struct OldConfig {
                #[serde(alias = "bot_token")]
                token: String,
                app_token: String,
                workspace_name: Option<String>,
                #[serde(default)]
                settings: Settings,
            }
            
            if let Ok(old_config) = serde_json::from_str::<OldConfig>(&content) {
                let workspace = Workspace {
                    name: old_config.workspace_name.unwrap_or_else(|| "Default".to_string()),
                    token: old_config.token,
                    app_token: old_config.app_token,
                };
                
                let config = Config {
                    workspaces: vec![workspace],
                    active_workspace: 0,
                    settings: old_config.settings,
                    config_dir: config_dir.clone(),
                };
                
                // Save in new format
                let _ = config.save();
                return Ok(config);
            }
        }

        // If Rust config doesn't exist, try to copy from Python client's config
        if let Some(python_config_path) = Self::find_python_config() {
            if python_config_path.exists() {
                match fs::read_to_string(&python_config_path) {
                    Ok(content) => {
                        // Handle both 'token' (Python client) and 'bot_token' (legacy) fields
                        match serde_json::from_str::<serde_json::Value>(&content) {
                            Ok(mut json_value) => {
                                if json_value.get("token").is_none() {
                                    if let Some(bot_token) = json_value.get("bot_token") {
                                        json_value["token"] = bot_token.clone();
                                    }
                                }

                                // Extract token and app_token from Python config
                                let token = json_value
                                    .get("token")
                                    .and_then(|v| v.as_str())
                                    .map(|s| s.to_string());
                                let app_token = json_value
                                    .get("app_token")
                                    .and_then(|v| v.as_str())
                                    .map(|s| s.to_string());

                                if let (Some(token), Some(app_token)) = (token, app_token) {
                                    // Create Rust config directory if it doesn't exist
                                    if let Err(e) = fs::create_dir_all(&config_dir) {
                                        eprintln!(
                                            "Warning: Could not create config directory: {}",
                                            e
                                        );
                                    }

                                    // Create config with copied credentials
                                    let workspace = Workspace {
                                        name: "Default".to_string(),
                                        token,
                                        app_token,
                                    };
                                    let config = Config {
                                        workspaces: vec![workspace],
                                        active_workspace: 0,
                                        settings: Settings::default(),
                                        config_dir: config_dir.clone(),
                                    };

                                    // Save to Rust config location
                                    if let Err(e) = config.save() {
                                        eprintln!("Warning: Could not save config: {}", e);
                                    } else {
                                        println!("Copied credentials from Python client config");
                                    }
                                    return Ok(config);
                                } else {
                                    eprintln!("Warning: Python config found but missing token or app_token");
                                }
                            }
                            Err(e) => {
                                eprintln!("Warning: Could not parse Python config: {}", e);
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("Warning: Could not read Python config: {}", e);
                    }
                }
            }
        }

        // No config found anywhere, create new one
        let config = Self::create_new(config_dir)?;
        Ok(config)
    }

    fn find_python_config() -> Option<PathBuf> {
        // Try multiple locations:
        // 1. Relative to current directory: ./slack_client/slack_config.json
        // 2. Relative to current directory: ../slack_client/slack_config.json
        // 3. In Code directory: ~/Code/slack_client/slack_config.json

        // Current directory
        let current_dir = std::env::current_dir().ok()?;
        let paths_to_try = vec![
            current_dir.join("slack_client").join("slack_config.json"),
            current_dir
                .join("..")
                .join("slack_client")
                .join("slack_config.json"),
            current_dir
                .join("..")
                .join("..")
                .join("slack_client")
                .join("slack_config.json"),
            dirs::home_dir()?
                .join("Code")
                .join("slack_client")
                .join("slack_config.json"),
        ];

        for path in paths_to_try {
            if path.exists() {
                return Some(path);
            }
        }

        None
    }

    pub fn save(&self) -> Result<()> {
        let config_path = self.config_dir.join("slack_config.json");
        let content = serde_json::to_string_pretty(&self)?;
        fs::write(config_path, content)?;
        Ok(())
    }

    fn create_new(config_dir: PathBuf) -> Result<Self> {
        fs::create_dir_all(&config_dir)?;

        println!("=== Slack Client Setup ===");
        println!("Get your Token from https://api.slack.com/apps");
        println!("You need:");
        println!("  1. User OAuth Token (xoxp-...) or Bot User OAuth Token (xoxb-...)");
        println!("  2. App-Level Token for Socket Mode (xapp-...)");
        println!();

        print!("Workspace name: ");
        use std::io::{self, Write};
        io::stdout().flush()?;
        let mut workspace_name = String::new();
        io::stdin().read_line(&mut workspace_name)?;
        let workspace_name = if workspace_name.trim().is_empty() {
            "Default".to_string()
        } else {
            workspace_name.trim().to_string()
        };

        print!("Enter Token (xoxp-... or xoxb-...): ");
        io::stdout().flush()?;
        let mut token = String::new();
        io::stdin().read_line(&mut token)?;
        let token = token.trim().to_string();

        print!("Enter App Token (xapp-...): ");
        io::stdout().flush()?;
        let mut app_token = String::new();
        io::stdin().read_line(&mut app_token)?;
        let app_token = app_token.trim().to_string();

        let workspace = Workspace {
            name: workspace_name,
            token,
            app_token,
        };

        let config = Config {
            workspaces: vec![workspace],
            active_workspace: 0,
            settings: Settings::default(),
            config_dir: config_dir.clone(),
        };

        config.save()?;
        println!("\nConfiguration saved to: {}", config_dir.display());
        Ok(config)
    }

    fn get_config_dir() -> PathBuf {
        // Use config directory relative to executable or current directory
        // This keeps config local to the project
        if let Ok(exe_path) = std::env::current_exe() {
            if let Some(exe_dir) = exe_path.parent() {
                // If running from target/release/, go up to project root
                if exe_dir.ends_with("target/release") || exe_dir.ends_with("target/debug") {
                    if let Some(project_root) = exe_dir.parent().and_then(|p| p.parent()) {
                        return project_root.join("config");
                    }
                }
                return exe_dir.join("config");
            }
        }

        // Fallback to current directory
        if let Ok(current_dir) = std::env::current_dir() {
            return current_dir.join("config");
        }

        // Last resort: use home directory
        let home = dirs::home_dir().expect("Cannot determine home directory");
        home.join(".config").join("slack_client_rs")
    }

    pub fn layout_path(&self) -> PathBuf {
        // Use workspace-specific layout files
        let workspace_name = if self.workspaces.is_empty() {
            "default".to_string()
        } else {
            let idx = self.active_workspace.min(self.workspaces.len().saturating_sub(1));
            self.workspaces[idx].name.clone()
        };
        self.config_dir.join(format!("layout_{}.json", workspace_name))
    }

    pub fn aliases_path(&self) -> PathBuf {
        self.config_dir.join("aliases.json")
    }
    
    pub fn settings_path(&self) -> PathBuf {
        self.config_dir.join("settings.json")
    }
}
