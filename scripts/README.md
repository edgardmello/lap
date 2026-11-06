# Scripts

Utility scripts for the Lap project.

## 1. download_models

Downloads AI model files (CLIP + InsightFace) required for the app to `src-tauri/resources/models/`. Already downloaded files are skipped automatically.

### Usage

**macOS / Linux:**
```bash
./scripts/download_models.sh
```

**Windows (PowerShell):**
```powershell
.\scripts\download_models.ps1
```

### Models Downloaded

| Model | Source | Purpose |
|-------|--------|---------|
| `tokenizer.json` | OpenAI CLIP ViT-B/32 | Text tokenization |
| `text_model.onnx` | CLIP ViT-B/32 (quantized) | Text embedding |
| `vision_model.onnx` | CLIP ViT-B/32 (quantized) | Image embedding |
| `det_500m.onnx` | InsightFace Buffalo-S | Face detection |
| `w600k_mbf.onnx` | InsightFace Buffalo-S | Face recognition |

## 2. download_ffmpeg_sidecar

Downloads FFmpeg and FFprobe sidecar binaries for the current platform into `src-tauri/resources/ffmpeg/`. This ensures the packaged app can include the sidecar binaries with releases built from source.

### Usage

**macOS / Linux:**
```bash
./scripts/download_ffmpeg_sidecar.sh
```

**Windows (PowerShell):**
```powershell
.\scripts\download_ffmpeg_sidecar.ps1
```

### Sidecar Files Downloaded

| Platform | Files |
|----------|-------|
| macOS Intel | `ffmpeg-x86_64-apple-darwin`, `ffprobe-x86_64-apple-darwin` |
| macOS Apple Silicon | `ffmpeg-aarch64-apple-darwin`, `ffprobe-aarch64-apple-darwin` |
| Linux x86_64 | `ffmpeg-x86_64-unknown-linux-gnu`, `ffprobe-x86_64-unknown-linux-gnu` |
| Linux aarch64 | `ffmpeg-aarch64-unknown-linux-gnu`, `ffprobe-aarch64-unknown-linux-gnu` |
| Windows x86_64 | `ffmpeg-x86_64-pc-windows-msvc.exe`, `ffprobe-x86_64-pc-windows-msvc.exe` |

## 3. pplx_embed_server.py

Local embedding server for Perplexity's
[`pplx-embed-v2-context-0.6b`](https://huggingface.co/perplexity-ai/pplx-embed-v2-context-0.6b),
a 0.6B vision-language embedding model, for image search. The model ships only
as Transformers custom code (no ONNX, MLX or GGUF build), so oMLX, Ollama and
LM Studio cannot serve it; this script serves it through the small part of the
OpenAI API that Lap's **OpenAI-compatible server** provider uses.

Searches are embedded as queries (the model adds its own `[Q]` marker) and each
photo as one whole-image vector. Vectors are 1024-dimensional and normalized;
the model has no documented Matryoshka truncation, so keep **Dimensions** at
**Full**.

### Usage

```bash
python3 -m venv ~/.venvs/pplx-embed
source ~/.venvs/pplx-embed/bin/activate
pip install -r scripts/pplx_embed_requirements.txt
python scripts/pplx_embed_server.py        # serves http://127.0.0.1:8090/v1
```

The first run downloads the weights (~2.4 GB) into the Hugging Face cache. It
uses Apple Silicon (MPS) or CUDA when available and the CPU otherwise; see
`--help` for `--port`, `--device`, `--dtype` and `--max-pixels`.

Then in Lap, open **Settings > Image search**, choose **OpenAI-compatible
server**, set the URL to `http://127.0.0.1:8090/v1` and the model to
`perplexity-ai/pplx-embed-v2-context-0.6b`, test the connection, and re-index.

### Portuguese and other non-English searches

pplx matches English text to photos better than other languages, so
"cachorro" and "dog" can return different photos. Lap's **Search translation
model** setting translates each search into English before embedding it, by
calling `/v1/chat/completions` on the same server. The pplx server has no chat
model, so give it a local chat server to forward those requests to:

```bash
# Ollama (pull a small instruct model first, e.g. `ollama pull qwen2.5:3b`)
python scripts/pplx_embed_server.py --chat-upstream http://127.0.0.1:11434/v1

# oMLX, which needs its own API key
PPLX_CHAT_API_KEY=<oMLX key> python scripts/pplx_embed_server.py \
    --chat-upstream http://127.0.0.1:8000/v1
```

Then set **Search translation model** in Lap to a model that chat server
serves (for example `qwen2.5:3b` in Ollama or `Qwen2.5-VL-3B-Instruct-4bit` in
oMLX). The upstream's models are also listed at `/v1/models`. No re-index is
needed: only searches change.
