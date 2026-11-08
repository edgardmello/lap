import test from 'node:test';
import assert from 'node:assert/strict';
import { chatModelChoices, embeddingModelChoices, isEmbeddingModelName } from '../src/common/modelKinds.js';

test('embedding models are told apart from chat models by name', () => {
  for (const name of [
    'perplexity-ai/pplx-embed-v2-context-0.6b', 'embeddinggemma-2-8bit', 'mlx-community/embeddinggemma-2-8bit',
    'embeddinggemma-2:latest', 'nomic-embed-text', 'BAAI/bge-m3', 'intfloat/multilingual-e5-large',
    'thenlper/gte-base', 'jina-clip-v2', 'google/siglip-so400m', 'all-MiniLM-L6-v2', 'text-embedding-3-small',
  ]) assert.ok(isEmbeddingModelName(name), name);
  for (const name of [
    'Qwen2.5-VL-3B-Instruct-4bit', 'Qwen3.5-2B-MLX-4bit', 'llama3.2:3b', 'gemma3:4b', 'gpt-4o-mini',
    'mistral-nemo', 'phi4', 'deepseek-r1:8b',
  ]) assert.ok(!isEmbeddingModelName(name), name);
});

test('the translation picker lists only chat models and the embedding picker lists embeddings first', () => {
  const models = ['Qwen3.5-2B-MLX-4bit', 'embeddinggemma-2-8bit', 'perplexity-ai/pplx-embed-v2-context-0.6b'];
  assert.deepEqual(chatModelChoices(models), ['Qwen3.5-2B-MLX-4bit']);
  assert.deepEqual(embeddingModelChoices(models), [
    'embeddinggemma-2-8bit', 'perplexity-ai/pplx-embed-v2-context-0.6b', 'Qwen3.5-2B-MLX-4bit',
  ]);
  assert.deepEqual(chatModelChoices(['perplexity-ai/pplx-embed-v2-context-0.6b']), []);
});
