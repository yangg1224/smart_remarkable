use crate::config::Config;
use crate::status::SmartRemarkableStatus;
use crate::touch::{Touch, TriggerCorner};
use anyhow::Result;
use log::{info, warn};
use serde_json::{json, Value};
use std::convert::Infallible;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use tokio::sync::RwLock as TokioRwLock;
use warp::http::StatusCode;
use warp::reply::{json as reply_json, with_status};
use warp::{Filter, Rejection, Reply};

const WEB_FILES: &[(&str, &str, &str)] = &[
    ("index.html", include_str!("web/index.html"), "text/html"),
    ("style.css", include_str!("web/style.css"), "text/css"),
    ("app.js", include_str!("web/app.js"), "application/javascript"),
];

/// Config fields that hold secrets. They are never sent back out of `GET /api/config`
/// (only an `<field>_set` boolean is), and an empty/missing value in `POST /api/config`
/// keeps the currently stored secret instead of wiping it.
const SECRET_FIELDS: &[&str] = &["engine_api_key", "image_api_key"];

/// Start the web server on `bind:port` with shared state.
///
/// Binds to whatever address the caller passes (the CLI defaults to `127.0.0.1`).
/// Every request is also checked by [`request_guard`], which rejects cross-origin
/// browser requests and non-IP/non-localhost `Host` headers (DNS rebinding).
pub async fn start_web_server(
    bind: IpAddr,
    port: u16,
    shared_config: Arc<TokioRwLock<Config>>,
    shared_status: Arc<TokioRwLock<SmartRemarkableStatus>>,
    shared_touch: Option<Arc<TokioRwLock<Touch>>>,
    cancellation: Option<Arc<TokioRwLock<crate::cancellation::SmartRemarkableCancellation>>>,
    config_watch_tx: Option<Arc<tokio::sync::watch::Sender<Config>>>,
) -> Result<()> {
    let addr = SocketAddr::new(bind, port);
    info!("Starting web server on {}", addr);
    if !bind.is_loopback() {
        warn!(
            "Web server is listening on non-loopback address {} with no authentication: anyone who can reach this \
             port can change the configuration",
            bind
        );
    }

    // Static file routes
    let static_files = warp::path::end()
        .map(|| serve_static_file("index.html"))
        .or(warp::path("style.css").map(|| serve_static_file("style.css")))
        .or(warp::path("app.js").map(|| serve_static_file("app.js")));

    // API routes with shared state
    let config_for_get = Arc::clone(&shared_config);
    let config_for_post = Arc::clone(&shared_config);
    let status_for_get = Arc::clone(&shared_status);
    let cancellation_for_config = cancellation.clone();
    let config_watch_tx_for_post = config_watch_tx.clone();

    let api_routes = warp::path("api").and(
        // GET /api/config
        warp::path("config")
            .and(warp::get())
            .and(warp::any().map(move || Arc::clone(&config_for_get)))
            .and_then(get_config_handler)
            .or(
                // POST /api/config
                warp::path("config")
                    .and(warp::post())
                    .and(warp::body::json())
                    .and(warp::any().map(move || Arc::clone(&config_for_post)))
                    .and(warp::any().map(move || cancellation_for_config.clone()))
                    .and(warp::any().map(move || config_watch_tx_for_post.clone()))
                    .and_then(save_config_handler),
            )
            .or(
                // GET /api/status
                warp::path("status")
                    .and(warp::get())
                    .and(warp::any().map(move || Arc::clone(&status_for_get)))
                    .and_then(get_status_handler),
            )
            .or(
                // Simulation endpoints
                warp::path("simulation").and(
                    // POST /api/simulation/trigger
                    warp::path("trigger")
                        .and(warp::post())
                        .and(warp::body::json())
                        .and(warp::any().map(move || shared_touch.clone()))
                        .and_then(simulation_trigger_handler),
                ),
            ),
    );

    // No CORS layer on purpose: the UI is served from this same origin, and
    // cross-origin browser requests are rejected by `request_guard`.
    let routes = request_guard().and(static_files.or(api_routes)).recover(handle_rejection);

    info!("Web interface available at http://{}/", addr);

    warp::serve(routes).run(addr).await;

    Ok(())
}

fn serve_static_file(filename: &str) -> impl Reply {
    if let Some((_, content, content_type)) = WEB_FILES.iter().find(|(name, _, _)| *name == filename) {
        warp::reply::with_header(warp::reply::with_status(*content, StatusCode::OK), "content-type", *content_type)
    } else {
        warp::reply::with_header(warp::reply::with_status("File not found", StatusCode::NOT_FOUND), "content-type", "text/plain")
    }
}

async fn simulation_trigger_handler(trigger_data: Value, shared_touch: Option<Arc<TokioRwLock<Touch>>>) -> Result<impl Reply, Rejection> {
    let corner_str = trigger_data["corner"].as_str().unwrap_or("UR");

    if let Some(touch_arc) = shared_touch {
        match TriggerCorner::from_string(corner_str) {
            Ok(corner) => {
                let touch = touch_arc.read().await;
                touch.add_manual_trigger(corner);
                info!("Manual trigger added for corner: {:?}", corner);
                Ok(reply_json(&json!({
                    "status": "success",
                    "message": format!("Trigger added for corner: {:?}", corner)
                })))
            }
            Err(e) => {
                warn!("Invalid trigger corner: {}", e);
                Err(warp::reject::custom(ConfigError::Validation(e.to_string())))
            }
        }
    } else {
        warn!("Simulation endpoints not available: no touch component");
        Err(warp::reject::custom(ConfigError::Validation(
            "Simulation endpoints are only available when touch component is enabled".to_string(),
        )))
    }
}

/// Rejects requests that a browser on another site (or a DNS-rebinding attack) could make:
/// the `Host` header must be `localhost` or an IP literal, and if an `Origin` header is
/// present it must be this same origin. Non-browser clients (curl) send no `Origin`.
fn request_guard() -> impl Filter<Extract = (), Error = Rejection> + Clone {
    warp::header::optional::<String>("host")
        .and(warp::header::optional::<String>("origin"))
        .and_then(|host: Option<String>, origin: Option<String>| async move {
            if is_request_allowed(host.as_deref(), origin.as_deref()) {
                Ok(())
            } else {
                warn!("Rejected web request: host={:?} origin={:?}", host, origin);
                Err(warp::reject::custom(Forbidden))
            }
        })
        .untuple_one()
}

/// Strip an optional `:port` from a `Host` header value, handling `[v6]:port`.
fn host_without_port(host: &str) -> &str {
    if let Some(rest) = host.strip_prefix('[') {
        return rest.split(']').next().unwrap_or(rest);
    }
    match host.rsplit_once(':') {
        Some((name, port)) if !name.contains(':') && port.chars().all(|c| c.is_ascii_digit()) => name,
        _ => host,
    }
}

fn is_request_allowed(host: Option<&str>, origin: Option<&str>) -> bool {
    let Some(host) = host.map(str::trim).filter(|h| !h.is_empty()) else {
        return false;
    };
    let name = host_without_port(host);
    let host_ok = name.eq_ignore_ascii_case("localhost") || name.parse::<IpAddr>().is_ok();
    if !host_ok {
        return false;
    }
    match origin {
        None => true,
        Some(origin) => origin.eq_ignore_ascii_case(&format!("http://{}", host)),
    }
}

/// Serialize the config for `GET /api/config` with secrets replaced by `<field>_set` flags.
fn redacted_config_json(config: &Config) -> Value {
    let mut value = serde_json::to_value(config).unwrap_or_else(|_| json!({}));
    if let Some(obj) = value.as_object_mut() {
        for field in SECRET_FIELDS {
            let is_set = obj.get(*field).and_then(Value::as_str).is_some_and(|s| !s.is_empty());
            obj.remove(*field);
            obj.insert(format!("{}_set", field), Value::Bool(is_set));
        }
    }
    value
}

/// Build the new config for `POST /api/config` by overlaying the submitted fields onto
/// the current config. Fields the client didn't send keep their current value, and a
/// secret sent as null/empty keeps the stored secret.
fn merge_config_update(current: &Config, update: &Value) -> Result<Config, String> {
    let update = update.as_object().ok_or_else(|| "config update must be a JSON object".to_string())?;
    let mut merged = serde_json::to_value(current).map_err(|e| e.to_string())?;
    let obj = merged.as_object_mut().ok_or_else(|| "config did not serialize to an object".to_string())?;
    for (key, value) in update {
        if key.ends_with("_set") && SECRET_FIELDS.contains(&key.trim_end_matches("_set")) {
            continue; // read-only flags we emitted in GET
        }
        if SECRET_FIELDS.contains(&key.as_str()) && value.as_str().is_none_or(str::is_empty) {
            continue; // keep the stored secret
        }
        obj.insert(key.clone(), value.clone());
    }
    serde_json::from_value(merged).map_err(|e| e.to_string())
}

async fn get_config_handler(shared_config: Arc<TokioRwLock<Config>>) -> Result<impl Reply, Rejection> {
    let config = shared_config.read().await;
    Ok(reply_json(&redacted_config_json(&config)))
}

async fn save_config_handler(
    update: Value,
    shared_config: Arc<TokioRwLock<Config>>,
    cancellation: Option<Arc<TokioRwLock<crate::cancellation::SmartRemarkableCancellation>>>,
    config_watch_tx: Option<Arc<tokio::sync::watch::Sender<Config>>>,
) -> Result<impl Reply, Rejection> {
    let config = {
        let current = shared_config.read().await;
        merge_config_update(&current, &update).map_err(|e| warp::reject::custom(ConfigError::Validation(e)))?
    };

    // Validate the config before saving
    if let Err(e) = config.validate() {
        warn!("Config validation failed: {}", e);
        return Err(warp::reject::custom(ConfigError::Validation(e.to_string())));
    }

    // Update shared config first (for immediate effect)
    {
        let mut shared = shared_config.write().await;
        *shared = config.clone();
    }

    // Notify main loop via watch channel (preferred method)
    if let Some(watch_tx) = &config_watch_tx {
        info!("Broadcasting config change via watch channel");
        let _ = watch_tx.send(config.clone());
    }

    // Also trigger cancellation to interrupt current execution
    if let Some(cancellation) = &cancellation {
        info!("Triggering cancellation due to config change from web interface");
        let cancel_guard = cancellation.read().await;
        cancel_guard.cancel_execution();
    }

    // Also save to file
    match config.save() {
        Ok(()) => {
            info!("Configuration saved successfully and updated in memory");
            Ok(reply_json(&json!({
                "status": "success",
                "message": "Configuration saved successfully and applied immediately"
            })))
        }
        Err(e) => {
            warn!("Failed to save config to file: {}", e);
            Err(warp::reject::custom(ConfigError::Save(e.to_string())))
        }
    }
}

async fn get_status_handler(shared_status: Arc<TokioRwLock<SmartRemarkableStatus>>) -> Result<impl Reply, Rejection> {
    let status = shared_status.read().await;
    Ok(reply_json(&*status))
}

#[derive(Debug)]
enum ConfigError {
    Load(String),
    Save(String),
    Validation(String),
}

impl warp::reject::Reject for ConfigError {}

#[derive(Debug)]
struct Forbidden;

impl warp::reject::Reject for Forbidden {}

async fn handle_rejection(err: Rejection) -> Result<impl Reply, Infallible> {
    let (code, message) = if err.is_not_found() {
        (StatusCode::NOT_FOUND, "Not Found".to_string())
    } else if err.find::<Forbidden>().is_some() {
        (StatusCode::FORBIDDEN, "Forbidden".to_string())
    } else if let Some(config_err) = err.find::<ConfigError>() {
        match config_err {
            ConfigError::Load(msg) => (StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to load config: {}", msg)),
            ConfigError::Save(msg) => (StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to save config: {}", msg)),
            ConfigError::Validation(msg) => (StatusCode::BAD_REQUEST, format!("Config validation failed: {}", msg)),
        }
    } else if err.find::<warp::filters::body::BodyDeserializeError>().is_some() {
        (StatusCode::BAD_REQUEST, "Invalid JSON".to_string())
    } else {
        (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error".to_string())
    };

    let json = reply_json(&json!({
        "error": message,
        "code": code.as_u16()
    }));

    Ok(with_status(json, code))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_config_never_returns_secrets() {
        let config = Config {
            engine_api_key: Some("sk-secret".into()),
            image_api_key: None,
            ..Config::default()
        };
        let v = redacted_config_json(&config);
        assert!(!v.to_string().contains("sk-secret"));
        assert!(v.get("engine_api_key").is_none());
        assert!(v.get("image_api_key").is_none());
        assert_eq!(v["engine_api_key_set"], json!(true));
        assert_eq!(v["image_api_key_set"], json!(false));
        assert_eq!(v["model"], json!(config.model));
    }

    #[test]
    fn post_with_empty_or_missing_key_keeps_stored_key() {
        let current = Config {
            engine_api_key: Some("sk-old".into()),
            image_api_key: Some("img-old".into()),
            ..Config::default()
        };
        let merged = merge_config_update(&current, &json!({ "model": "gpt-4o", "engine_api_key": "", "engine_api_key_set": false })).unwrap();
        assert_eq!(merged.model, "gpt-4o");
        assert_eq!(merged.engine_api_key.as_deref(), Some("sk-old"));
        assert_eq!(merged.image_api_key.as_deref(), Some("img-old"));

        let merged = merge_config_update(&current, &json!({ "engine_api_key": null })).unwrap();
        assert_eq!(merged.engine_api_key.as_deref(), Some("sk-old"));
    }

    #[test]
    fn post_with_new_key_replaces_it() {
        let current = Config {
            engine_api_key: Some("sk-old".into()),
            ..Config::default()
        };
        let merged = merge_config_update(&current, &json!({ "engine_api_key": "sk-new" })).unwrap();
        assert_eq!(merged.engine_api_key.as_deref(), Some("sk-new"));
    }

    #[test]
    fn post_keeps_fields_the_client_did_not_send() {
        let current = Config {
            select_mode: true,
            image_model: Some("gemini-2.5-flash-image".into()),
            ..Config::default()
        };
        let merged = merge_config_update(&current, &json!({ "model": "x" })).unwrap();
        assert!(merged.select_mode);
        assert_eq!(merged.image_model.as_deref(), Some("gemini-2.5-flash-image"));
    }

    #[test]
    fn post_rejects_non_object_and_bad_types() {
        let current = Config::default();
        assert!(merge_config_update(&current, &json!([1, 2])).is_err());
        assert!(merge_config_update(&current, &json!({ "thinking_tokens": "lots" })).is_err());
    }

    #[test]
    fn host_and_origin_checks() {
        // Same-origin browser requests and plain clients are fine.
        assert!(is_request_allowed(Some("127.0.0.1:8080"), None));
        assert!(is_request_allowed(Some("localhost:8080"), Some("http://localhost:8080")));
        assert!(is_request_allowed(Some("10.11.99.1:8080"), Some("http://10.11.99.1:8080")));
        assert!(is_request_allowed(Some("[::1]:8080"), Some("http://[::1]:8080")));
        assert!(is_request_allowed(Some("192.168.1.20"), None));

        // Cross-origin browser requests are rejected.
        assert!(!is_request_allowed(Some("127.0.0.1:8080"), Some("http://evil.example")));
        assert!(!is_request_allowed(Some("127.0.0.1:8080"), Some("null")));
        // DNS rebinding: attacker hostname in Host.
        assert!(!is_request_allowed(Some("evil.example:8080"), Some("http://evil.example:8080")));
        assert!(!is_request_allowed(Some("evil.example"), None));
        // Missing Host.
        assert!(!is_request_allowed(None, None));
    }

    #[test]
    fn strips_ports_from_host() {
        assert_eq!(host_without_port("localhost:8080"), "localhost");
        assert_eq!(host_without_port("localhost"), "localhost");
        assert_eq!(host_without_port("[::1]:8080"), "::1");
        assert_eq!(host_without_port("::1"), "::1");
    }
}
