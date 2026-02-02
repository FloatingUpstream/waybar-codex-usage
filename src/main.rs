use anyhow::Result;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::Utc;
use clap::Parser;
use reqwest::StatusCode;
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue, USER_AGENT};
use serde::{Deserialize, Serialize};
use sha2::Digest;
use sha2::Sha256;
use std::fs;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

const DEFAULT_FORMAT: &str = "C {pct}% {reset} ({win})";
const COMPACT_FORMAT: &str = "C {pct}%";
const DEFAULT_CACHE_TTL: u64 = 60;
const DEFAULT_CHATGPT_BASE_URL: &str = "https://chatgpt.com/backend-api";
const KEYRING_SERVICE: &str = "Codex Auth";
const REFRESH_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const OAUTH_SCOPE: &str = "openid profile email";

#[derive(Parser, Debug)]
#[command(author, version, about)]
struct Args {
    #[arg(long)]
    format: Option<String>,
    #[arg(long)]
    tooltip: Option<String>,
    #[arg(long)]
    compact: bool,
    #[arg(long = "use-weekly")]
    use_weekly: bool,
    #[arg(long = "no-credits")]
    no_credits: bool,
    #[arg(long, default_value_t = DEFAULT_CACHE_TTL)]
    cache_ttl: u64,
}

#[derive(Serialize)]
struct WaybarOutput {
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    tooltip: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    class: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    alt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    percentage: Option<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DataSource {
    Live,
    CacheFresh,
    CacheStale,
}

#[derive(Serialize, Deserialize, Clone)]
struct CacheEntry {
    saved_at: i64,
    snapshot: SnapshotData,
}

#[derive(Serialize, Deserialize, Clone)]
struct SnapshotData {
    primary: Option<WindowData>,
    secondary: Option<WindowData>,
    credits: Option<CreditsData>,
}

#[derive(Serialize, Deserialize, Clone)]
struct WindowData {
    used_percent: f64,
    window_minutes: Option<i64>,
    resets_at: Option<i64>,
}

#[derive(Serialize, Deserialize, Clone)]
struct CreditsData {
    has_credits: bool,
    unlimited: bool,
    balance: Option<String>,
}

#[derive(Clone, Debug)]
struct WindowDisplay {
    used_percent: f64,
    reset_eta: String,
    label: String,
}

#[derive(Clone, Debug)]
struct FormatData {
    pct: String,
    reset: String,
    win: String,
    p_pct: String,
    p_reset: String,
    s_pct: String,
    s_reset: String,
    credits: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StoreMode {
    File,
    Keyring,
    Auto,
    Ephemeral,
}

#[derive(Clone, Debug)]
struct AppConfig {
    codex_home: PathBuf,
    chatgpt_base_url: String,
    store_mode: StoreMode,
}

#[derive(Deserialize, Default)]
struct ConfigFile {
    chatgpt_base_url: Option<String>,
    cli_auth_credentials_store: Option<String>,
}

#[derive(Deserialize, Serialize, Clone, Debug)]
struct AuthDotJson {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    auth_mode: Option<String>,
    #[serde(rename = "OPENAI_API_KEY")]
    openai_api_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tokens: Option<TokenData>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_refresh: Option<String>,
}

#[derive(Deserialize, Serialize, Clone, Debug)]
struct TokenData {
    id_token: String,
    access_token: String,
    refresh_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    account_id: Option<String>,
}

#[derive(Clone, Debug)]
struct AuthData {
    auth: AuthDotJson,
    storage: StorageType,
    modified: bool,
}

#[derive(Clone, Debug)]
enum StorageType {
    File(PathBuf),
    Keyring(String),
}

#[derive(Deserialize)]
struct RateLimitStatusPayload {
    rate_limit: Option<RateLimitStatusDetails>,
    credits: Option<CreditStatusDetails>,
}

#[derive(Deserialize)]
struct RateLimitStatusDetails {
    primary_window: Option<RateLimitWindowSnapshot>,
    secondary_window: Option<RateLimitWindowSnapshot>,
}

#[derive(Deserialize)]
struct RateLimitWindowSnapshot {
    used_percent: Option<i32>,
    limit_window_seconds: Option<i32>,
    reset_at: Option<i32>,
}

#[derive(Deserialize)]
struct CreditStatusDetails {
    has_credits: bool,
    unlimited: bool,
    balance: Option<Option<String>>,
}

#[derive(Deserialize)]
struct RefreshResponse {
    id_token: Option<String>,
    access_token: Option<String>,
    refresh_token: Option<String>,
}

#[derive(Deserialize)]
struct IdClaims {
    #[serde(rename = "https://api.openai.com/auth")]
    auth: Option<AuthClaims>,
}

#[derive(Deserialize)]
struct AuthClaims {
    chatgpt_account_id: Option<String>,
    user_id: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PathStyle {
    CodexApi,
    ChatGptApi,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    let output = run(args).await.unwrap_or_else(|err| WaybarOutput {
        text: "Codex error".to_string(),
        tooltip: Some(err.to_string()),
        class: Some("error".to_string()),
        alt: None,
        percentage: None,
    });
    let payload = serde_json::to_string(&output).unwrap_or_else(|_| "{}".to_string());
    println!("{payload}");
}

async fn run(args: Args) -> Result<WaybarOutput> {
    let config = load_app_config()?;
    let mut auth_data = match load_auth(&config) {
        Ok(auth) => auth,
        Err(_) => {
            return Ok(WaybarOutput {
                text: "Codex auth".to_string(),
                tooltip: Some("Not logged in. Run `codex login`.".to_string()),
                class: Some("error".to_string()),
                alt: None,
                percentage: None,
            });
        }
    };

    if auth_data.auth.tokens.is_none() {
        return Ok(WaybarOutput {
            text: "Codex auth".to_string(),
            tooltip: Some("Usage limits require ChatGPT login, not API key auth.".to_string()),
            class: Some("error".to_string()),
            alt: None,
            percentage: None,
        });
    }

    let cache_path = cache_file_path();
    let cache_enabled = args.cache_ttl > 0;
    let now_ts = current_timestamp();

    let cached = if cache_enabled {
        cache_path.as_deref().and_then(read_cache)
    } else {
        None
    };

    let (snapshot, source) = if let Some(entry) = cached.as_ref()
        && is_cache_fresh(entry, args.cache_ttl, now_ts)
    {
        (entry.snapshot.clone(), DataSource::CacheFresh)
    } else {
        match fetch_rate_limits(&config, &mut auth_data).await {
            Ok(snapshot) => {
                if cache_enabled && let Some(path) = cache_path.as_deref() {
                    write_cache(
                        path,
                        &CacheEntry {
                            saved_at: now_ts,
                            snapshot: snapshot.clone(),
                        },
                    );
                }
                (snapshot, DataSource::Live)
            }
            Err(fetch_err) => {
                if let Some(entry) = cached {
                    let mut output =
                        build_output(&args, entry.snapshot.clone(), DataSource::CacheStale);
                    if let Some(tooltip) = output.tooltip.as_mut() {
                        tooltip.push_str("\nCached (fetch failed)");
                    }
                    return Ok(output);
                }
                return Err(fetch_err);
            }
        }
    };

    Ok(build_output(&args, snapshot, source))
}

fn load_app_config() -> Result<AppConfig> {
    let codex_home = find_codex_home()?;
    let config_path = codex_home.join("config.toml");
    let config_file = load_config_file(&config_path);

    let chatgpt_base_url = config_file
        .chatgpt_base_url
        .unwrap_or_else(|| DEFAULT_CHATGPT_BASE_URL.to_string());
    let store_mode = config_file
        .cli_auth_credentials_store
        .as_deref()
        .and_then(StoreMode::from_str)
        .unwrap_or(StoreMode::Auto);

    Ok(AppConfig {
        codex_home,
        chatgpt_base_url,
        store_mode,
    })
}

fn load_config_file(path: &Path) -> ConfigFile {
    let data = match fs::read_to_string(path) {
        Ok(data) => data,
        Err(_) => return ConfigFile::default(),
    };
    toml::from_str(&data).unwrap_or_default()
}

fn find_codex_home() -> Result<PathBuf> {
    if let Ok(value) = std::env::var("CODEX_HOME") {
        if value.is_empty() {
            return default_codex_home();
        }
        let path = PathBuf::from(value);
        let metadata = fs::metadata(&path)
            .map_err(|err| anyhow::anyhow!("CODEX_HOME points to missing path: {err}"))?;
        if !metadata.is_dir() {
            return Err(anyhow::anyhow!("CODEX_HOME is not a directory"));
        }
        return Ok(path.canonicalize().unwrap_or(path));
    }
    default_codex_home()
}

fn default_codex_home() -> Result<PathBuf> {
    let mut home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Home directory not found"))?;
    home.push(".codex");
    Ok(home)
}

fn load_auth(config: &AppConfig) -> Result<AuthData> {
    match config.store_mode {
        StoreMode::File => load_auth_from_file(config.codex_home.join("auth.json")),
        StoreMode::Keyring => load_auth_from_keyring(&config.codex_home),
        StoreMode::Auto => load_auth_auto(&config.codex_home),
        StoreMode::Ephemeral => Err(anyhow::anyhow!(
            "Ephemeral auth is not accessible outside Codex CLI"
        )),
    }
}

fn load_auth_auto(codex_home: &Path) -> Result<AuthData> {
    match load_auth_from_keyring(codex_home) {
        Ok(auth) => Ok(auth),
        Err(_) => load_auth_from_file(codex_home.join("auth.json")),
    }
}

fn load_auth_from_file(path: PathBuf) -> Result<AuthData> {
    let data = fs::read_to_string(&path)?;
    let auth: AuthDotJson = serde_json::from_str(&data)?;
    Ok(AuthData {
        auth,
        storage: StorageType::File(path),
        modified: false,
    })
}

fn load_auth_from_keyring(codex_home: &Path) -> Result<AuthData> {
    let key = compute_store_key(codex_home)?;
    let entry = keyring::Entry::new(KEYRING_SERVICE, &key)?;
    let secret = entry.get_password()?;
    let auth: AuthDotJson = serde_json::from_str(&secret)?;
    Ok(AuthData {
        auth,
        storage: StorageType::Keyring(key),
        modified: false,
    })
}

fn compute_store_key(codex_home: &Path) -> Result<String> {
    let canonical = codex_home
        .canonicalize()
        .unwrap_or_else(|_| codex_home.to_path_buf());
    let mut hasher = Sha256::new();
    hasher.update(canonical.to_string_lossy().as_bytes());
    let digest = hasher.finalize();
    let hex = format!("{digest:x}");
    let truncated = hex.get(..16).unwrap_or(&hex);
    Ok(format!("cli|{truncated}"))
}

async fn fetch_rate_limits(config: &AppConfig, auth_data: &mut AuthData) -> Result<SnapshotData> {
    let tokens = auth_data
        .auth
        .tokens
        .as_mut()
        .ok_or_else(|| anyhow::anyhow!("Token data is not available"))?;

    if tokens.access_token.trim().is_empty() {
        return Err(anyhow::anyhow!("Access token is empty"));
    }

    if tokens.account_id.is_none()
        && let Some(account_id) = account_id_from_id_token(&tokens.id_token)
    {
        tokens.account_id = Some(account_id);
        auth_data.modified = true;
    }

    let (mut status, mut body) = request_usage(config, tokens).await?;
    if status == StatusCode::UNAUTHORIZED {
        refresh_tokens(auth_data).await?;
        let refreshed = auth_data
            .auth
            .tokens
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Token data is not available"))?;
        let response = request_usage(config, refreshed).await?;
        status = response.0;
        body = response.1;
    }

    if !status.is_success() {
        return Err(anyhow::anyhow!("Usage request failed: {status}; {body}"));
    }

    if auth_data.modified {
        save_auth(auth_data)?;
    }

    let payload: RateLimitStatusPayload = serde_json::from_str(&body)?;
    Ok(snapshot_from_payload(payload))
}

async fn request_usage(config: &AppConfig, tokens: &TokenData) -> Result<(StatusCode, String)> {
    let (base_url, path_style) = normalize_base_url(&config.chatgpt_base_url);
    let url = match path_style {
        PathStyle::CodexApi => format!("{base_url}/api/codex/usage"),
        PathStyle::ChatGptApi => format!("{base_url}/wham/usage"),
    };

    let client = reqwest::Client::new();
    let mut headers = HeaderMap::new();
    headers.insert(USER_AGENT, HeaderValue::from_static("waybar-codex-usage"));
    let auth_header = format!("Bearer {}", tokens.access_token);
    headers.insert(AUTHORIZATION, HeaderValue::from_str(&auth_header)?);
    if let Some(account_id) = tokens.account_id.as_deref()
        && let Ok(value) = HeaderValue::from_str(account_id)
    {
        headers.insert("ChatGPT-Account-Id", value);
    }

    let res = client.get(&url).headers(headers).send().await?;
    let status = res.status();
    let body = res.text().await.unwrap_or_default();
    Ok((status, body))
}

fn normalize_base_url(raw: &str) -> (String, PathStyle) {
    let mut base = raw.trim().trim_end_matches('/').to_string();
    if (base.starts_with("https://chatgpt.com") || base.starts_with("https://chat.openai.com"))
        && !base.contains("/backend-api")
    {
        base = format!("{base}/backend-api");
    }
    let style = if base.contains("/backend-api") {
        PathStyle::ChatGptApi
    } else {
        PathStyle::CodexApi
    };
    (base, style)
}

async fn refresh_tokens(auth_data: &mut AuthData) -> Result<()> {
    let tokens = auth_data
        .auth
        .tokens
        .as_mut()
        .ok_or_else(|| anyhow::anyhow!("Token data is not available"))?;
    if tokens.refresh_token.trim().is_empty() {
        return Err(anyhow::anyhow!("Refresh token is empty"));
    }

    let client = reqwest::Client::new();
    let response = client
        .post(REFRESH_TOKEN_URL)
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({
            "client_id": CLIENT_ID,
            "grant_type": "refresh_token",
            "refresh_token": tokens.refresh_token,
            "scope": OAUTH_SCOPE,
        }))
        .send()
        .await?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(anyhow::anyhow!("Token refresh failed: {status}: {body}"));
    }

    let refresh: RefreshResponse = response.json().await?;
    if let Some(access) = refresh.access_token {
        tokens.access_token = access;
    }
    if let Some(refresh_token) = refresh.refresh_token {
        tokens.refresh_token = refresh_token;
    }
    if let Some(id_token) = refresh.id_token {
        tokens.id_token = id_token;
        tokens.account_id = account_id_from_id_token(&tokens.id_token);
    }
    auth_data.auth.last_refresh = Some(Utc::now().to_rfc3339());
    auth_data.modified = true;
    save_auth(auth_data)?;
    Ok(())
}

fn account_id_from_id_token(id_token: &str) -> Option<String> {
    let mut parts = id_token.split('.');
    let _header = parts.next()?;
    let payload = parts.next()?;
    let payload_bytes = URL_SAFE_NO_PAD.decode(payload).ok()?;
    let claims: IdClaims = serde_json::from_slice(&payload_bytes).ok()?;
    claims
        .auth
        .and_then(|auth| auth.chatgpt_account_id.or(auth.user_id))
}

fn snapshot_from_payload(payload: RateLimitStatusPayload) -> SnapshotData {
    let primary = payload
        .rate_limit
        .as_ref()
        .and_then(|details| details.primary_window.as_ref())
        .map(window_from_snapshot);
    let secondary = payload
        .rate_limit
        .as_ref()
        .and_then(|details| details.secondary_window.as_ref())
        .map(window_from_snapshot);
    let credits = payload.credits.as_ref().map(CreditsData::from_details);

    SnapshotData {
        primary,
        secondary,
        credits,
    }
}

fn window_from_snapshot(window: &RateLimitWindowSnapshot) -> WindowData {
    let used_percent = window.used_percent.unwrap_or(0) as f64;
    let window_minutes = window.limit_window_seconds.and_then(|seconds| {
        if seconds > 0 {
            Some((i64::from(seconds) + 59) / 60)
        } else {
            None
        }
    });
    let resets_at = window.reset_at.map(i64::from);

    WindowData {
        used_percent,
        window_minutes,
        resets_at,
    }
}

impl CreditsData {
    fn from_details(details: &CreditStatusDetails) -> Self {
        Self {
            has_credits: details.has_credits,
            unlimited: details.unlimited,
            balance: details.balance.clone().and_then(|inner| inner),
        }
    }
}

fn save_auth(auth_data: &AuthData) -> Result<()> {
    let json_data = serde_json::to_string_pretty(&auth_data.auth)?;
    match &auth_data.storage {
        StorageType::File(path) => {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut options = OpenOptions::new();
            options.truncate(true).write(true).create(true);
            #[cfg(unix)]
            {
                options.mode(0o600);
            }
            let mut file = options.open(path)?;
            use std::io::Write;
            file.write_all(json_data.as_bytes())?;
            file.flush()?;
        }
        StorageType::Keyring(key) => {
            let entry = keyring::Entry::new(KEYRING_SERVICE, key)?;
            entry.set_password(&json_data)?;
        }
    }
    Ok(())
}

impl StoreMode {
    fn from_str(value: &str) -> Option<Self> {
        match value.trim().to_lowercase().as_str() {
            "file" => Some(StoreMode::File),
            "keyring" => Some(StoreMode::Keyring),
            "auto" => Some(StoreMode::Auto),
            "ephemeral" => Some(StoreMode::Ephemeral),
            _ => None,
        }
    }
}

fn build_output(args: &Args, snapshot: SnapshotData, source: DataSource) -> WaybarOutput {
    let now_ts = current_timestamp();
    let display = build_format_data(args, &snapshot, now_ts);
    let text_template = match (&args.format, args.compact) {
        (Some(value), _) => value.as_str(),
        (None, true) => COMPACT_FORMAT,
        (None, false) => DEFAULT_FORMAT,
    };
    let text = apply_format(text_template, &display);

    let tooltip = if let Some(template) = args.tooltip.as_deref() {
        Some(apply_format(template, &display))
    } else {
        Some(default_tooltip(&display, !args.no_credits))
    };

    let class = match source {
        DataSource::Live => "ok",
        DataSource::CacheFresh => "cached",
        DataSource::CacheStale => "stale",
    };

    WaybarOutput {
        text,
        tooltip,
        class: Some(class.to_string()),
        alt: None,
        percentage: display.pct.parse::<f64>().ok(),
    }
}

fn build_format_data(args: &Args, snapshot: &SnapshotData, now_ts: i64) -> FormatData {
    let primary = snapshot
        .primary
        .as_ref()
        .map(|w| WindowDisplay::from_window(w, "5h", now_ts));
    let secondary = snapshot
        .secondary
        .as_ref()
        .map(|w| WindowDisplay::from_window(w, "7d", now_ts));

    let active = if args.use_weekly {
        secondary.clone().or_else(|| primary.clone())
    } else {
        primary.clone().or_else(|| secondary.clone())
    };

    let pct = active
        .as_ref()
        .map(|w| format_percent(w.used_percent))
        .unwrap_or_else(|| "N/A".to_string());
    let reset = active
        .as_ref()
        .map(|w| w.reset_eta.clone())
        .unwrap_or_else(|| "N/A".to_string());
    let win = active
        .as_ref()
        .map(|w| w.label.clone())
        .unwrap_or_else(|| "N/A".to_string());

    let p_pct = primary
        .as_ref()
        .map(|w| format_percent(w.used_percent))
        .unwrap_or_else(|| "N/A".to_string());
    let p_reset = primary
        .as_ref()
        .map(|w| w.reset_eta.clone())
        .unwrap_or_else(|| "N/A".to_string());
    let s_pct = secondary
        .as_ref()
        .map(|w| format_percent(w.used_percent))
        .unwrap_or_else(|| "N/A".to_string());
    let s_reset = secondary
        .as_ref()
        .map(|w| w.reset_eta.clone())
        .unwrap_or_else(|| "N/A".to_string());

    let credits = if args.no_credits {
        String::new()
    } else {
        format_credits(snapshot.credits.as_ref())
    };

    FormatData {
        pct,
        reset,
        win,
        p_pct,
        p_reset,
        s_pct,
        s_reset,
        credits,
    }
}

impl WindowDisplay {
    fn from_window(window: &WindowData, fallback_label: &str, now_ts: i64) -> Self {
        let label = format_window_label(window.window_minutes, fallback_label);
        let reset_eta = format_eta(window.resets_at, now_ts);
        Self {
            used_percent: window.used_percent,
            reset_eta,
            label,
        }
    }
}

fn apply_format(template: &str, data: &FormatData) -> String {
    let mut out = template.to_string();
    for (key, value) in [
        ("{pct}", data.pct.as_str()),
        ("{reset}", data.reset.as_str()),
        ("{win}", data.win.as_str()),
        ("{p_pct}", data.p_pct.as_str()),
        ("{p_reset}", data.p_reset.as_str()),
        ("{s_pct}", data.s_pct.as_str()),
        ("{s_reset}", data.s_reset.as_str()),
        ("{credits}", data.credits.as_str()),
    ] {
        out = out.replace(key, value);
    }
    out
}

fn default_tooltip(data: &FormatData, include_credits: bool) -> String {
    let mut lines = vec![
        format!("5h: {}% ({})", data.p_pct, data.p_reset),
        format!("7d: {}% ({})", data.s_pct, data.s_reset),
    ];
    if include_credits {
        lines.push(format!("Credits: {}", data.credits));
    }
    lines.join("\n")
}

fn format_percent(value: f64) -> String {
    format!("{:.0}", value)
}

fn format_window_label(window_minutes: Option<i64>, fallback: &str) -> String {
    let Some(minutes) = window_minutes else {
        return fallback.to_string();
    };
    if minutes % 1440 == 0 {
        format!("{}d", minutes / 1440)
    } else if minutes % 60 == 0 {
        format!("{}h", minutes / 60)
    } else {
        format!("{}m", minutes)
    }
}

fn format_eta(resets_at: Option<i64>, now_ts: i64) -> String {
    let Some(resets_at) = resets_at else {
        return "N/A".to_string();
    };
    let delta = resets_at.saturating_sub(now_ts);
    if delta <= 0 {
        return "0m00s".to_string();
    }

    if delta >= 86_400 {
        let days = delta / 86_400;
        let hours = (delta % 86_400) / 3600;
        return format!("{days}d{hours:02}h");
    }

    if delta >= 3600 {
        let hours = delta / 3600;
        let mins = (delta % 3600) / 60;
        return format!("{hours}h{mins:02}m");
    }

    let mins = delta / 60;
    let secs = delta % 60;
    format!("{mins}m{secs:02}s")
}

fn format_credits(credits: Option<&CreditsData>) -> String {
    let Some(credits) = credits else {
        return "N/A".to_string();
    };
    if !credits.has_credits {
        return "N/A".to_string();
    }
    if credits.unlimited {
        return "Unlimited".to_string();
    }
    if let Some(balance) = credits.balance.as_deref() {
        if let Ok(int_value) = balance.trim().parse::<i64>()
            && int_value > 0
        {
            return int_value.to_string();
        }
        if let Ok(float_value) = balance.trim().parse::<f64>()
            && float_value > 0.0
        {
            return (float_value.round() as i64).to_string();
        }
    }
    "0".to_string()
}

fn cache_file_path() -> Option<PathBuf> {
    if let Ok(base) = std::env::var("XDG_CACHE_HOME") {
        return Some(
            PathBuf::from(base)
                .join("waybar-codex-usage")
                .join("cache.json"),
        );
    }
    if let Ok(home) = std::env::var("HOME") {
        return Some(
            PathBuf::from(home)
                .join(".cache")
                .join("waybar-codex-usage")
                .join("cache.json"),
        );
    }
    None
}

fn read_cache(path: &Path) -> Option<CacheEntry> {
    let data = fs::read_to_string(path).ok()?;
    serde_json::from_str(&data).ok()
}

fn write_cache(path: &Path, entry: &CacheEntry) {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(data) = serde_json::to_string(entry) {
        let _ = fs::write(path, data);
    }
}

fn is_cache_fresh(entry: &CacheEntry, ttl: u64, now_ts: i64) -> bool {
    if ttl == 0 {
        return false;
    }
    let ttl_i64 = ttl as i64;
    now_ts.saturating_sub(entry.saved_at) <= ttl_i64
}

fn current_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
