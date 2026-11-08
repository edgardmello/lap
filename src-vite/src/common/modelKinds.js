// Servers list embedding and chat models together in /v1/models without
// saying which is which, so the model pickers in Settings tell them apart by
// name: embedding models are named after what they do (…-embed…, bge, e5,
// gte, …) or after the image-text encoders they are built on (clip, siglip).
const EMBEDDING_NAME = /embed|(^|[^a-z])(bge|e5|gte|clip|siglip|minilm|mpnet|rerank(er)?)([^a-z]|$)/i;

export function isEmbeddingModelName(name) {
  return EMBEDDING_NAME.test(String(name || ''));
}

// Embedding picker: every model, embedding models first.
export function embeddingModelChoices(models) {
  const list = models || [];
  return [...list.filter(isEmbeddingModelName), ...list.filter(model => !isEmbeddingModelName(model))];
}

// Translation picker: chat models only.
export function chatModelChoices(models) {
  return (models || []).filter(model => !isEmbeddingModelName(model));
}
