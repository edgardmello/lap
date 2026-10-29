/**
 * Optional embedding server provider for image search.
 * When configured, text and image embeddings are computed by a local
 * embedding server instead of the bundled ONNX CLIP models. Three APIs are
 * supported: oMLX (/v1/embeddings with structured items), Ollama
 * (/api/embed) and generic OpenAI-compatible servers (/v1/embeddings).
 * Everything still runs on the user's machine; the server URL is expected
 * to point at localhost or a trusted LAN host.
 * project: Lap
 */
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, RwLock, mpsc};
use std::time::Duration;
use tauri::{Emitter, Manager};

const DEFAULT_URL: &str = "http://127.0.0.1:8000";
pub const DEFAULT_MODEL: &str = "mlx-community/embeddinggemma-2-bf16";

/// EmbeddingGemma task prompt for retrieval queries, from the model's
/// sentence-transformers prompt configuration.
const EMBEDDINGGEMMA_QUERY_PROMPT: &str = "task: search result | query: ";

/// Listing models is quick, but a server that is still loading the model
/// (the calibration started by switching providers loads it) can hold the
/// request for 10+ s.
const HEALTH_TIMEOUT: Duration = Duration::from_secs(30);
const EMBED_TIMEOUT: Duration = Duration::from_secs(120);
/// Images embedded per request when re-indexing with an embedding server.
/// Measured on an M1 Pro, one image per request already keeps the GPU at
/// ~98%, so larger batches barely add throughput (8 x 4 was ~2% faster
/// than 2 x 2) while holding ~5 GB of activations and making a text search
/// wait ~15 s behind them; 2 x 2 keeps searches near 1 s with no memory
/// pressure, and two workers let one prepare thumbnails while the other
/// waits on the GPU.
const REINDEX_BATCH_SIZE: usize = 2;
/// Concurrent re-index workers hitting the server in parallel.
const REINDEX_CONCURRENCY: usize = 2;
/// Abort the re-index after this many consecutive failed batches.
const MAX_CONSECUTIVE_BATCH_ERRORS: usize = 3;
/// Page size when collecting the re-index work list.
const REINDEX_PAGE_SIZE: i64 = 512;
/// Files the indexer may have waiting on embeddings at once with an
/// embedding server: enough to fill every worker's batch.
pub const INDEX_CONCURRENT_FILES: usize = REINDEX_CONCURRENCY * REINDEX_BATCH_SIZE;

/// Model id stamped on embeddings produced by the built-in ONNX engine.
/// Rows written before the embed_model column existed are treated as this.
pub const BUILTIN_EMBED_MODEL: &str = "clip-vit-b32";

/// Search-score calibration for embedding servers. EmbeddingGemma-style
/// models spread cosine scores over a high, narrow band (measured on a real
/// library: relevant matches 0.70-0.79, related-but-wrong images 0.65-0.73,
/// absent concepts peaking around 0.67). Searches therefore keep only
/// results within `margin` of the best match, and a best match below
/// `floor` means the query has no real hits. `baseline` is the mean score
/// of the calibration probes against the library; text queries that score
/// lower against everything (long, descriptive prompts) get their floor
/// lowered by the difference.
///
/// These defaults are hand-tuned for EmbeddingGemma 2; `calibrate` refines
/// them per model by measuring the text/image similarity band of the actual
/// library, so other server models work without manual tuning.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScoreCalibration {
    pub floor: f32,
    pub margin: f32,
    pub baseline: f32,
}

impl Default for ScoreCalibration {
    fn default() -> Self {
        Self {
            floor: MIN_CONFIDENT_MATCH,
            margin: RELATIVE_MATCH_MARGIN,
            baseline: PROBE_BASELINE,
        }
    }
}

pub const MIN_CONFIDENT_MATCH: f32 = 0.69;
pub const RELATIVE_MATCH_MARGIN: f32 = 0.04;
pub const PROBE_BASELINE: f32 = 0.60;

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
/// Stored-embedding sample size for calibration. Larger samples keep the
/// measured band stable across recalibrations (the floor moves by less than
/// ~0.005 at this size on a several-thousand-photo library).
const CALIBRATION_SAMPLE_SIZE: i64 = 300;
/// Minimum sample the calibration accepts.
const CALIBRATION_MIN_VECTORS: usize = 20;

/// The score band depends on both the model (and its dimensions) and the
/// library's content, so each pair is measured separately.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CalibrationKey {
    library_id: String,
    model_id: String,
}

/// Measured score bands plus the bookkeeping that keeps measurements from
/// piling up. Every measurement gets a generation number; a result only
/// lands when nothing newer has been started or stored for its key, so a
/// slow measurement over a half-indexed library cannot overwrite the one
/// taken after the re-index finished.
#[derive(Default)]
struct CalibrationStore {
    /// None records a measurement that found too few indexed photos; it is
    /// retried only when forced (settings change or re-index).
    results: HashMap<CalibrationKey, (u64, Option<ScoreCalibration>)>,
    pending: HashMap<CalibrationKey, u64>,
    generation: u64,
}

impl CalibrationStore {
    /// Generation for a new measurement, or None when one is not needed.
    fn begin(&mut self, key: &CalibrationKey, force: bool) -> Option<u64> {
        if !force && (self.results.contains_key(key) || self.pending.contains_key(key)) {
            return None;
        }
        self.generation += 1;
        self.pending.insert(key.clone(), self.generation);
        Some(self.generation)
    }

    /// Record a finished measurement; returns whether it was kept.
    fn finish(
        &mut self,
        key: &CalibrationKey,
        generation: u64,
        result: Option<ScoreCalibration>,
    ) -> bool {
        let newer_pending = self.pending.get(key).is_some_and(|&g| g > generation);
        if self.pending.get(key) == Some(&generation) {
            self.pending.remove(key);
        }
        let newer_stored = self.results.get(key).is_some_and(|(g, _)| *g > generation);
        if newer_pending || newer_stored {
            return false;
        }
        self.results.insert(key.clone(), (generation, result));
        true
    }

    /// Forget a failed measurement so the next request can retry it.
    fn abandon(&mut self, key: &CalibrationKey, generation: u64) {
        if self.pending.get(key) == Some(&generation) {
            self.pending.remove(key);
        }
    }

    fn get(&self, key: &CalibrationKey) -> Option<Option<ScoreCalibration>> {
        self.results.get(key).map(|(_, result)| *result)
    }
}

fn calibration_store() -> &'static Mutex<CalibrationStore> {
    static STORE: OnceLock<Mutex<CalibrationStore>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(CalibrationStore::default()))
}

fn calibration_key(settings: &EmbeddingServerSettings, library_id: &str) -> CalibrationKey {
    CalibrationKey {
        library_id: library_id.to_string(),
        model_id: settings.model_id(),
    }
}

/// Score band for searching `library_id` with `settings`. The first search
/// of a library/model pair starts a background measurement and uses the
/// hand-tuned defaults until it lands.
pub fn calibration_for(settings: &EmbeddingServerSettings, library_id: &str) -> ScoreCalibration {
    let key = calibration_key(settings, library_id);
    let stored = calibration_store().lock().ok().and_then(|store| store.get(&key));
    match stored {
        Some(result) => result.unwrap_or_default(),
        None => {
            request_calibration(settings.clone(), library_id, false);
            ScoreCalibration::default()
        }
    }
}

/// Measure the score band for a library/model pair in the background.
/// Without `force`, nothing happens when a result exists or a measurement
/// is already running.
pub fn request_calibration(settings: EmbeddingServerSettings, library_id: &str, force: bool) {
    let key = calibration_key(&settings, library_id);
    let Some(generation) = calibration_store()
        .lock()
        .ok()
        .and_then(|mut store| store.begin(&key, force))
    else {
        return;
    };

    std::thread::spawn(move || {
        let result = calibrate(&settings, &key.library_id);
        let Ok(mut store) = calibration_store().lock() else { return };
        match result {
            Ok(calibration) => {
                if store.finish(&key, generation, calibration) {
                    match calibration {
                        Some(c) => println!(
                            "Embedding score calibration for {} in {}: floor={:.3} margin={:.3}",
                            key.model_id, key.library_id, c.floor, c.margin
                        ),
                        None => println!(
                            "Embedding score calibration for {} in {}: too few photos indexed, using defaults",
                            key.model_id, key.library_id
                        ),
                    }
                }
            }
            Err(error) => {
                store.abandon(&key, generation);
                eprintln!("Embedding score calibration skipped: {}", error);
            }
        }
    });
}

pub(crate) fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
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
/// floor = mean + 2.75·σ, margin = 1.5·σ. The band depends heavily on the
/// library's content (measured means range from ~0.53 on a documents-heavy
/// library to ~0.60 on a colorful photo set), so the clamps are wide safety
/// rails rather than a window around the hand-tuned defaults.
///
/// Ok(None) means the library has too few photos indexed for this model.
fn calibrate(
    settings: &EmbeddingServerSettings,
    library_id: &str,
) -> Result<Option<ScoreCalibration>, String> {
    let probes = calibration_probes(settings)?;
    let dimensions = probes.first().map_or(0, Vec::len);
    let model_id = settings.model_id();
    let vectors = crate::t_cmds::with_current_library(library_id, || {
        crate::t_sqlite::AFile::sample_embedding_vectors(
            &model_id,
            dimensions,
            CALIBRATION_SAMPLE_SIZE,
        )
    })?;
    Ok(measure_band(&probes, &vectors))
}

pub(crate) fn calibration_probes(settings: &EmbeddingServerSettings) -> Result<Vec<Vec<f32>>, String> {
    CALIBRATION_PROBE_QUERIES
        .iter()
        .map(|query| embed_query_text(settings, query))
        .collect()
}

/// Score band from probe/library similarities; None when the sample is too
/// small to trust.
pub(crate) fn measure_band(probes: &[Vec<f32>], vectors: &[Vec<f32>]) -> Option<ScoreCalibration> {
    if vectors.len() < CALIBRATION_MIN_VECTORS || probes.is_empty() {
        return None;
    }
    let mut samples = Vec::with_capacity(probes.len() * vectors.len());
    for probe in probes {
        for vector in vectors {
            samples.push(cosine_similarity(probe, vector));
        }
    }
    let count = samples.len() as f32;
    let mean = samples.iter().sum::<f32>() / count;
    let variance = samples.iter().map(|s| (s - mean) * (s - mean)).sum::<f32>() / count;
    let sigma = variance.sqrt();

    let floor = (mean + 2.75 * sigma).clamp(0.55, 0.80);
    let margin = (1.5 * sigma).clamp(0.02, 0.10);
    Some(ScoreCalibration { floor, margin, baseline: mean })
}

/// Wire format of the embedding server.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ServerApi {
    /// oMLX: POST /v1/embeddings with `items` of `{text}` / `{image}`
    /// (data URI) objects.
    #[default]
    Omlx,
    /// Ollama: POST /api/embed with `input` mixing strings and `{image}`
    /// objects (raw base64).
    Ollama,
    /// Generic OpenAI-compatible server (LM Studio, LocalAI, vLLM, llama.cpp,
    /// Jina, ...): POST /v1/embeddings with `input` mixing strings and
    /// `{image}` objects (data URI). Images need a server and model that
    /// accept image inputs.
    Openai,
}

impl ServerApi {
    /// Name used in error messages.
    pub fn label(self) -> &'static str {
        match self {
            ServerApi::Omlx => "oMLX",
            ServerApi::Ollama => "Ollama",
            ServerApi::Openai => "embedding server",
        }
    }

    /// Prefix of the model ids stamped on stored vectors.
    fn id_prefix(self) -> &'static str {
        match self {
            ServerApi::Omlx => "omlx",
            ServerApi::Ollama => "ollama",
            ServerApi::Openai => "openai",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbeddingServerSettings {
    #[serde(default)]
    pub api: ServerApi,
    #[serde(default = "default_url")]
    pub url: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default = "default_model")]
    pub model: String,
    /// Optional Matryoshka truncation (512 / 256 / 128 for EmbeddingGemma 2).
    #[serde(default)]
    pub dimensions: Option<u32>,
    /// Optional chat model on the same server that translates searches to
    /// English before they are embedded; empty disables translation.
    #[serde(default)]
    pub translation_model: String,
}

fn default_url() -> String {
    DEFAULT_URL.to_string()
}

fn default_model() -> String {
    DEFAULT_MODEL.to_string()
}

impl EmbeddingServerSettings {
    pub fn normalized(&self) -> Result<EmbeddingServerSettings, String> {
        let mut settings = self.clone();
        settings.url = settings.url.trim().trim_end_matches('/').to_string();
        // Ollama's native API lives next to its OpenAI layer, so a pasted
        // OpenAI base URL still works.
        if settings.api == ServerApi::Ollama
            && let Some(base) = settings.url.strip_suffix("/v1")
        {
            settings.url = base.to_string();
        }
        settings.model = settings.model.trim().to_string();
        settings.api_key = settings.api_key.trim().to_string();
        settings.translation_model = settings.translation_model.trim().to_string();
        let label = settings.api.label();
        if !settings.url.starts_with("http://") && !settings.url.starts_with("https://") {
            return Err(format!("The {} URL must start with http:// or https://", label));
        }
        if settings.model.is_empty() {
            return Err(format!("The {} embedding model name is required", label));
        }
        if let Some(dimensions) = settings.dimensions {
            if dimensions == 0 {
                settings.dimensions = None;
            }
        }
        Ok(settings)
    }

    /// Versioned id stamped on the vectors this configuration produces.
    pub fn model_id(&self) -> String {
        let prefix = self.api.id_prefix();
        match self.dimensions {
            Some(dimensions) => format!("{}:{}:{}", prefix, self.model, dimensions),
            None => format!("{}:{}", prefix, self.model),
        }
    }

    /// URL of an OpenAI-style endpoint (`path` like "/embeddings"); accepts
    /// base URLs with or without the trailing /v1.
    fn openai_url(&self, path: &str) -> String {
        if self.url.ends_with("/v1") {
            format!("{}{}", self.url, path)
        } else {
            format!("{}/v1{}", self.url, path)
        }
    }

    fn authorized(&self, request: reqwest::blocking::RequestBuilder) -> reqwest::blocking::RequestBuilder {
        if self.api_key.is_empty() {
            request
        } else {
            request.bearer_auth(&self.api_key)
        }
    }
}

/// Size of the vectors `settings` produces: the requested dimensions, or for
/// full-size output the size of one probe embedding, cached per model.
pub fn expected_dimensions(settings: &EmbeddingServerSettings) -> Result<usize, String> {
    if let Some(dimensions) = settings.dimensions {
        return Ok(dimensions as usize);
    }
    static CACHE: OnceLock<Mutex<HashMap<String, usize>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let model_id = settings.model_id();
    if let Some(&dimensions) = cache.lock().ok().as_ref().and_then(|c| c.get(&model_id)) {
        return Ok(dimensions);
    }
    let dimensions = embed_query_text(settings, "dimension probe")?.len();
    if let Ok(mut cache) = cache.lock() {
        cache.insert(model_id, dimensions);
    }
    Ok(dimensions)
}

/// Model id for a provider snapshot (None = built-in engine). Operations take
/// one snapshot up front and label everything they write with it, so a
/// settings change mid-operation cannot mislabel vectors.
pub fn embed_model_id(settings: Option<&EmbeddingServerSettings>) -> String {
    settings.map_or_else(|| BUILTIN_EMBED_MODEL.to_string(), EmbeddingServerSettings::model_id)
}

// ============================================================================
// Active provider configuration
// ============================================================================

fn active_settings_cell() -> &'static RwLock<Option<EmbeddingServerSettings>> {
    static ACTIVE: OnceLock<RwLock<Option<EmbeddingServerSettings>>> = OnceLock::new();
    ACTIVE.get_or_init(|| RwLock::new(None))
}

/// Install (or, with None, remove) the embedding server provider. Re-applying
/// the current settings is a no-op; the frontend does so before searches.
pub fn apply_settings(settings: Option<EmbeddingServerSettings>) -> Result<(), String> {
    let settings = match settings {
        Some(settings) => Some(settings.normalized()?),
        None => None,
    };
    {
        let mut active = active_settings_cell()
            .write()
            .map_err(|e| format!("Embedding provider lock poisoned: {}", e))?;
        if *active == settings {
            return Ok(());
        }
        *active = settings.clone();
    }
    // Warm the score band so the first search is already calibrated; a pair
    // measured earlier in this session is reused as-is.
    if let (Some(active), Ok(library_id)) = (settings, crate::t_config::current_library_id()) {
        request_calibration(active, &library_id, false);
    }
    Ok(())
}

/// Currently configured server settings, if the provider is active.
pub fn active_settings() -> Option<EmbeddingServerSettings> {
    active_settings_cell().read().ok().and_then(|v| v.clone())
}

pub fn is_active() -> bool {
    active_settings().is_some()
}

/// Model id of the embeddings the app currently produces. Stored in
/// afiles.embed_model so vectors from different providers never mix.
pub fn active_model_id() -> String {
    embed_model_id(active_settings().as_ref())
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
pub struct ServerConnectionStatus {
    pub ok: bool,
    pub model_available: bool,
    pub models: Vec<String>,
    /// Whether the model embedded a probe image; None when it was not tried.
    pub images_supported: Option<bool>,
    pub error: Option<String>,
}

impl ServerConnectionStatus {
    fn failed(error: String) -> Self {
        Self {
            ok: false,
            model_available: false,
            models: Vec::new(),
            images_supported: None,
            error: Some(error),
        }
    }
}

/// Reach out to the server, report which models it currently serves, and
/// check that the configured model accepts images: a text-only embedding
/// model would otherwise only fail once a re-index starts.
pub fn test_connection(settings: &EmbeddingServerSettings) -> ServerConnectionStatus {
    let settings = match settings.normalized() {
        Ok(settings) => settings,
        Err(error) => return ServerConnectionStatus::failed(error),
    };
    let client = match http_client(HEALTH_TIMEOUT) {
        Ok(client) => client,
        Err(error) => return ServerConnectionStatus::failed(error),
    };
    let url = match settings.api {
        ServerApi::Ollama => format!("{}/api/tags", settings.url),
        ServerApi::Omlx | ServerApi::Openai => settings.openai_url("/models"),
    };

    // A failed listing does not end the test: not every OpenAI-compatible
    // server lists its models, and a busy server may answer the embedding
    // probe below (which waits longer) anyway.
    let (models, list_error) = match settings.authorized(client.get(url)).send() {
        Ok(response) => {
            let status = response.status();
            let body = response.text().unwrap_or_default();
            let models = serde_json::from_str::<serde_json::Value>(&body)
                .map(|value| parse_model_list(settings.api, &value))
                .unwrap_or_default();
            let error = (!status.is_success()).then(|| {
                format!("Server responded with {}: {}", status.as_u16(), truncate(body.trim(), 300))
            });
            (models, error)
        }
        Err(error) => (Vec::new(), Some(request_error(settings.api, &error))),
    };
    let listed = models
        .iter()
        .any(|id| id == &settings.model || id.split(':').next() == Some(settings.model.as_str()));

    // Some OpenAI-compatible servers list only chat models, so probe even
    // when the model is not listed; a working probe proves it is served.
    let probe = embed_single_image(&settings, &probe_image());
    let (model_available, images_supported, error) = match probe {
        Ok(_) => (true, Some(true), None),
        Err(error) if listed => (true, Some(false), Some(error)),
        Err(error) => match list_error {
            Some(list_error) => return ServerConnectionStatus::failed(list_error),
            None => (false, None, Some(error)),
        },
    };
    ServerConnectionStatus {
        ok: true,
        model_available,
        models,
        images_supported,
        error,
    }
}

/// Readable message for a request that got no response.
fn request_error(api: ServerApi, error: &reqwest::Error) -> String {
    if error.is_timeout() {
        format!(
            "The {} server did not answer in time; the model may still be loading, try again in a moment ({})",
            api.label(),
            error
        )
    } else if error.is_connect() {
        format!("Cannot reach the {} server; is it running at this URL? ({})", api.label(), error)
    } else {
        format!("{} request failed: {}", api.label(), error)
    }
}

/// Model names from a model-list response: Ollama's `models[].name` or the
/// OpenAI-style `data[].id`.
fn parse_model_list(api: ServerApi, value: &serde_json::Value) -> Vec<String> {
    let (list, field) = match api {
        ServerApi::Ollama => ("models", "name"),
        ServerApi::Omlx | ServerApi::Openai => ("data", "id"),
    };
    value
        .get(list)
        .and_then(|entries| entries.as_array())
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry.get(field).and_then(|id| id.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// Thumbnail-sized JPEG for the image-support probe, so servers see the same
/// kind of input a re-index sends.
fn probe_image() -> Vec<u8> {
    let image = image::RgbImage::from_fn(224, 224, |x, y| image::Rgb([x as u8, y as u8, 128]));
    let mut bytes = std::io::Cursor::new(Vec::new());
    image
        .write_to(&mut bytes, image::ImageFormat::Jpeg)
        .map(|_| bytes.into_inner())
        .unwrap_or_default()
}

/// One input to embed.
enum EmbedInput<'a> {
    Text(&'a str),
    Image(&'a [u8]),
}

/// Request body for `inputs` in the server's wire format.
fn embeddings_body(settings: &EmbeddingServerSettings, inputs: &[EmbedInput]) -> serde_json::Value {
    let data_uri = |bytes: &[u8]| {
        format!("data:{};base64,{}", image_mime(bytes), base64::engine::general_purpose::STANDARD.encode(bytes))
    };
    let mut body = match settings.api {
        ServerApi::Omlx => {
            let items: Vec<serde_json::Value> = inputs
                .iter()
                .map(|input| match input {
                    EmbedInput::Text(text) => json!({ "text": text }),
                    EmbedInput::Image(bytes) => json!({ "image": data_uri(bytes) }),
                })
                .collect();
            json!({ "model": settings.model, "items": items })
        }
        ServerApi::Ollama => {
            let input: Vec<serde_json::Value> = inputs
                .iter()
                .map(|input| match input {
                    EmbedInput::Text(text) => json!(text),
                    EmbedInput::Image(bytes) => {
                        json!({ "image": base64::engine::general_purpose::STANDARD.encode(bytes) })
                    }
                })
                .collect();
            json!({ "model": settings.model, "input": input })
        }
        ServerApi::Openai => {
            let input: Vec<serde_json::Value> = inputs
                .iter()
                .map(|input| match input {
                    EmbedInput::Text(text) => json!(text),
                    EmbedInput::Image(bytes) => json!({ "image": data_uri(bytes) }),
                })
                .collect();
            json!({ "model": settings.model, "input": input, "encoding_format": "float" })
        }
    };
    if let Some(dimensions) = settings.dimensions {
        body["dimensions"] = json!(dimensions);
    }
    body
}

fn parse_vector(value: &serde_json::Value, label: &str) -> Result<Vec<f32>, String> {
    value
        .as_array()
        .ok_or_else(|| format!("{} embeddings entry has no embedding", label))?
        .iter()
        .map(|value| {
            value
                .as_f64()
                .map(|number| number as f32)
                .ok_or_else(|| format!("{} embedding contains a non-numeric value", label))
        })
        .collect()
}

/// Vectors in input order from a response in the server's wire format.
fn parse_embeddings(api: ServerApi, parsed: &serde_json::Value) -> Result<Vec<Vec<f32>>, String> {
    let label = api.label();
    if api == ServerApi::Ollama {
        return parsed
            .get("embeddings")
            .and_then(|embeddings| embeddings.as_array())
            .ok_or_else(|| format!("{} embeddings response has no embeddings array", label))?
            .iter()
            .map(|vector| parse_vector(vector, label))
            .collect();
    }
    let data = parsed
        .get("data")
        .and_then(|data| data.as_array())
        .ok_or_else(|| format!("{} embeddings response has no data array", label))?;
    let mut vectors: Vec<(usize, Vec<f32>)> = Vec::with_capacity(data.len());
    for (position, entry) in data.iter().enumerate() {
        // oMLX always sends the index; other servers may rely on order.
        let index = match entry.get("index").and_then(|index| index.as_u64()) {
            Some(index) => index as usize,
            None if api == ServerApi::Omlx => {
                return Err(format!("{} embeddings entry has no index", label));
            }
            None => position,
        };
        let vector = parse_vector(entry.get("embedding").unwrap_or(&serde_json::Value::Null), label)?;
        vectors.push((index, vector));
    }
    vectors.sort_by_key(|(index, _)| *index);
    Ok(vectors.into_iter().map(|(_, vector)| vector).collect())
}

/// Embed `inputs` in one request and return one vector per input.
fn request_embeddings(
    settings: &EmbeddingServerSettings,
    inputs: &[EmbedInput],
) -> Result<Vec<Vec<f32>>, String> {
    let label = settings.api.label();
    let url = match settings.api {
        ServerApi::Ollama => format!("{}/api/embed", settings.url),
        ServerApi::Omlx | ServerApi::Openai => settings.openai_url("/embeddings"),
    };
    let client = http_client(EMBED_TIMEOUT)?;
    let request = settings.authorized(client.post(url).json(&embeddings_body(settings, inputs)));

    let response = request.send().map_err(|e| request_error(settings.api, &e))?;
    let status = response.status();
    let body = response
        .text()
        .map_err(|e| format!("Failed to read {} response: {}", label, e))?;
    if !status.is_success() {
        return Err(format!(
            "{} embeddings error ({}): {}",
            label,
            status.as_u16(),
            truncate(body.trim(), 300)
        ));
    }

    let parsed = serde_json::from_str::<serde_json::Value>(&body)
        .map_err(|e| format!("Failed to parse {} embeddings response: {}", label, e))?;
    let vectors = parse_embeddings(settings.api, &parsed)?;
    check_vectors(settings, &vectors, inputs.len())?;
    Ok(vectors)
}

fn check_vectors(
    settings: &EmbeddingServerSettings,
    vectors: &[Vec<f32>],
    expected_count: usize,
) -> Result<(), String> {
    let label = settings.api.label();
    if vectors.len() != expected_count {
        return Err(format!(
            "{} returned {} embeddings for {} items",
            label,
            vectors.len(),
            expected_count
        ));
    }
    // A server that ignores `dimensions` would otherwise fill the index with
    // vectors labelled as a size they are not.
    if let Some(expected) = settings.dimensions
        && let Some(vector) = vectors.iter().find(|v| v.len() != expected as usize)
    {
        return Err(format!(
            "{} returned {}-dimensional embeddings, but {} were requested",
            label,
            vector.len(),
            expected
        ));
    }
    // Text-only models on some servers answer an image with an empty vector.
    if vectors.iter().any(|v| v.is_empty()) {
        return Err(format!("{} returned an empty embedding", label));
    }
    Ok(())
}

fn uses_embeddinggemma_prompt(settings: &EmbeddingServerSettings) -> bool {
    settings.model.to_lowercase().contains("embeddinggemma")
}

/// Embed a user query. EmbeddingGemma models expect the retrieval query
/// task prompt; other models receive the raw text.
pub fn embed_query_text(settings: &EmbeddingServerSettings, text: &str) -> Result<Vec<f32>, String> {
    let input = if uses_embeddinggemma_prompt(settings) {
        format!("{}{}", EMBEDDINGGEMMA_QUERY_PROMPT, text)
    } else {
        text.to_string()
    };
    let mut vectors = request_embeddings(settings, &[EmbedInput::Text(&input)])?;
    vectors
        .pop()
        .ok_or_else(|| format!("{} returned no embedding for the query", settings.api.label()))
}

/// Search translation: the configured chat model translates searches to
/// English before they are embedded. EmbeddingGemma scores English queries
/// higher against photos, so "trash" clears the floor where "lixo" does not.
const QUERY_TRANSLATION_TIMEOUT: Duration = Duration::from_secs(30);
const QUERY_TRANSLATION_PROMPT: &str = "Translate the user's photo search into short, natural English. \
Reply with the English search terms only, no quotes or explanation. \
If it is already English, repeat it unchanged.";
const QUERY_TRANSLATION_MAX_CHARS: usize = 200;
const QUERY_TRANSLATION_CACHE_LIMIT: usize = 256;

/// Embed a user search: translate it to English when a translation model is
/// configured, then embed the translation. Falls back to the original text
/// when the translation is unavailable.
pub fn embed_search_text(settings: &EmbeddingServerSettings, text: &str) -> Result<Vec<f32>, String> {
    if settings.translation_model.is_empty() {
        return embed_query_text(settings, text);
    }
    let english = translate_search_text(settings, text);
    embed_query_text(settings, &english)
}

fn translate_search_text(settings: &EmbeddingServerSettings, text: &str) -> String {
    static CACHE: OnceLock<Mutex<HashMap<(String, String), String>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let key = (settings.translation_model.clone(), text.trim().to_lowercase());
    if let Some(hit) = cache.lock().ok().and_then(|c| c.get(&key).cloned()) {
        return hit;
    }
    match request_translation(settings, text.trim()) {
        Some(english) => {
            if let Ok(mut cache) = cache.lock() {
                if cache.len() >= QUERY_TRANSLATION_CACHE_LIMIT {
                    cache.clear();
                }
                cache.insert(key, english.clone());
            }
            english
        }
        None => {
            eprintln!("Search translation unavailable, embedding the original text: {}", text);
            text.to_string()
        }
    }
}

/// OpenAI-style chat completion; oMLX, Ollama and the OpenAI-compatible
/// servers all serve it under /v1.
fn request_translation(settings: &EmbeddingServerSettings, text: &str) -> Option<String> {
    if text.is_empty() || text.chars().count() > QUERY_TRANSLATION_MAX_CHARS {
        return None;
    }
    let client = http_client(QUERY_TRANSLATION_TIMEOUT).ok()?;
    let body = json!({
        "model": settings.translation_model,
        "temperature": 0,
        "max_tokens": 24,
        "messages": [
            { "role": "system", "content": QUERY_TRANSLATION_PROMPT },
            { "role": "user", "content": text },
        ],
    });
    let request = settings.authorized(client.post(settings.openai_url("/chat/completions")).json(&body));
    let response: serde_json::Value = request
        .send()
        .ok()?
        .error_for_status()
        .ok()?
        .json()
        .ok()?;
    let english = response["choices"][0]["message"]["content"]
        .as_str()?
        .trim()
        .trim_matches(|c| c == '"' || c == '\'')
        .trim();
    (!english.is_empty() && english.chars().count() <= QUERY_TRANSLATION_MAX_CHARS)
        .then(|| english.to_string())
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

/// Embed a single image (thumbnail bytes or raw file contents).
pub fn embed_single_image(settings: &EmbeddingServerSettings, bytes: &[u8]) -> Result<Vec<f32>, String> {
    let mut vectors = request_embeddings(settings, &[EmbedInput::Image(bytes)])?;
    vectors
        .pop()
        .ok_or_else(|| format!("{} returned no embedding for the image", settings.api.label()))
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
    // One snapshot for the whole run: switching providers mid-run must not
    // change which model the remaining vectors are produced or labelled by.
    let provider = active_settings();
    if let Ok(mut status) = state.status.lock() {
        *status = EmbeddingReindexStatus {
            state: "running".into(),
            model: embed_model_id(provider.as_ref()),
            ..Default::default()
        };
    }
    let running = state.running.clone();
    let cancel = state.cancel.clone();
    let status_shared = state.status.clone();

    std::thread::spawn(move || {
        let result = run_reindex(&app, &cancel, &library_id, &status_shared, provider);
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
    provider: Option<EmbeddingServerSettings>,
) -> Result<(), String> {
    let model_id = embed_model_id(provider.as_ref());
    // Vectors of another size under this label (written by an older build)
    // are re-embedded too.
    let expected_bytes = provider
        .as_ref()
        .map(|settings| expected_dimensions(settings).map(|d| d * 4))
        .transpose()?;

    // Collect the full work list up front so parallel workers can share it.
    let work = crate::t_cmds::with_current_library(library_id, || {
        let mut work: Vec<(i64, String)> = Vec::new();
        let mut last_id: i64 = 0;
        loop {
            let page = crate::t_sqlite::AFile::get_embedding_reindex_batch(
                last_id,
                REINDEX_PAGE_SIZE,
                &model_id,
                expected_bytes,
            )?;
            let Some((id, _)) = page.last() else { break };
            last_id = *id;
            work.extend(page);
        }
        Ok(work)
    })?;
    if let Ok(mut status) = status.lock() {
        status.total = work.len() as i64;
    }

    match provider {
        Some(settings) => run_reindex_server(app, cancel, library_id, status, settings, work),
        None => run_reindex_builtin(app, cancel, library_id, status, work),
    }
}

/// Side effects of one re-index batch. Production goes to the library
/// database and the embedding server; tests substitute library switches and
/// settings changes that land while the embedding request is in flight.
trait BatchIo {
    /// Run `op` only while `library_id` is the active library, holding off
    /// library switches until it returns.
    fn in_library<T>(
        &self,
        library_id: &str,
        op: impl FnOnce() -> Result<T, String>,
    ) -> Result<T, String>;
    fn read_input(&self, file_id: i64, file_path: &str, library_id: &str) -> Result<Vec<u8>, String>;
    fn embed(&self, inputs: &[Vec<u8>]) -> Result<Vec<Vec<f32>>, String>;
    fn store(&self, results: &[(i64, Vec<f32>)], model_id: &str) -> Result<usize, String>;
}

struct ServerBatchIo<'a> {
    settings: &'a EmbeddingServerSettings,
}

impl BatchIo for ServerBatchIo<'_> {
    fn in_library<T>(
        &self,
        library_id: &str,
        op: impl FnOnce() -> Result<T, String>,
    ) -> Result<T, String> {
        crate::t_cmds::with_current_library(library_id, op)
    }

    fn read_input(&self, file_id: i64, file_path: &str, library_id: &str) -> Result<Vec<u8>, String> {
        image_input_bytes(file_id, file_path, library_id)
    }

    fn embed(&self, inputs: &[Vec<u8>]) -> Result<Vec<Vec<f32>>, String> {
        let items: Vec<EmbedInput> = inputs.iter().map(|bytes| EmbedInput::Image(bytes)).collect();
        request_embeddings(self.settings, &items)
    }

    fn store(&self, results: &[(i64, Vec<f32>)], model_id: &str) -> Result<usize, String> {
        crate::t_sqlite::AFile::store_embeddings(results, model_id)
    }
}

#[derive(Debug, PartialEq)]
struct BatchOutcome {
    /// Files whose vectors were written.
    stored: Vec<i64>,
    failed: i64,
}

#[derive(Debug, PartialEq)]
enum BatchError {
    /// The server rejected the batch; counts toward the consecutive-error abort.
    Embed { error: String, failed: i64 },
    /// The library changed or the database refused the write; stop the run.
    Abort(String),
}

/// Embed one batch: read inputs from `library_id`, embed them without
/// holding the library guard (requests can take seconds), then store them
/// back under the guard in one transaction. A library switch while the
/// request is in flight fails the second guard, so the vectors are dropped
/// instead of landing in whichever library is active by then.
fn embed_batch(
    io: &impl BatchIo,
    chunk: &[(i64, String)],
    library_id: &str,
    model_id: &str,
) -> Result<BatchOutcome, BatchError> {
    let mut failed = 0i64;
    let inputs = io
        .in_library(library_id, || {
            let mut inputs = Vec::with_capacity(chunk.len());
            for (file_id, file_path) in chunk {
                match io.read_input(*file_id, file_path, library_id) {
                    Ok(bytes) => inputs.push((*file_id, bytes)),
                    Err(error) => {
                        failed += 1;
                        eprintln!("Embedding re-index skipped file {}: {}", file_id, error);
                    }
                }
            }
            Ok(inputs)
        })
        .map_err(BatchError::Abort)?;
    if inputs.is_empty() {
        return Ok(BatchOutcome { stored: Vec::new(), failed });
    }

    let (ids, bytes): (Vec<i64>, Vec<Vec<u8>>) = inputs.into_iter().unzip();
    let vectors = io.embed(&bytes).map_err(|error| BatchError::Embed {
        error,
        failed: failed + ids.len() as i64,
    })?;
    let results: Vec<(i64, Vec<f32>)> = ids.into_iter().zip(vectors).collect();
    io.in_library(library_id, || io.store(&results, model_id))
        .map_err(BatchError::Abort)?;
    Ok(BatchOutcome {
        stored: results.into_iter().map(|(file_id, _)| file_id).collect(),
        failed,
    })
}

// ============================================================================
// Embeddings for newly indexed photos
// ============================================================================

/// One indexed file waiting for its embedding.
struct IndexJob {
    settings: EmbeddingServerSettings,
    library_id: String,
    file_id: i64,
    file_path: String,
    reply: mpsc::Sender<Result<(), String>>,
}

struct IndexQueue {
    jobs: Mutex<VecDeque<IndexJob>>,
    ready: Condvar,
}

fn index_queue() -> &'static IndexQueue {
    static QUEUE: OnceLock<IndexQueue> = OnceLock::new();
    QUEUE.get_or_init(|| {
        for _ in 0..REINDEX_CONCURRENCY {
            std::thread::spawn(index_worker);
        }
        IndexQueue {
            jobs: Mutex::new(VecDeque::new()),
            ready: Condvar::new(),
        }
    })
}

/// Embed a newly indexed file with the embedding server and wait for the
/// result. Files the indexer processes concurrently share requests, the
/// same way a re-index does, instead of sending one image per request.
pub fn embed_indexed_file(
    settings: &EmbeddingServerSettings,
    library_id: &str,
    file_id: i64,
    file_path: &str,
) -> Result<(), String> {
    let (reply, result) = mpsc::channel();
    let queue = index_queue();
    queue
        .jobs
        .lock()
        .map_err(|e| e.to_string())?
        .push_back(IndexJob {
            settings: settings.clone(),
            library_id: library_id.to_string(),
            file_id,
            file_path: file_path.to_string(),
            reply,
        });
    queue.ready.notify_one();
    result
        .recv()
        .map_err(|_| "Embedding worker stopped".to_string())?
}

/// Next batch: the oldest job plus up to REINDEX_BATCH_SIZE - 1 more for the
/// same library and settings; other jobs keep their place in the queue.
fn take_index_batch(jobs: &mut VecDeque<IndexJob>) -> Vec<IndexJob> {
    let Some(first) = jobs.pop_front() else { return Vec::new() };
    let mut batch = vec![first];
    let mut index = 0;
    while batch.len() < REINDEX_BATCH_SIZE && index < jobs.len() {
        let same = jobs[index].library_id == batch[0].library_id
            && jobs[index].settings == batch[0].settings;
        if same {
            batch.extend(jobs.remove(index));
        } else {
            index += 1;
        }
    }
    batch
}

fn index_worker() {
    let queue = index_queue();
    loop {
        let batch = {
            let Ok(mut jobs) = queue.jobs.lock() else { return };
            while jobs.is_empty() {
                jobs = match queue.ready.wait(jobs) {
                    Ok(jobs) => jobs,
                    Err(_) => return,
                };
            }
            take_index_batch(&mut jobs)
        };

        let first = &batch[0];
        let chunk: Vec<(i64, String)> = batch
            .iter()
            .map(|job| (job.file_id, job.file_path.clone()))
            .collect();
        let io = ServerBatchIo { settings: &first.settings };
        let outcome = embed_batch(&io, &chunk, &first.library_id, &first.settings.model_id());
        for job in &batch {
            let result = match &outcome {
                Ok(outcome) if outcome.stored.contains(&job.file_id) => Ok(()),
                Ok(_) => Err("no thumbnail or decodable image to embed".to_string()),
                Err(BatchError::Embed { error, .. }) | Err(BatchError::Abort(error)) => {
                    Err(error.clone())
                }
            };
            let _ = job.reply.send(result);
        }
    }
}

/// Server re-index: several workers pull batches from a shared queue and embed
/// them concurrently; the server batches each request internally.
fn run_reindex_server(
    app: &tauri::AppHandle,
    cancel: &Arc<AtomicBool>,
    library_id: &str,
    status: &Arc<Mutex<EmbeddingReindexStatus>>,
    settings: EmbeddingServerSettings,
    work: Vec<(i64, String)>,
) -> Result<(), String> {
    let model_id = settings.model_id();
    let io = ServerBatchIo { settings: &settings };
    let queue: Mutex<VecDeque<(i64, String)>> = Mutex::new(VecDeque::from(work));
    let done = AtomicI64::new(0);
    let failed = AtomicI64::new(0);
    let consecutive_errors = AtomicUsize::new(0);
    let abort_reason: Mutex<Option<String>> = Mutex::new(None);
    let set_abort = |reason: String| {
        if let Ok(mut current) = abort_reason.lock() {
            current.get_or_insert(reason);
        }
    };

    std::thread::scope(|scope| {
        for _ in 0..REINDEX_CONCURRENCY {
            scope.spawn(|| loop {
                if cancel.load(Ordering::SeqCst)
                    || abort_reason.lock().map_or(true, |reason| reason.is_some())
                {
                    break;
                }

                let chunk: Vec<(i64, String)> = match queue.lock() {
                    Ok(mut queue) => {
                        let take = queue.len().min(REINDEX_BATCH_SIZE);
                        queue.drain(..take).collect()
                    }
                    Err(_) => break,
                };
                if chunk.is_empty() {
                    break;
                }

                match embed_batch(&io, &chunk, library_id, &model_id) {
                    Ok(outcome) => {
                        consecutive_errors.store(0, Ordering::SeqCst);
                        done.fetch_add(outcome.stored.len() as i64, Ordering::SeqCst);
                        failed.fetch_add(outcome.failed, Ordering::SeqCst);
                    }
                    Err(BatchError::Embed { error, failed: count }) => {
                        failed.fetch_add(count, Ordering::SeqCst);
                        eprintln!("Embedding re-index batch failed: {}", error);
                        if consecutive_errors.fetch_add(1, Ordering::SeqCst) + 1
                            >= MAX_CONSECUTIVE_BATCH_ERRORS
                        {
                            set_abort(error);
                        }
                    }
                    Err(BatchError::Abort(error)) => set_abort(error),
                }

                if let Ok(mut current) = status.lock() {
                    current.done = done.load(Ordering::SeqCst);
                    current.failed = failed.load(Ordering::SeqCst);
                    let _ = app.emit("embedding_reindex_progress", current.clone());
                }
            });
        }
    });

    if let Some(error) = abort_reason.into_inner().ok().flatten() {
        return Err(error);
    }
    // The stored band predates these vectors; measure again.
    request_calibration(settings, library_id, true);
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
        let result = crate::t_cmds::with_current_library(library_id, || {
            crate::t_sqlite::AFile::generate_embedding(&ai_state, file_id)
        });
        match result {
            Ok(_) => done += 1,
            Err(error) if error == "Library changed" => return Err(error),
            Err(error) => {
                failed += 1;
                eprintln!("Embedding re-index failed for {}: {}", file_id, error);
            }
        }
        if (done + failed) % 16 == 0
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

/// Thumbnail bytes when available (much smaller than the original, and in a
/// format the server can always decode). Regenerates the cached thumbnail
/// when the cache file is missing — common after cache cleanup or for HEIC /
/// RAW libraries — and falls back to the original only for formats the
/// embedding server can decode directly.
pub fn image_input_bytes(file_id: i64, file_path: &str, library_id: &str) -> Result<Vec<u8>, String> {
    if let Ok(Some(bytes)) =
        crate::t_sqlite::AThumb::fetch_raw_for_library(file_id, library_id)
    {
        if !bytes.is_empty() {
            return Ok(bytes);
        }
    }
    // Reading an online-only original would download it.
    if !crate::t_utils::content_readable(file_path) {
        return Err("Skipped: the original is not local".to_string());
    }
    let bytes =
        std::fs::read(file_path).map_err(|e| format!("Failed to read image file: {}", e))?;
    if !is_server_decodable_image(&bytes) {
        return Err(format!(
            "no thumbnail available and original format is not decodable by the server: {}",
            file_path
        ));
    }
    Ok(bytes)
}

/// Formats embedding servers can decode from inline data (JPEG, PNG,
/// WebP, GIF). HEIC, TIFF, PSD, and camera RAW must go through thumbnails.
pub fn is_server_decodable_image(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0xFF, 0xD8, 0xFF]) // JPEG
        || bytes.starts_with(&[0x89, b'P', b'N', b'G']) // PNG
        || (bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP")
        || bytes.starts_with(b"GIF87a")
        || bytes.starts_with(b"GIF89a")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn settings(model: &str, dimensions: Option<u32>) -> EmbeddingServerSettings {
        EmbeddingServerSettings {
            api: ServerApi::Omlx,
            url: DEFAULT_URL.into(),
            api_key: String::new(),
            model: model.into(),
            dimensions,
            translation_model: String::new(),
        }
    }

    fn server(api: ServerApi, url: &str) -> EmbeddingServerSettings {
        EmbeddingServerSettings { api, url: url.into(), ..settings("eg2", None) }
    }

    fn band(floor: f32) -> Option<ScoreCalibration> {
        Some(ScoreCalibration { floor, margin: 0.04, baseline: 0.55 })
    }

    #[test]
    fn calibration_is_scoped_to_library_model_and_dimensions() {
        let mut store = CalibrationStore::default();
        let full = calibration_key(&settings("eg2", None), "lib-a");
        let dims = calibration_key(&settings("eg2", Some(256)), "lib-a");
        let other_library = calibration_key(&settings("eg2", None), "lib-b");

        let generation = store.begin(&full, false).unwrap();
        assert!(store.finish(&full, generation, band(0.60)));

        assert_eq!(store.get(&full), Some(band(0.60)));
        assert_eq!(store.get(&dims), None, "dimensions change the score band");
        assert_eq!(store.get(&other_library), None, "libraries are measured separately");
    }

    #[test]
    fn calibration_runs_once_unless_forced() {
        let mut store = CalibrationStore::default();
        let key = calibration_key(&settings("eg2", None), "lib-a");

        let first = store.begin(&key, false).unwrap();
        assert_eq!(store.begin(&key, false), None, "repeat searches must not restart a running measurement");
        assert!(store.finish(&key, first, band(0.60)));
        assert_eq!(store.begin(&key, false), None, "a stored result is reused");
        assert!(store.begin(&key, true).is_some(), "a re-index forces a fresh measurement");
    }

    #[test]
    fn stale_calibration_results_are_discarded() {
        let mut store = CalibrationStore::default();
        let key = calibration_key(&settings("eg2", None), "lib-a");

        // Measurement over a half-indexed library, superseded by the one
        // forced when the re-index finished.
        let stale = store.begin(&key, false).unwrap();
        let fresh = store.begin(&key, true).unwrap();

        // Slow stale measurement lands first: dropped while the fresh one runs.
        assert!(!store.finish(&key, stale, band(0.55)));
        assert!(store.finish(&key, fresh, band(0.62)));
        assert_eq!(store.get(&key), Some(band(0.62)));

        // And the other order: the stale result must not overwrite.
        let stale = store.begin(&key, true).unwrap();
        let fresh = store.begin(&key, true).unwrap();
        assert!(store.finish(&key, fresh, band(0.64)));
        assert!(!store.finish(&key, stale, band(0.50)));
        assert_eq!(store.get(&key), Some(band(0.64)));
    }

    #[test]
    fn failed_calibration_can_be_retried() {
        let mut store = CalibrationStore::default();
        let key = calibration_key(&settings("eg2", None), "lib-a");
        let generation = store.begin(&key, false).unwrap();
        store.abandon(&key, generation);
        assert!(store.begin(&key, false).is_some());
    }

    fn job(library_id: &str, model: &str, file_id: i64) -> IndexJob {
        IndexJob {
            settings: settings(model, Some(512)),
            library_id: library_id.into(),
            file_id,
            file_path: String::new(),
            reply: mpsc::channel().0,
        }
    }

    #[test]
    fn index_batches_group_same_library_and_model_in_order() {
        let size = REINDEX_BATCH_SIZE as i64;
        let mut jobs: VecDeque<IndexJob> = VecDeque::new();
        jobs.push_back(job("lib-a", "eg2", 1));
        jobs.push_back(job("lib-b", "eg2", 2));
        jobs.push_back(job("lib-a", "other", 3));
        // One full batch after job 1, plus a remainder of one.
        for id in 4..4 + size {
            jobs.push_back(job("lib-a", "eg2", id));
        }

        let ids = |batch: Vec<IndexJob>| batch.iter().map(|j| j.file_id).collect::<Vec<_>>();
        let first: Vec<i64> = std::iter::once(1).chain(4..4 + size - 1).collect();
        assert_eq!(ids(take_index_batch(&mut jobs)), first);
        assert_eq!(ids(take_index_batch(&mut jobs)), vec![2]);
        assert_eq!(ids(take_index_batch(&mut jobs)), vec![3]);
        assert_eq!(ids(take_index_batch(&mut jobs)), vec![3 + size]);
        assert!(take_index_batch(&mut jobs).is_empty());
    }

    #[test]
    fn requested_dimensions_need_no_probe() {
        // An unreachable URL proves no request is made.
        let mut fixed = settings("eg2", Some(256));
        fixed.url = "http://127.0.0.1:1".into();
        assert_eq!(expected_dimensions(&fixed), Ok(256));
    }

    #[test]
    fn model_id_changes_with_model_and_dimensions() {
        assert_eq!(embed_model_id(None), BUILTIN_EMBED_MODEL);
        assert_eq!(embed_model_id(Some(&settings("eg2", None))), "omlx:eg2");
        assert_eq!(embed_model_id(Some(&settings("eg2", Some(512)))), "omlx:eg2:512");
    }

    #[test]
    fn model_id_is_scoped_to_the_server_api() {
        let ollama = EmbeddingServerSettings { dimensions: Some(256), ..server(ServerApi::Ollama, "http://h") };
        assert_eq!(ollama.model_id(), "ollama:eg2:256");
        assert_eq!(server(ServerApi::Openai, "http://h").model_id(), "openai:eg2");
    }

    #[test]
    fn stored_settings_without_api_are_omlx() {
        let stored: EmbeddingServerSettings =
            serde_json::from_value(json!({ "url": "http://h", "model": "eg2" })).unwrap();
        assert_eq!(stored.api, ServerApi::Omlx);
        assert_eq!(stored.translation_model, "");
    }

    #[test]
    fn urls_accept_base_with_or_without_v1() {
        let plain = server(ServerApi::Openai, "http://h:1234/").normalized().unwrap();
        let v1 = server(ServerApi::Openai, "http://h:1234/v1").normalized().unwrap();
        assert_eq!(plain.openai_url("/embeddings"), "http://h:1234/v1/embeddings");
        assert_eq!(v1.openai_url("/embeddings"), "http://h:1234/v1/embeddings");
        let ollama = server(ServerApi::Ollama, "http://h:11434/v1/").normalized().unwrap();
        assert_eq!(ollama.url, "http://h:11434", "Ollama's native API is outside /v1");
    }

    #[test]
    fn request_bodies_follow_each_api() {
        let png = [0x89, b'P', b'N', b'G', 1];
        let inputs = [EmbedInput::Text("cat"), EmbedInput::Image(&png)];
        let encoded = base64::engine::general_purpose::STANDARD.encode(png);

        let omlx = embeddings_body(&settings("eg2", Some(256)), &inputs);
        assert_eq!(omlx["items"][0], json!({ "text": "cat" }));
        assert_eq!(omlx["items"][1], json!({ "image": format!("data:image/png;base64,{}", encoded) }));
        assert_eq!(omlx["dimensions"], json!(256));

        let ollama = embeddings_body(&server(ServerApi::Ollama, "http://h"), &inputs);
        assert_eq!(ollama["input"], json!(["cat", { "image": encoded }]), "Ollama takes raw base64");
        assert!(ollama.get("dimensions").is_none());

        let openai = embeddings_body(&server(ServerApi::Openai, "http://h"), &inputs);
        assert_eq!(
            openai["input"],
            json!(["cat", { "image": format!("data:image/png;base64,{}", encoded) }])
        );
        assert_eq!(openai["encoding_format"], json!("float"));
    }

    #[test]
    fn responses_parse_in_input_order() {
        let ollama = json!({ "embeddings": [[1.0, 0.0], [0.0, 1.0]] });
        assert_eq!(
            parse_embeddings(ServerApi::Ollama, &ollama).unwrap(),
            vec![vec![1.0, 0.0], vec![0.0, 1.0]]
        );

        let shuffled = json!({ "data": [
            { "index": 1, "embedding": [0.0, 1.0] },
            { "index": 0, "embedding": [1.0, 0.0] },
        ] });
        assert_eq!(
            parse_embeddings(ServerApi::Omlx, &shuffled).unwrap(),
            vec![vec![1.0, 0.0], vec![0.0, 1.0]]
        );

        let unindexed = json!({ "data": [{ "embedding": [1.0] }, { "embedding": [2.0] }] });
        assert_eq!(parse_embeddings(ServerApi::Openai, &unindexed).unwrap(), vec![vec![1.0], vec![2.0]]);
        assert!(parse_embeddings(ServerApi::Omlx, &unindexed).is_err(), "oMLX always sends the index");
    }

    #[test]
    fn vector_checks_reject_wrong_count_size_and_empty() {
        let fixed = settings("eg2", Some(2));
        assert!(check_vectors(&fixed, &[vec![1.0, 0.0]], 1).is_ok());
        assert!(check_vectors(&fixed, &[vec![1.0, 0.0]], 2).is_err());
        assert!(check_vectors(&fixed, &[vec![1.0, 0.0, 0.0]], 1).is_err());
        assert!(check_vectors(&settings("eg2", None), &[vec![]], 1).is_err());
    }

    #[test]
    fn model_lists_parse_for_each_api() {
        let ollama = json!({ "models": [{ "name": "embeddinggemma-2:latest" }] });
        assert_eq!(parse_model_list(ServerApi::Ollama, &ollama), vec!["embeddinggemma-2:latest"]);
        let openai = json!({ "data": [{ "id": "text-embedding-nomic" }] });
        assert_eq!(parse_model_list(ServerApi::Openai, &openai), vec!["text-embedding-nomic"]);
    }

    #[test]
    fn probe_image_is_a_jpeg() {
        assert_eq!(image_mime(&probe_image()), "image/jpeg");
    }

    /// Re-index side effects against an in-memory "current library"; the
    /// hook runs while the embedding request is in flight.
    struct FakeIo {
        lock: RwLock<()>,
        current_library: RefCell<String>,
        unreadable: Vec<i64>,
        during_request: Box<dyn Fn(&FakeIo)>,
        stored: RefCell<Vec<(String, i64, String)>>,
    }

    impl FakeIo {
        fn new(during_request: impl Fn(&FakeIo) + 'static) -> Self {
            Self {
                lock: RwLock::new(()),
                current_library: RefCell::new("lib-a".into()),
                unreadable: Vec::new(),
                during_request: Box::new(during_request),
                stored: RefCell::new(Vec::new()),
            }
        }
    }

    impl BatchIo for FakeIo {
        fn in_library<T>(
            &self,
            library_id: &str,
            op: impl FnOnce() -> Result<T, String>,
        ) -> Result<T, String> {
            let current = self.current_library.borrow().clone();
            crate::t_cmds::with_library_context(&self.lock, library_id, || Ok(current), op)
        }

        fn read_input(&self, file_id: i64, _: &str, _: &str) -> Result<Vec<u8>, String> {
            if self.unreadable.contains(&file_id) {
                return Err("unreadable".into());
            }
            Ok(vec![file_id as u8])
        }

        fn embed(&self, inputs: &[Vec<u8>]) -> Result<Vec<Vec<f32>>, String> {
            (self.during_request)(self);
            Ok(inputs.iter().map(|bytes| vec![bytes[0] as f32, 1.0]).collect())
        }

        fn store(&self, results: &[(i64, Vec<f32>)], model_id: &str) -> Result<usize, String> {
            let library = self.current_library.borrow().clone();
            let mut stored = self.stored.borrow_mut();
            for (file_id, _) in results {
                stored.push((library.clone(), *file_id, model_id.to_string()));
            }
            Ok(results.len())
        }
    }

    fn chunk(ids: &[i64]) -> Vec<(i64, String)> {
        ids.iter().map(|id| (*id, format!("/photos/{}.jpg", id))).collect()
    }

    #[test]
    fn library_switch_during_request_drops_the_batch() {
        let io = FakeIo::new(|io| *io.current_library.borrow_mut() = "lib-b".into());
        let result = embed_batch(&io, &chunk(&[1, 2, 3]), "lib-a", "omlx:eg2");
        assert_eq!(result, Err(BatchError::Abort("Library changed".into())));
        assert!(
            io.stored.borrow().is_empty(),
            "vectors computed for lib-a must not land in lib-b"
        );
    }

    #[test]
    fn library_switch_before_reading_stores_nothing() {
        let io = FakeIo::new(|_| panic!("no request once the library changed"));
        *io.current_library.borrow_mut() = "lib-b".into();
        let result = embed_batch(&io, &chunk(&[1]), "lib-a", "omlx:eg2");
        assert_eq!(result, Err(BatchError::Abort("Library changed".into())));
        assert!(io.stored.borrow().is_empty());
    }

    #[test]
    fn model_change_during_request_keeps_the_generating_model_label() {
        let snapshot = settings("eg2", Some(512));
        let io = FakeIo::new(|_| {
            *active_settings_cell().write().unwrap() = Some(settings("other-model", None));
        });
        let result = embed_batch(&io, &chunk(&[1, 2]), "lib-a", &snapshot.model_id());
        let active_after = active_model_id();
        *active_settings_cell().write().unwrap() = None;

        assert_eq!(result, Ok(BatchOutcome { stored: vec![1, 2], failed: 0 }));
        assert_eq!(active_after, "omlx:other-model");
        assert_eq!(
            *io.stored.borrow(),
            vec![
                ("lib-a".to_string(), 1, "omlx:eg2:512".to_string()),
                ("lib-a".to_string(), 2, "omlx:eg2:512".to_string()),
            ]
        );
    }

    #[test]
    fn unreadable_files_are_counted_and_the_rest_stored() {
        let mut io = FakeIo::new(|_| {});
        io.unreadable = vec![2];
        let result = embed_batch(&io, &chunk(&[1, 2, 3]), "lib-a", "omlx:eg2");
        assert_eq!(result, Ok(BatchOutcome { stored: vec![1, 3], failed: 1 }));
        let ids: Vec<i64> = io.stored.borrow().iter().map(|(_, id, _)| *id).collect();
        assert_eq!(ids, vec![1, 3]);
    }
}
