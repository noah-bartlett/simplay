use anyhow::{anyhow, Context, Result};
use rpassword::read_password;
use serde::{Deserialize, Serialize};
use std::env;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;

const DEFAULT_API_VERSION: &str = "1.16.1";
const DEFAULT_CLIENT_NAME: &str = "simplay";
const DEFAULT_ENDPOINT_SUFFIX: &str = "view";
const DEFAULT_MAX_SHUFFLE: usize = 0;
const DEFAULT_VOLUME_STEP: u8 = 5;
const DEFAULT_END_GRACE_MS: u64 = 500;

#[derive(Debug, Clone)]
pub struct Config {
    pub server_url: String,
    pub username: String,
    pub password: String,
    pub api_version: String,
    pub client_name: String,
    pub endpoint_suffix: String,
    pub tls_verify: bool,
    pub max_shuffle: usize,
    pub volume_step: u8,
    pub end_grace_ms: u64,
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct ConfigFile {
    server_url: Option<String>,
    username: Option<String>,
    password: Option<String>,
    api_version: Option<String>,
    client_name: Option<String>,
    endpoint_suffix: Option<String>,
    tls_verify: Option<bool>,
    max_shuffle: Option<usize>,
    volume_step: Option<u8>,
    end_grace_ms: Option<u64>,
}

impl Config {
    pub fn load_or_prompt_required() -> Result<Self> {
        let mut file = load_config_file()?.unwrap_or_default();
        let mut updated = false;

        if file.server_url.as_deref().unwrap_or("").is_empty() {
            file.server_url = Some(prompt_required("Navidrome server URL")?);
            updated = true;
        }

        if file.username.as_deref().unwrap_or("").is_empty() {
            file.username = Some(prompt_required("Username")?);
            updated = true;
        }

        if file.password.as_deref().unwrap_or("").is_empty() {
            file.password = Some(prompt_password("Password", None)?);
            updated = true;
        }

        let config = Config::from_file(file);
        if updated {
            config.save()?;
        }
        Ok(config)
    }

    pub fn configure() -> Result<Self> {
        let file = load_config_file()?.unwrap_or_default();

        let server_url = prompt_with_default(
            "Navidrome server URL",
            file.server_url.as_deref(),
            true,
        )?;
        let username = prompt_with_default("Username", file.username.as_deref(), true)?;
        let password = prompt_password("Password", file.password.as_deref())?;

        let api_version = prompt_with_default(
            "Subsonic API version",
            file.api_version.as_deref().or(Some(DEFAULT_API_VERSION)),
            false,
        )?;
        let client_name = prompt_with_default(
            "Client name",
            file.client_name.as_deref().or(Some(DEFAULT_CLIENT_NAME)),
            false,
        )?;
        let endpoint_suffix = prompt_with_default(
            "Endpoint suffix",
            file.endpoint_suffix.as_deref().or(Some(DEFAULT_ENDPOINT_SUFFIX)),
            false,
        )?;
        let tls_verify = prompt_bool(
            "Verify TLS certificates",
            file.tls_verify.unwrap_or(true),
        )?;
        let max_shuffle = prompt_usize(
            "Max shuffle size (0 = full library)",
            file.max_shuffle.unwrap_or(DEFAULT_MAX_SHUFFLE),
        )?;
        let volume_step = prompt_u8(
            "Volume step (0-100)",
            file.volume_step.unwrap_or(DEFAULT_VOLUME_STEP),
        )?;
        let end_grace_ms = prompt_u64(
            "End-of-track grace ms",
            file.end_grace_ms.unwrap_or(DEFAULT_END_GRACE_MS),
        )?;

        let config = Config {
            server_url: normalize_url(&server_url),
            username,
            password,
            api_version,
            client_name,
            endpoint_suffix,
            tls_verify,
            max_shuffle,
            volume_step,
            end_grace_ms,
        };
        config.save()?;
        Ok(config)
    }

    pub fn save(&self) -> Result<()> {
        let path = config_path()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let file = ConfigFile {
            server_url: Some(self.server_url.clone()),
            username: Some(self.username.clone()),
            password: Some(self.password.clone()),
            api_version: Some(self.api_version.clone()),
            client_name: Some(self.client_name.clone()),
            endpoint_suffix: Some(self.endpoint_suffix.clone()),
            tls_verify: Some(self.tls_verify),
            max_shuffle: Some(self.max_shuffle),
            volume_step: Some(self.volume_step),
            end_grace_ms: Some(self.end_grace_ms),
        };

        let encoded = toml::to_string_pretty(&file)?;
        let mut handle = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)?;
        handle.write_all(encoded.as_bytes())?;
        handle.flush()?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        Ok(())
    }

    pub fn socket_path() -> Result<PathBuf> {
        let base = match runtime_dir() {
            Some(dir) => dir,
            None => config_dir()?,
        };
        let dir = base.join("simplay");
        fs::create_dir_all(&dir)?;
        Ok(dir.join("simplay.sock"))
    }

    pub fn mpv_socket_path() -> Result<PathBuf> {
        let base = match runtime_dir() {
            Some(dir) => dir,
            None => config_dir()?,
        };
        let dir = base.join("simplay");
        fs::create_dir_all(&dir)?;
        Ok(dir.join("simplay-mpv.sock"))
    }

    pub fn max_shuffle(&self) -> usize {
        self.max_shuffle
    }

    pub fn volume_step(&self) -> u8 {
        self.volume_step
    }

    pub fn end_grace_ms(&self) -> u64 {
        self.end_grace_ms
    }
}

impl Config {
    fn from_file(file: ConfigFile) -> Self {
        let server_url = normalize_url(file.server_url.unwrap_or_default().as_str());
        let username = file.username.unwrap_or_default();
        let password = file.password.unwrap_or_default();
        let api_version = file
            .api_version
            .unwrap_or_else(|| DEFAULT_API_VERSION.to_string());
        let client_name = file
            .client_name
            .unwrap_or_else(|| DEFAULT_CLIENT_NAME.to_string());
        let endpoint_suffix = file
            .endpoint_suffix
            .unwrap_or_else(|| DEFAULT_ENDPOINT_SUFFIX.to_string());
        let tls_verify = file.tls_verify.unwrap_or(true);
        let max_shuffle = file.max_shuffle.unwrap_or(DEFAULT_MAX_SHUFFLE);
        let volume_step = file.volume_step.unwrap_or(DEFAULT_VOLUME_STEP);
        let end_grace_ms = file.end_grace_ms.unwrap_or(DEFAULT_END_GRACE_MS);

        Self {
            server_url,
            username,
            password,
            api_version,
            client_name,
            endpoint_suffix,
            tls_verify,
            max_shuffle,
            volume_step,
            end_grace_ms,
        }
    }
}

fn load_config_file() -> Result<Option<ConfigFile>> {
    let path = config_path()?;
    if !path.exists() {
        return Ok(None);
    }
    let contents = fs::read_to_string(&path)
        .with_context(|| format!("Failed reading config {}", path.display()))?;
    let file = toml::from_str(&contents).context("Invalid config file format")?;
    Ok(Some(file))
}

fn config_dir() -> Result<PathBuf> {
    if let Ok(dir) = env::var("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(dir));
    }
    let home = env::var("HOME").map_err(|_| anyhow!("HOME not set"))?;
    Ok(PathBuf::from(home).join(".config"))
}

fn runtime_dir() -> Option<PathBuf> {
    env::var("XDG_RUNTIME_DIR").ok().map(PathBuf::from)
}

fn config_path() -> Result<PathBuf> {
    let dir = config_dir()?.join("simplay");
    fs::create_dir_all(&dir)?;
    Ok(dir.join("simplay.conf"))
}

fn normalize_url(input: &str) -> String {
    input.trim().trim_end_matches('/').to_string()
}

fn prompt_required(label: &str) -> Result<String> {
    loop {
        let value = prompt_line(&format!("{}: ", label))?;
        if !value.trim().is_empty() {
            return Ok(value.trim().to_string());
        }
        println!("Value is required.");
    }
}

fn prompt_with_default(label: &str, current: Option<&str>, required: bool) -> Result<String> {
    let suffix = if let Some(val) = current {
        format!(" [{}]", val)
    } else {
        String::new()
    };
    let prompt = format!("{}{}: ", label, suffix);
    let input = prompt_line(&prompt)?;
    if input.trim().is_empty() {
        if let Some(val) = current {
            return Ok(val.to_string());
        }
        if required {
            return prompt_required(label);
        }
        return Ok(String::new());
    }
    Ok(input.trim().to_string())
}

fn prompt_password(label: &str, current: Option<&str>) -> Result<String> {
    loop {
        let suffix = if current.is_some() {
            " (leave blank to keep current)"
        } else {
            ""
        };
        print!("{}{}: ", label, suffix);
        io::stdout().flush()?;
        let password = read_password()?;
        if password.is_empty() {
            if let Some(val) = current {
                return Ok(val.to_string());
            }
            println!("Password is required.");
            continue;
        }
        return Ok(password);
    }
}

fn prompt_bool(label: &str, default: bool) -> Result<bool> {
    let prompt = format!("{} [{}]: ", label, if default { "Y" } else { "n" });
    let input = prompt_line(&prompt)?;
    if input.trim().is_empty() {
        return Ok(default);
    }
    let normalized = input.trim().to_lowercase();
    Ok(matches!(normalized.as_str(), "y" | "yes" | "true" | "1"))
}

fn prompt_usize(label: &str, default: usize) -> Result<usize> {
    let prompt = format!("{} [{}]: ", label, default);
    let input = prompt_line(&prompt)?;
    if input.trim().is_empty() {
        return Ok(default);
    }
    input
        .trim()
        .parse::<usize>()
        .map_err(|_| anyhow!("Invalid number"))
}

fn prompt_u8(label: &str, default: u8) -> Result<u8> {
    let prompt = format!("{} [{}]: ", label, default);
    let input = prompt_line(&prompt)?;
    if input.trim().is_empty() {
        return Ok(default);
    }
    input
        .trim()
        .parse::<u8>()
        .map_err(|_| anyhow!("Invalid number"))
}

fn prompt_u64(label: &str, default: u64) -> Result<u64> {
    let prompt = format!("{} [{}]: ", label, default);
    let input = prompt_line(&prompt)?;
    if input.trim().is_empty() {
        return Ok(default);
    }
    input
        .trim()
        .parse::<u64>()
        .map_err(|_| anyhow!("Invalid number"))
}

fn prompt_line(prompt: &str) -> Result<String> {
    print!("{}", prompt);
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    Ok(input)
}
