import test from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import vm from 'node:vm';
import { defineStore, createPinia, setActivePinia } from 'pinia';

const storeSource = await readFile(new URL('../src/stores/configStore.js', import.meta.url), 'utf8');

async function configStore() {
  const context = vm.createContext({ console });
  const synthetic = exports => new vm.SyntheticModule(Object.keys(exports), function () {
    for (const [key, value] of Object.entries(exports)) this.setExport(key, value);
  }, { context });
  const module = new vm.SourceTextModule(storeSource, { context });
  await module.link(specifier => {
    if (specifier === 'pinia') return synthetic({ defineStore });
    if (specifier === '@/common/i18n') return synthetic({ loadLanguage: async () => {}, i18n: { global: {} } });
    return synthetic({ SIDEBAR: { ALBUM: 1 }, MAP_MARKER_SIZES: [10, 20] });
  });
  await module.evaluate();
  setActivePinia(createPinia());
  return module.namespace.useConfigStore();
}

test('switching provider applies that provider\'s default strictness', async () => {
  const config = await configStore();
  assert.equal(config.settings.imageSearch.thresholdIndex, 2);

  config.setImageSearchProvider('omlx');
  assert.equal(config.settings.imageSearch.thresholdIndex, 1, 'oMLX defaults to Focused');

  config.setImageSearchThresholdIndex(3);
  config.setImageSearchProvider('omlx');
  assert.equal(config.settings.imageSearch.thresholdIndex, 3, 're-selecting the same provider keeps the user choice');

  config.setImageSearchProvider('builtin');
  assert.equal(config.settings.imageSearch.thresholdIndex, 2, 'built-in CLIP returns to Standard');
});

test('moving between embedding servers keeps the strictness and each server\'s connection', async () => {
  const config = await configStore();
  config.setImageSearchProvider('ollama');
  assert.equal(config.settings.imageSearch.thresholdIndex, 1, 'server models default to Focused');
  config.setImageSearchThresholdIndex(0);
  config.setImageSearchProvider('openai');
  assert.equal(config.settings.imageSearch.thresholdIndex, 0, 'servers share the score scale');
  assert.deepEqual([...config.imageSearchThresholds], [0.69, 0.67, 0.65, 0.62]);

  config.setImageSearchServerSettings('ollama', { model: 'embeddinggemma-2:740m' });
  assert.equal(config.settings.imageSearch.ollama.model, 'embeddinggemma-2:740m');
  assert.equal(config.settings.imageSearch.ollama.url, 'http://127.0.0.1:11434');
  assert.equal(config.settings.imageSearch.omlx.model, 'mlx-community/embeddinggemma-2-bf16');

  config.setImageSearchProvider('unknown');
  assert.equal(config.settings.imageSearch.provider, 'builtin');
});

test('the main window takes the provider and connections chosen in Settings', async () => {
  const config = await configStore();
  config.setImageSearchThresholdIndex(3);
  config.syncImageSearchProvider({
    provider: 'ollama',
    ollama: { url: 'http://mac:11434', model: 'embeddinggemma-2:740m' },
  });
  const search = config.settings.imageSearch;
  assert.equal(search.provider, 'ollama');
  assert.equal(search.ollama.url, 'http://mac:11434');
  assert.equal(search.ollama.dimensionsIndex, 0, 'missing fields fall back to defaults');
  assert.equal(search.omlx.url, 'http://127.0.0.1:8000', 'servers not in the payload are kept');
  assert.equal(search.thresholdIndex, 3, 'strictness syncs separately');
});

test('Settings sends provider changes to the main window, which applies them', async () => {
  const settings = await readFile(new URL('../src/views/Settings.vue', import.meta.url), 'utf8');
  const main = await readFile(new URL('../src/main.js', import.meta.url), 'utf8');
  assert.match(settings, /emit\('settings-imageSearchProvider-changed'/);
  assert.match(main, /listen\('settings-imageSearchProvider-changed'[\s\S]{0,80}syncImageSearchProvider/);
});
