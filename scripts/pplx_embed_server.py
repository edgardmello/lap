#!/usr/bin/env python3
"""
Local OpenAI-compatible embedding server for Perplexity's
pplx-embed-v2-context-0.6b, for Lap's image search.

The model is a 0.6B vision-language embedding model that ships only as
Transformers custom code (no ONNX, MLX or GGUF build), so oMLX, Ollama and
LM Studio cannot serve it. This script loads it with Transformers and serves
the subset of the OpenAI API that Lap's "OpenAI-compatible server" provider
uses:

    GET  /v1/models       -> {"data": [{"id": "<model>"}]}
    POST /v1/embeddings   -> {"input": ["text", {"image": "data:image/jpeg;base64,..."}]}

Lap only sends two kinds of inputs: search queries (text) and photos
(images). Text is therefore embedded with `encode_queries` (the model adds its
"[Q]" query marker itself) and images with `encode_images(chunks=1)`, which
gives one 1024-dimensional vector for the whole photo. Vectors are
L2-normalized. The model has no documented Matryoshka truncation, so a
`dimensions` request is refused; keep Lap's Dimensions setting at "Full".

Setup (macOS, Apple Silicon):

    python3 -m venv ~/.venvs/pplx-embed
    source ~/.venvs/pplx-embed/bin/activate
    pip install -r scripts/pplx_embed_requirements.txt
    python scripts/pplx_embed_server.py            # http://127.0.0.1:8090

Then in Lap: Settings > Image search > Provider "OpenAI-compatible server",
URL http://127.0.0.1:8090/v1, model perplexity-ai/pplx-embed-v2-context-0.6b,
Dimensions "Full", and re-index.

Everything runs locally; the model weights (~2.4 GB) are downloaded once
from Hugging Face into the usual Hugging Face cache.
"""

from __future__ import annotations

import argparse
import base64
import binascii
import io
import json
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

DEFAULT_MODEL = "perplexity-ai/pplx-embed-v2-context-0.6b"


def pick_device(requested: str) -> str:
    import torch

    if requested != "auto":
        return requested
    if torch.cuda.is_available():
        return "cuda"
    if getattr(torch.backends, "mps", None) and torch.backends.mps.is_available():
        return "mps"
    return "cpu"


def pick_dtype(requested: str, device: str):
    import torch

    if requested == "auto":
        requested = "float32" if device == "cpu" else "bfloat16"
    return {
        "float32": torch.float32,
        "float16": torch.float16,
        "bfloat16": torch.bfloat16,
    }[requested]


class Embedder:
    """Loaded model plus a lock: one forward pass at a time."""

    def __init__(self, args: argparse.Namespace):
        import torch
        from transformers import AutoModel

        self.device = pick_device(args.device)
        dtype = pick_dtype(args.dtype, self.device)
        self.model_name = args.model
        self.min_pixels = args.min_pixels
        self.max_pixels = args.max_pixels
        self.lock = threading.Lock()

        print(f"Loading {args.model} on {self.device} ({str(dtype).replace('torch.', '')})...", flush=True)
        started = time.time()
        kwargs = {"trust_remote_code": True, "dtype": dtype}
        if args.revision:
            kwargs["revision"] = args.revision
        if self.device != "cpu":
            kwargs["attn_implementation"] = "sdpa"
        self.model = AutoModel.from_pretrained(args.model, **kwargs).to(self.device).eval()
        self.torch = torch
        print(f"Model ready in {time.time() - started:.1f} s", flush=True)

    def served_names(self) -> set[str]:
        short = self.model_name.rstrip("/").split("/")[-1]
        return {self.model_name, short}

    def embed_texts(self, texts: list[str]) -> list[list[float]]:
        if not texts:
            return []
        with self.lock, self.torch.inference_mode():
            rows = self.model.encode_queries(
                [[text] for text in texts],
                batch_size=len(texts),
                normalize_embeddings=True,
            )
        return [row[0].tolist() for row in rows]

    def embed_images(self, images) -> list[list[float]]:
        if not images:
            return []
        with self.lock, self.torch.inference_mode():
            rows = self.model.encode_images(
                images,
                batch_size=len(images),
                normalize_embeddings=True,
                chunks=1,
                min_pixels=self.min_pixels,
                max_pixels=self.max_pixels,
            )
        return [row[0].tolist() for row in rows]


class BadRequest(Exception):
    pass


def decode_image(value: str):
    from PIL import Image

    if value.startswith("data:"):
        _, _, value = value.partition(",")
    try:
        raw = base64.b64decode(value, validate=False)
    except (binascii.Error, ValueError) as error:
        raise BadRequest(f"image is not valid base64: {error}") from error
    try:
        with Image.open(io.BytesIO(raw)) as image:
            return image.convert("RGB")
    except Exception as error:  # PIL raises several unrelated types
        raise BadRequest(f"cannot decode image: {error}") from error


def split_inputs(raw) -> list[tuple[str, object]]:
    """Normalize the OpenAI `input` field to [("text"|"image", value)]."""
    if isinstance(raw, (str, dict)):
        raw = [raw]
    if not isinstance(raw, list) or not raw:
        raise BadRequest("`input` must be a non-empty string or list")
    items = []
    for entry in raw:
        if isinstance(entry, str):
            items.append(("text", entry))
        elif isinstance(entry, dict) and isinstance(entry.get("image"), str):
            items.append(("image", decode_image(entry["image"])))
        elif isinstance(entry, dict) and isinstance(entry.get("text"), str):
            items.append(("text", entry["text"]))
        else:
            raise BadRequest("each input must be a string, {\"text\": ...} or {\"image\": <data URI>}")
    return items


def make_handler(embedder: Embedder):
    class Handler(BaseHTTPRequestHandler):
        server_version = "pplx-embed-server/1.0"

        def log_message(self, fmt, *args):
            sys.stderr.write("%s - %s\n" % (self.address_string(), fmt % args))

        def send_json(self, status: int, body: dict):
            data = json.dumps(body).encode("utf-8")
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

        def send_error_json(self, status: int, message: str):
            self.send_json(status, {"error": {"message": message, "type": "invalid_request_error"}})

        def route(self) -> str:
            path = self.path.split("?", 1)[0].rstrip("/")
            return path[3:] if path.startswith("/v1") else path

        def do_GET(self):
            route = self.route()
            if route == "/models":
                self.send_json(200, {
                    "object": "list",
                    "data": [{"id": embedder.model_name, "object": "model", "owned_by": "local"}],
                })
            elif route in ("", "/health"):
                self.send_json(200, {"status": "ok", "model": embedder.model_name, "device": embedder.device})
            else:
                self.send_error_json(404, f"unknown path {self.path}")

        def do_POST(self):
            if self.route() != "/embeddings":
                self.send_error_json(404, f"unknown path {self.path}")
                return
            try:
                length = int(self.headers.get("Content-Length") or 0)
                body = json.loads(self.rfile.read(length) or b"{}")
                if not isinstance(body, dict):
                    raise BadRequest("request body must be a JSON object")
                model = body.get("model")
                if model and model not in embedder.served_names():
                    self.send_error_json(404, f"model {model!r} is not served here; use {embedder.model_name!r}")
                    return
                dimensions = body.get("dimensions")
                if dimensions not in (None, 0, 1024):
                    raise BadRequest(
                        "pplx-embed-v2-context-0.6b only produces 1024-dimensional vectors; "
                        "set Lap's Dimensions to Full"
                    )
                items = split_inputs(body.get("input"))
            except BadRequest as error:
                self.send_error_json(400, str(error))
                return
            except (ValueError, json.JSONDecodeError) as error:
                self.send_error_json(400, f"invalid JSON: {error}")
                return

            try:
                text_slots = [i for i, (kind, _) in enumerate(items) if kind == "text"]
                image_slots = [i for i, (kind, _) in enumerate(items) if kind == "image"]
                vectors: list = [None] * len(items)
                for slot, vector in zip(text_slots, embedder.embed_texts([items[i][1] for i in text_slots])):
                    vectors[slot] = vector
                for slot, vector in zip(image_slots, embedder.embed_images([items[i][1] for i in image_slots])):
                    vectors[slot] = vector
            except Exception as error:
                print(f"Embedding failed: {error!r}", file=sys.stderr, flush=True)
                self.send_error_json(500, f"embedding failed: {error}")
                return

            self.send_json(200, {
                "object": "list",
                "model": embedder.model_name,
                "data": [
                    {"object": "embedding", "index": index, "embedding": vector}
                    for index, vector in enumerate(vectors)
                ],
                "usage": {"prompt_tokens": 0, "total_tokens": 0},
            })

    return Handler


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--host", default="127.0.0.1", help="bind address (default: 127.0.0.1, local only)")
    parser.add_argument("--port", type=int, default=8090)
    parser.add_argument("--model", default=DEFAULT_MODEL, help="Hugging Face repo id or local path")
    parser.add_argument("--revision", default=None, help="pin a model revision (commit hash)")
    parser.add_argument("--device", default="auto", choices=["auto", "mps", "cuda", "cpu"])
    parser.add_argument("--dtype", default="auto", choices=["auto", "float32", "float16", "bfloat16"],
                        help="auto: bfloat16 on GPU/MPS, float32 on CPU")
    parser.add_argument("--min-pixels", type=int, default=16384)
    parser.add_argument("--max-pixels", type=int, default=1800000,
                        help="larger images are downscaled to at most this many pixels")
    args = parser.parse_args()

    embedder = Embedder(args)
    server = ThreadingHTTPServer((args.host, args.port), make_handler(embedder))
    print(f"Serving {args.model} at http://{args.host}:{args.port}/v1", flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()


if __name__ == "__main__":
    main()
