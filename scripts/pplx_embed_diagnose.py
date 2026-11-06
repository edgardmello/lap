#!/usr/bin/env python3
"""
Diagnose image-search quality for an embedding-server model in Lap.

Reads the vectors Lap stored for a model straight from the library
databases (read-only) and scores a few search queries against them through
the running embedding server. It prints the numbers that decide what Lap
shows: the score band of each query, its best matches, and how similar the
stored photo vectors are to one another (vectors that are all nearly the
same mean the server is not embedding the photos properly).

Only the Python standard library is needed:

    python3 scripts/pplx_embed_diagnose.py
    python3 scripts/pplx_embed_diagnose.py --query "cachorro" --query "dog"

Paste the whole output back into the conversation.
"""

from __future__ import annotations

import argparse
import glob
import json
import math
import os
import random
import sqlite3
import statistics
import struct
import sys
import urllib.request

DEFAULT_QUERIES = [
    "dog", "cachorro",
    "beach", "praia",
    "food", "comida",
    "a smiling person", "pessoa sorrindo",
    "car", "carro",
    "a spaceship on the moon",
]


def data_dirs() -> list[str]:
    home = os.path.expanduser("~")
    roots = [
        os.path.join(home, "Library", "Application Support"),
        os.path.join(home, ".local", "share"),
        os.environ.get("XDG_DATA_HOME", ""),
    ]
    found = []
    for root in roots:
        if root:
            found += glob.glob(os.path.join(root, "com.*lap*"))
    return sorted(set(found))


def find_databases(extra: list[str]) -> list[str]:
    paths = list(extra)
    for directory in data_dirs():
        for pattern in ("*.db", "**/*.db", "*.sqlite", "**/*.sqlite"):
            paths += glob.glob(os.path.join(directory, pattern), recursive=True)
    result = []
    for path in sorted(set(paths)):
        try:
            conn = sqlite3.connect(f"file:{path}?mode=ro", uri=True)
            ok = conn.execute(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name='afiles'"
            ).fetchone()
            conn.close()
            if ok:
                result.append(path)
        except sqlite3.Error:
            pass
    return result


def unpack(blob: bytes) -> list[float]:
    return list(struct.unpack(f"<{len(blob) // 4}f", blob))


def norm(vector: list[float]) -> list[float]:
    length = math.sqrt(sum(x * x for x in vector)) or 1.0
    return [x / length for x in vector]


def dot(a: list[float], b: list[float]) -> float:
    return sum(x * y for x, y in zip(a, b))


def embed_queries(url: str, model: str, api_key: str, texts: list[str]) -> list[list[float]]:
    body = json.dumps({"model": model, "input": texts, "encoding_format": "float"}).encode()
    request = urllib.request.Request(
        url.rstrip("/") + "/embeddings", body, {"Content-Type": "application/json"}
    )
    if api_key:
        request.add_header("Authorization", f"Bearer {api_key}")
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    with opener.open(request, timeout=300) as response:
        data = json.load(response)["data"]
    data.sort(key=lambda entry: entry.get("index", 0))
    return [entry["embedding"] for entry in data]


def describe(scores: list[float]) -> str:
    ordered = sorted(scores)
    p = lambda q: ordered[min(len(ordered) - 1, int(q * len(ordered)))]
    return (
        f"mean={statistics.fmean(scores):.3f} sd={statistics.pstdev(scores):.3f} "
        f"p50={p(0.5):.3f} p99={p(0.99):.3f} max={ordered[-1]:.3f}"
    )


def main():
    parser = argparse.ArgumentParser(description="Diagnose Lap image-search scores")
    parser.add_argument("--url", default="http://127.0.0.1:8090/v1")
    parser.add_argument("--model", default="perplexity-ai/pplx-embed-v2-context-0.6b")
    parser.add_argument("--api-key", default="")
    parser.add_argument("--db", action="append", default=[], help="library database path (repeatable)")
    parser.add_argument("--query", action="append", default=[], help="search text (repeatable)")
    parser.add_argument("--top", type=int, default=5)
    args = parser.parse_args()

    databases = find_databases(args.db)
    if not databases:
        sys.exit("No Lap library database found; pass one with --db <path>")

    print("== Stored vectors ==")
    rows = []
    for path in databases:
        conn = sqlite3.connect(f"file:{path}?mode=ro", uri=True)
        counts = conn.execute(
            "SELECT COALESCE(embed_model, 'clip-vit-b32'), length(embeds) / 4, COUNT(*) "
            "FROM afiles WHERE embeds IS NOT NULL GROUP BY 1, 2"
        ).fetchall()
        print(path)
        for model_id, dims, count in counts:
            print(f"   {model_id}  dims={dims}  files={count}")
        for file_id, name, blob in conn.execute(
            "SELECT id, name, embeds FROM afiles WHERE embeds IS NOT NULL AND embed_model LIKE ?",
            (f"%{args.model}%",),
        ):
            rows.append((os.path.basename(path), name, norm(unpack(blob))))
        conn.close()
    if not rows:
        sys.exit(f"No stored vectors for a model matching {args.model!r}; re-index first")
    print(f"Using {len(rows)} vectors of {args.model}")

    print("\n== Photo-to-photo similarity (random pairs) ==")
    rng = random.Random(0)
    pairs = [rng.sample(range(len(rows)), 2) for _ in range(min(2000, len(rows) * 3))] if len(rows) > 1 else []
    if pairs:
        print("   " + describe([dot(rows[a][2], rows[b][2]) for a, b in pairs]))
        print("   (a mean above ~0.9 means the photos all got nearly the same vector)")

    queries = args.query or DEFAULT_QUERIES
    print(f"\n== Queries via {args.url} ==")
    vectors = [norm(v) for v in embed_queries(args.url, args.model, args.api_key, queries)]
    for query, vector in zip(queries, vectors):
        scored = sorted(((dot(vector, v), db, name) for db, name, v in rows), reverse=True)
        print(f"\n'{query}': {describe([s for s, _, _ in scored])}")
        for score, db, name in scored[: args.top]:
            print(f"   {score:.3f}  {name}")
    if len(vectors) > 1:
        print("\n== Query-to-query similarity ==")
        for i in range(0, len(queries) - 1, 2):
            print(f"   '{queries[i]}' vs '{queries[i + 1]}': {dot(vectors[i], vectors[i + 1]):.3f}")


if __name__ == "__main__":
    main()
