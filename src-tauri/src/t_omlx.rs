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
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
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
/// Concurrent re-index workers hitting the server in parallel.
const REINDEX_CONCURRENCY: usize = 4;
/// Abort the re-index after this many consecutive failed batches.
const MAX_CONSECUTIVE_BATCH_ERRORS: usize = 3;
/// Page size when collecting the re-index work list.
const REINDEX_PAGE_SIZE: i64 = 512;

/// Model id stamped on embeddings produced by the built-in ONNX engine.
/// Rows written before the embed_model column existed are treated as this.
pub const BUILTIN_EMBED_MODEL: &str = "clip-vit-b32";

/// Search-score calibration for the oMLX provider. EmbeddingGemma-style
/// models spread cosine scores over a high, narrow band (measured on a real
/// library: relevant matches 0.70-0.79, related-but-wrong images 0.65-0.73,
/// absent concepts peaking around 0.67). Searches therefore keep only
/// results within `margin` of the best match, and a best match below
/// `floor` means the query has no real hits.
///
/// These defaults are hand-tuned for EmbeddingGemma 2; `calibrate` refines
/// them per model by measuring the text/image similarity band of the actual
/// library, so other server models work without manual tuning.
#[derive(Debug, Clone, Copy)]
pub struct ScoreCalibration {
    pub floor: f32,
    pub margin: f32,
}

impl Default for ScoreCalibration {
    fn default() -> Self {
        Self {
            floor: MIN_CONFIDENT_MATCH,
            margin: RELATIVE_MATCH_MARGIN,
        }
    }
}

pub const MIN_CONFIDENT_MATCH: f32 = 0.69;
pub const RELATIVE_MATCH_MARGIN: f32 = 0.04;

/// Generic probe concepts used to measure the model's score band against
/// the user's own photos.
const CALIBRATION_PROBE_QUERIES: [&str; 6] = [
    "a photo of a person",
    "a landscape scene",
    "a plate of food",
    "an animal",
    "a building",
    "a plant",
];
/// Stored-embedding sample size for calibration.
const CALIBRATION_SAMPLE_SIZE: i64 = 100;
/// Minimum sample the calibration accepts.
const CALIBRATION_MIN_VECTORS: usize = 20;

fn calibration_cell() -> &'static RwLock<Option<ScoreCalibration>> {
    static CALIBRATION: OnceLock<RwLock<Option<ScoreCalibration>>> = OnceLock::new();
    CALIBRATION.get_or_init(|| RwLock::new(None))
}

/// Calibration used by search filtering: the measured band when available,
/// the hand-tuned defaults otherwise.
pub fn effective_calibration() -> ScoreCalibration {
    calibration_cell()
        .read()
        .ok()
        .and_then(|cell| *cell)
        .unwrap_or_default()
}

/// Measure the score band in the background and store it for searches.
pub fn calibrate_async(settings: OmlxEmbeddingSettings) {
    std::thread::spawn(move || match calibrate(&settings) {
        Ok(calibration) => {
            println!(
                "Embedding score calibration for {}: floor={:.3} margin={:.3}",
                settings.model, calibration.floor, calibration.margin
            );
            if let Ok(mut cell) = calibration_cell().write() {
                *cell = Some(calibration);
            }
        }
        Err(error) => eprintln!("Embedding score calibration skipped: {}", error),
    });
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let (mut dot, mut na, mut nb) = (0.0f32, 0.0f32, 0.0f32);
    for (x, y) in a.iter().zip(b) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na * nb).sqrt()
    }
}

/// Estimate the floor ("confident best match") and margin ("close to the
/// best match") from random text/image similarity over the user's library:
/// floor = mean + 2.75·σ, margin = 1.5·σ, clamped around the hand-tuned
/// defaults so a degenerate sample cannot derail search.
fn calibrate(settings: &OmlxEmbeddingSettings) -> Result<ScoreCalibration, String> {
    let model_id = omlx_model_id(&settings.model, settings.dimensions);
    let vectors =
        crate::t_sqlite::AFile::sample_embedding_vectors(&model_id, CALIBRATION_SAMPLE_SIZE)?;
    if vectors.len() < CALIBRATION_MIN_VECTORS {
        return Err(format!(
            "only {} photos indexed for this model yet",
            vectors.len()
        ));
    }

    let mut probes = Vec::with_capacity(CALIBRATION_PROBE_QUERIES.len());
    for query in CALIBRATION_PROBE_QUERIES {
        probes.push(embed_query_text(settings, query)?);
    }

    let mut samples = Vec::with_capacity(probes.len() * vectors.len());
    for probe in &probes {
        for vector in &vectors {
            samples.push(cosine_similarity(probe, vector));
        }
    }
    let count = samples.len() as f32;
    let mean = samples.iter().sum::<f32>() / count;
    let variance = samples.iter().map(|s| (s - mean) * (s - mean)).sum::<f32>() / count;
    let sigma = variance.sqrt();

    let floor = (mean + 2.75 * sigma).clamp(MIN_CONFIDENT_MATCH - 0.06, MIN_CONFIDENT_MATCH + 0.06);
    let margin = (1.5 * sigma).clamp(RELATIVE_MATCH_MARGIN - 0.02, RELATIVE_MATCH_MARGIN + 0.03);
    Ok(ScoreCalibration { floor, margin })
}

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
    if let Some(active) = &settings {
        // Refresh the score band whenever the model or dimensions change.
        calibrate_async(active.clone());
    }
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

    // Collect the full work list up front so parallel workers can share it.
    let mut work: Vec<(i64, String)> = Vec::new();
    let mut last_id: i64 = 0;
    loop {
        let page = crate::t_sqlite::AFile::get_embedding_reindex_batch(
            last_id,
            REINDEX_PAGE_SIZE,
            &model_id,
        )?;
        if page.is_empty() {
            break;
        }
        last_id = page.last().map(|(id, _)| *id).unwrap_or(last_id);
        work.extend(page);
    }
    if let Ok(mut status) = status.lock() {
        status.total = work.len() as i64;
    }

    match omlx_settings {
        Some(settings) => run_reindex_omlx(app, cancel, library_id, status, settings, work),
        None => run_reindex_builtin(app, cancel, library_id, status, work),
    }
}

/// oMLX re-index: several workers pull batches from a shared queue and embed
/// them concurrently; the server batches each request internally.
fn run_reindex_omlx(
    app: &tauri::AppHandle,
    cancel: &Arc<AtomicBool>,
    library_id: &str,
    status: &Arc<Mutex<EmbeddingReindexStatus>>,
    settings: OmlxEmbeddingSettings,
    work: Vec<(i64, String)>,
) -> Result<(), String> {
    let queue: Arc<Mutex<VecDeque<(i64, String)>>> = Arc::new(Mutex::new(VecDeque::from(work)));
    let done = Arc::new(AtomicI64::new(0));
    let failed = Arc::new(AtomicI64::new(0));
    let consecutive_errors = Arc::new(AtomicUsize::new(0));
    let abort_reason: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

    std::thread::scope(|scope| {
        for _ in 0..REINDEX_CONCURRENCY {
            let queue = queue.clone();
            let done = done.clone();
            let failed = failed.clone();
            let consecutive_errors = consecutive_errors.clone();
            let abort_reason = abort_reason.clone();
            let cancel = cancel.clone();
            let status = status.clone();
            let settings = settings.clone();
            let app = app.clone();
            let library_id = library_id.to_string();
            scope.spawn(move || loop {
                if cancel.load(Ordering::SeqCst)
                    || consecutive_errors.load(Ordering::SeqCst) >= MAX_CONSECUTIVE_BATCH_ERRORS
                {
                    break;
                }
                if let Ok(reason) = abort_reason.lock() {
                    if reason.is_some() {
                        break;
                    }
                }
                if crate::t_config::current_library_id().ok().as_deref() != Some(library_id.as_str())
                {
                    if let Ok(mut reason) = abort_reason.lock() {
                        *reason = Some("Library changed".into());
                    }
                    break;
                }

                let mut chunk: Vec<(i64, String)> = Vec::with_capacity(REINDEX_BATCH_SIZE);
                if let Ok(mut queue) = queue.lock() {
                    for _ in 0..REINDEX_BATCH_SIZE {
                        match queue.pop_front() {
                            Some(item) => chunk.push(item),
                            None => break,
                        }
                    }
                }
                if chunk.is_empty() {
                    break;
                }

                // Resolve input bytes first (thumbnail preferred), then embed
                // the whole batch in a single request.
                let mut inputs: Vec<(i64, Vec<u8>)> = Vec::with_capacity(chunk.len());
                for (file_id, file_path) in &chunk {
                    match image_input_bytes(*file_id, file_path) {
                        Ok(bytes) => inputs.push((*file_id, bytes)),
                        Err(error) => {
                            failed.fetch_add(1, Ordering::SeqCst);
                            eprintln!("Embedding re-index skipped file {}: {}", file_id, error);
                        }
                    }
                }

                if !inputs.is_empty() {
                    let items: Vec<serde_json::Value> =
                        inputs.iter().map(|(_, bytes)| image_item(bytes)).collect();
                    match request_embeddings(&settings, &items) {
                        Ok(vectors) => {
                            consecutive_errors.store(0, Ordering::SeqCst);
                            let mut stored = 0i64;
                            for ((file_id, _), vector) in inputs.iter().zip(vectors.iter()) {
                                match crate::t_sqlite::AFile::update_embedding(
                                    *file_id,
                                    vector.clone(),
                                ) {
                                    Ok(_) => stored += 1,
                                    Err(error) => {
                                        failed.fetch_add(1, Ordering::SeqCst);
                                        eprintln!(
                                            "Embedding re-index failed to store {}: {}",
                                            file_id, error
                                        );
                                    }
                                }
                            }
                            done.fetch_add(stored, Ordering::SeqCst);
                        }
                        Err(error) => {
                            failed.fetch_add(inputs.len() as i64, Ordering::SeqCst);
                            let errors =
                                consecutive_errors.fetch_add(1, Ordering::SeqCst) + 1;
                            eprintln!("Embedding re-index batch failed: {}", error);
                            if errors >= MAX_CONSECUTIVE_BATCH_ERRORS
                                && let Ok(mut reason) = abort_reason.lock()
                            {
                                *reason = Some(error);
                            }
                        }
                    }
                }

                if let Ok(mut current) = status.lock() {
                    current.done = done.load(Ordering::SeqCst);
                    current.failed = failed.load(Ordering::SeqCst);
                    let _ = app.emit("embedding_reindex_progress", current.clone());
                }
            });
        }
    });

    if let Ok(reason) = abort_reason.lock() {
        if let Some(error) = reason.clone() {
            return Err(error);
        }
    }
    // The score band now reflects the freshly indexed model.
    calibrate_async(settings);
    Ok(())
}

/// Built-in re-index: the ONNX engine is mutex-serialized, so run files one
/// at a time through the regular embedding pipeline.
fn run_reindex_builtin(
    app: &tauri::AppHandle,
    cancel: &Arc<AtomicBool>,
    library_id: &str,
    status: &Arc<Mutex<EmbeddingReindexStatus>>,
    work: Vec<(i64, String)>,
) -> Result<(), String> {
    if work.is_empty() {
        return Ok(());
    }
    crate::t_startup::ensure_ai(app, None)?;
    let ai_state: tauri::State<'_, crate::t_ai::AiState> = app.state();
    let (mut done, mut failed) = (0i64, 0i64);

    for (file_id, _) in work {
        if cancel.load(Ordering::SeqCst) {
            return Ok(());
        }
        if crate::t_config::current_library_id().ok().as_deref() != Some(library_id) {
            return Err("Library changed".into());
        }
        match crate::t_sqlite::AFile::generate_embedding(&ai_state, file_id) {
            Ok(_) => done += 1,
            Err(error) => {
                failed += 1;
                eprintln!("Embedding re-index failed for {}: {}", file_id, error);
            }
        }
        if done % 16 == 0
            && let Ok(mut current) = status.lock()
        {
            current.done = done;
            current.failed = failed;
            let _ = app.emit("embedding_reindex_progress", current.clone());
        }
    }

    if let Ok(mut current) = status.lock() {
        current.done = done;
        current.failed = failed;
        let _ = app.emit("embedding_reindex_progress", current.clone());
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
