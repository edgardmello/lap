/**
 * Optional oMLX embedding provider for image search.
 * When configured, text and image embeddings are computed by a local oMLX
 * server (OpenAI-compatible /v1/embeddings endpoint) instead of the bundled
 * ONNX CLIP models. Everything still runs on the user's machine; the server
 * URL is expected to point at localhost or a trusted LAN host.
 * project: Lap
 */
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::Duration;
use tauri::{Emitter, Manager};

const DEFAULT_URL: &str = "http://127.0.0.1:8000";
pub const DEFAULT_MODEL: &str = "mlx-community/embeddinggemma-2-bf16";

/// EmbeddingGemma task prompt for retrieval queries, from the model's
/// sentence-transformers prompt configuration.
const EMBEDDINGGEMMA_QUERY_PROMPT: &str = "task: search result | query: ";

const HEALTH_TIMEOUT: Duration = Duration::from_secs(8);
const EMBED_TIMEOUT: Duration = Duration::from_secs(120);
/// Images embedded per request when re-indexing with the oMLX provider.
const REINDEX_BATCH_SIZE: usize = 8;
/// Abort the re-index after this many consecutive failed batches.
const MAX_CONSECUTIVE_BATCH_ERRORS: usize = 3;

/// Model id stamped on embeddings produced by the built-in ONNX engine.
/// Rows written before the embed_model column existed are treated as this.
pub const BUILTIN_EMBED_MODEL: &str = "clip-vit-b32";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OmlxEmbeddingSettings {
    #[serde(default = "default_url")]
    pub url: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default = "default_model")]
    pub model: String,
    /// Optional Matryoshka truncation (512 / 256 / 128 for EmbeddingGemma 2).
    #[serde(default)]
    pub dimensions: Option<u32>,
}

fn default_url() -> String {
    DEFAULT_URL.to_string()
}

fn default_model() -> String {
    DEFAULT_MODEL.to_string()
}

impl OmlxEmbeddingSettings {
    pub fn normalized(&self) -> Result<OmlxEmbeddingSettings, String> {
        let mut settings = self.clone();
        settings.url = settings.url.trim().trim_end_matches('/').to_string();
        settings.model = settings.model.trim().to_string();
        settings.api_key = settings.api_key.trim().to_string();
        if !settings.url.starts_with("http://") && !settings.url.starts_with("https://") {
            return Err("The oMLX server URL must start with http:// or https://".to_string());
        }
        if settings.model.is_empty() {
            return Err("The oMLX embedding model name is required".to_string());
        }
        if let Some(dimensions) = settings.dimensions {
            if dimensions == 0 {
                settings.dimensions = None;
            }
        }
        Ok(settings)
    }

    fn model_id(&self) -> String {
        omlx_model_id(&self.model, self.dimensions)
    }
}

fn omlx_model_id(model: &str, dimensions: Option<u32>) -> String {
    match dimensions {
        Some(dimensions) => format!("omlx:{}:{}", model, dimensions),
        None => format!("omlx:{}", model),
    }
}

// ============================================================================
// Active provider configuration
// ============================================================================

fn active_settings_cell() -> &'static RwLock<Option<OmlxEmbeddingSettings>> {
    static ACTIVE: OnceLock<RwLock<Option<OmlxEmbeddingSettings>>> = OnceLock::new();
    ACTIVE.get_or_init(|| RwLock::new(None))
}

/// Install (or, with None, remove) the oMLX embedding provider.
pub fn apply_settings(settings: Option<OmlxEmbeddingSettings>) -> Result<(), String> {
    let settings = match settings {
        Some(settings) => Some(settings.normalized()?),
        None => None,
    };
    *active_settings_cell()
        .write()
        .map_err(|e| format!("Embedding provider lock poisoned: {}", e))? = settings;
    Ok(())
}

/// Currently configured oMLX settings, if the provider is active.
pub fn active_settings() -> Option<OmlxEmbeddingSettings> {
    active_settings_cell().read().ok().and_then(|v| v.clone())
}

pub fn is_active() -> bool {
    active_settings().is_some()
}

/// Model id of the embeddings the app currently produces. Stored in
/// afiles.embed_model so vectors from different providers never mix.
pub fn active_model_id() -> String {
    match active_settings() {
        Some(settings) => settings.model_id(),
        None => BUILTIN_EMBED_MODEL.to_string(),
    }
}

// ============================================================================
// HTTP client
// ============================================================================

fn http_client(timeout: Duration) -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|e| format!("Failed to create HTTP client: {}", e))
}

fn truncate(text: &str, max: usize) -> String {
    if text.len() <= max {
        text.to_string()
    } else {
        let mut cut = max;
        while cut > 0 && !text.is_char_boundary(cut) {
            cut -= 1;
        }
        format!("{}...", &text[..cut])
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OmlxConnectionStatus {
    pub ok: bool,
    pub model_available: bool,
    pub models: Vec<String>,
    pub error: Option<String>,
}

/// Reach out to the server and report which models it currently serves.
pub fn test_connection(settings: &OmlxEmbeddingSettings) -> OmlxConnectionStatus {
    let settings = match settings.normalized() {
        Ok(settings) => settings,
        Err(error) => {
            return OmlxConnectionStatus {
                ok: false,
                model_available: false,
                models: Vec::new(),
                error: Some(error),
            };
        }
    };

    let request = match http_client(HEALTH_TIMEOUT) {
        Ok(client) => client
            .get(format!("{}/v1/models", settings.url))
            .bearer_auth(&settings.api_key),
        Err(error) => {
            return OmlxConnectionStatus {
                ok: false,
                model_available: false,
                models: Vec::new(),
                error: Some(error),
            };
        }
    };

    match request.send() {
        Ok(response) => {
            let status = response.status();
            let body = response.text().unwrap_or_default();
            if !status.is_success() {
                return OmlxConnectionStatus {
                    ok: false,
                    model_available: false,
                    models: Vec::new(),
                    error: Some(format!("Server responded with {}: {}", status.as_u16(), truncate(body.trim(), 300))),
                };
            }
            let models = serde_json::from_str::<serde_json::Value>(&body)
                .ok()
                .and_then(|value| {
                    value
                        .get("data")
                        .and_then(|data| data.as_array())
                        .map(|entries| {
                            entries
                                .iter()
                                .filter_map(|entry| {
                                    entry.get("id").and_then(|id| id.as_str()).map(String::from)
                                })
                                .collect::<Vec<_>>()
                        })
                })
                .unwrap_or_default();
            let model_available = models
                .iter()
                .any(|id| id == &settings.model || id.split(':').next() == Some(settings.model.as_str()));
            OmlxConnectionStatus {
                ok: true,
                model_available,
                models,
                error: None,
            }
        }
        Err(error) => OmlxConnectionStatus {
            ok: false,
            model_available: false,
            models: Vec::new(),
            error: Some(format!("Cannot reach the oMLX server: {}", error)),
        },
    }
}

/// POST /v1/embeddings with structured items and return one vector per item.
fn request_embeddings(
    settings: &OmlxEmbeddingSettings,
    items: &[serde_json::Value],
) -> Result<Vec<Vec<f32>>, String> {
    let mut body = json!({ "model": settings.model, "items": items });
    if let Some(dimensions) = settings.dimensions {
        body["dimensions"] = json!(dimensions);
    }

    let client = http_client(EMBED_TIMEOUT)?;
    let mut request = client
        .post(format!("{}/v1/embeddings", settings.url))
        .json(&body);
    if !settings.api_key.is_empty() {
        request = request.bearer_auth(&settings.api_key);
    }

    let response = request
        .send()
        .map_err(|e| format!("oMLX embeddings request failed: {}", e))?;
    let status = response.status();
    let body = response
        .text()
        .map_err(|e| format!("Failed to read oMLX response: {}", e))?;
    if !status.is_success() {
        return Err(format!(
            "oMLX embeddings error ({}): {}",
            status.as_u16(),
            truncate(body.trim(), 300)
        ));
    }

    let parsed = serde_json::from_str::<serde_json::Value>(&body)
        .map_err(|e| format!("Failed to parse oMLX embeddings response: {}", e))?;
    let data = parsed
        .get("data")
        .and_then(|data| data.as_array())
        .ok_or_else(|| "oMLX embeddings response has no data array".to_string())?;

    let mut vectors: Vec<(usize, Vec<f32>)> = Vec::with_capacity(data.len());
    for entry in data {
        let index = entry
            .get("index")
            .and_then(|index| index.as_u64())
            .ok_or_else(|| "oMLX embeddings entry has no index".to_string())? as usize;
        let vector = entry
            .get("embedding")
            .and_then(|embedding| embedding.as_array())
            .ok_or_else(|| "oMLX embeddings entry has no embedding".to_string())?
            .iter()
            .map(|value| {
                value
                    .as_f64()
                    .map(|number| number as f32)
                    .ok_or_else(|| "oMLX embedding contains a non-numeric value".to_string())
            })
            .collect::<Result<Vec<f32>, String>>()?;
        vectors.push((index, vector));
    }
    vectors.sort_by_key(|(index, _)| *index);

    if vectors.len() != items.len() {
        return Err(format!(
            "oMLX returned {} embeddings for {} items",
            vectors.len(),
            items.len()
        ));
    }

    Ok(vectors.into_iter().map(|(_, vector)| vector).collect())
}

fn uses_embeddinggemma_prompt(settings: &OmlxEmbeddingSettings) -> bool {
    settings.model.to_lowercase().contains("embeddinggemma")
}

/// Embed a user query. EmbeddingGemma models expect the retrieval query
/// task prompt; other models receive the raw text.
pub fn embed_query_text(settings: &OmlxEmbeddingSettings, text: &str) -> Result<Vec<f32>, String> {
    let input = if uses_embeddinggemma_prompt(settings) {
        format!("{}{}", EMBEDDINGGEMMA_QUERY_PROMPT, text)
    } else {
        text.to_string()
    };
    let mut vectors = request_embeddings(settings, &[json!({ "text": input })])?;
    vectors
        .pop()
        .ok_or_else(|| "oMLX returned no embedding for the query".to_string())
}

fn image_mime(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        "image/jpeg"
    } else if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        "image/png"
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        "image/webp"
    } else if bytes.len() >= 12 && &bytes[4..8] == b"ftyp" {
        "image/heic"
    } else {
        "image/jpeg"
    }
}

fn image_item(bytes: &[u8]) -> serde_json::Value {
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    json!({ "image": format!("data:{};base64,{}", image_mime(bytes), encoded) })
}

/// Embed a single image (thumbnail bytes or raw file contents).
pub fn embed_single_image(settings: &OmlxEmbeddingSettings, bytes: &[u8]) -> Result<Vec<f32>, String> {
    let mut vectors = request_embeddings(settings, &[image_item(bytes)])?;
    vectors
        .pop()
        .ok_or_else(|| "oMLX returned no embedding for the image".to_string())
}

// ============================================================================
// Embedding re-index scan
// ============================================================================

#[derive(Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct EmbeddingReindexStatus {
    pub state: String, // running | finished | cancelled | error | idle
    pub done: i64,
    pub total: i64,
    pub failed: i64,
    pub model: String,
    pub error: Option<String>,
}

/// Shared scan state, managed by Tauri. The Arc fields are cloned into the
/// worker thread so the managed state always reflects live progress.
pub struct EmbeddingReindexState {
    running: Arc<AtomicBool>,
    cancel: Arc<AtomicBool>,
    status: Arc<Mutex<EmbeddingReindexStatus>>,
}

impl Default for EmbeddingReindexState {
    fn default() -> Self {
        Self {
            running: Arc::new(AtomicBool::new(false)),
            cancel: Arc::new(AtomicBool::new(false)),
            status: Arc::new(Mutex::new(EmbeddingReindexStatus {
                state: "idle".into(),
                ..Default::default()
            })),
        }
    }
}

impl EmbeddingReindexState {
    pub fn status(&self) -> EmbeddingReindexStatus {
        self.status
            .lock()
            .map(|status| status.clone())
            .unwrap_or_default()
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }
}

pub fn spawn_reindex(
    app: tauri::AppHandle,
    state: tauri::State<'_, EmbeddingReindexState>,
    library_id: String,
) -> Result<(), String> {
    if state
        .running
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Err("An embedding re-index is already running.".into());
    }
    state.cancel.store(false, Ordering::SeqCst);
    if let Ok(mut status) = state.status.lock() {
        *status = EmbeddingReindexStatus {
            state: "running".into(),
            model: active_model_id(),
            ..Default::default()
        };
    }
    let running = state.running.clone();
    let cancel = state.cancel.clone();
    let status_shared = state.status.clone();

    std::thread::spawn(move || {
        let result = run_reindex(&app, &cancel, &library_id, &status_shared);
        let final_state = match &result {
            Ok(()) => {
                if cancel.load(Ordering::SeqCst) {
                    "cancelled"
                } else {
                    "finished"
                }
            }
            Err(_) => "error",
        };
        if let Ok(mut status) = status_shared.lock() {
            status.state = final_state.into();
            if let Err(error) = result {
                status.error = Some(error);
            }
            let _ = app.emit("embedding_reindex_progress", status.clone());
        }
        running.store(false, Ordering::SeqCst);
    });
    Ok(())
}

fn run_reindex(
    app: &tauri::AppHandle,
    cancel: &Arc<AtomicBool>,
    library_id: &str,
    status: &Arc<Mutex<EmbeddingReindexStatus>>,
) -> Result<(), String> {
    let model_id = active_model_id();
    let omlx_settings = active_settings();
    let total = crate::t_sqlite::AFile::count_embedding_reindex(&model_id)?;
    if let Ok(mut status) = status.lock() {
        status.total = total;
    }

    let batch_size = if omlx_settings.is_some() {
        REINDEX_BATCH_SIZE
    } else {
        1
    };
    let mut last_id: i64 = 0;
    let mut done: i64 = 0;
    let mut failed: i64 = 0;
    let mut consecutive_errors: usize = 0;

    loop {
        if cancel.load(Ordering::SeqCst) {
            return Ok(());
        }
        if crate::t_config::current_library_id().ok().as_deref() != Some(library_id) {
            return Err("Library changed".into());
        }

        let batch = crate::t_sqlite::AFile::get_embedding_reindex_batch(
            last_id,
            batch_size as i64,
            &model_id,
        )?;
        if batch.is_empty() {
            break;
        }
        last_id = batch.last().map(|(id, _)| *id).unwrap_or(last_id);

        if let Some(settings) = &omlx_settings {
            // Resolve input bytes first (thumbnail preferred), then embed the
            // whole batch in a single request.
            let mut inputs: Vec<(i64, Vec<u8>)> = Vec::with_capacity(batch.len());
            for (file_id, file_path) in &batch {
                match image_input_bytes(*file_id, file_path) {
                    Ok(bytes) => inputs.push((*file_id, bytes)),
                    Err(error) => {
                        failed += 1;
                        eprintln!("Embedding re-index skipped file {}: {}", file_id, error);
                    }
                }
            }

            if !inputs.is_empty() {
                let items: Vec<serde_json::Value> =
                    inputs.iter().map(|(_, bytes)| image_item(bytes)).collect();
                match request_embeddings(settings, &items) {
                    Ok(vectors) => {
                        consecutive_errors = 0;
                        for ((file_id, _), vector) in inputs.iter().zip(vectors.iter()) {
                            match crate::t_sqlite::AFile::update_embedding(*file_id, vector.clone())
                            {
                                Ok(_) => done += 1,
                                Err(error) => {
                                    failed += 1;
                                    eprintln!("Embedding re-index failed to store {}: {}", file_id, error);
                                }
                            }
                        }
                    }
                    Err(error) => {
                        consecutive_errors += 1;
                        failed += inputs.len() as i64;
                        eprintln!("Embedding re-index batch failed: {}", error);
                        if consecutive_errors >= MAX_CONSECUTIVE_BATCH_ERRORS {
                            return Err(error);
                        }
                    }
                }
            }
        } else {
            // Built-in provider: encode through the ONNX engine one file at a
            // time; AFile::generate_embedding handles thumbnails and storage.
            if let Err(error) = crate::t_startup::ensure_ai(app, None) {
                return Err(format!("Failed to initialize AI for re-index: {}", error));
            }
            let ai_state: tauri::State<'_, crate::t_ai::AiState> = app.state();
            for (file_id, _) in &batch {
                match crate::t_sqlite::AFile::generate_embedding(&ai_state, *file_id) {
                    Ok(_) => done += 1,
                    Err(error) => {
                        failed += 1;
                        eprintln!("Embedding re-index failed for {}: {}", file_id, error);
                    }
                }
            }
        }

        if let Ok(mut status) = status.lock() {
            status.done = done;
            status.failed = failed;
        }
        if let Ok(status) = status.lock() {
            let _ = app.emit("embedding_reindex_progress", status.clone());
        }
    }

    Ok(())
}

/// Thumbnail bytes when available (much smaller), otherwise the original file.
fn image_input_bytes(file_id: i64, file_path: &str) -> Result<Vec<u8>, String> {
    if let Ok(Some(thumb)) = crate::t_sqlite::AThumb::fetch(file_id) {
        if let Some(thumb_data) = thumb.thumb_data {
            if !thumb_data.is_empty() {
                return Ok(thumb_data);
            }
        }
    }
    std::fs::read(file_path).map_err(|e| format!("Failed to read image file: {}", e))
}
