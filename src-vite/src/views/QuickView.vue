<template>
  <main class="h-screen flex flex-col bg-base-300 text-base-content select-none"
    @dragenter.prevent="dragEnter" @dragover.prevent="dragOver" @dragleave="dragLeave" @drop.prevent="drop">
    <header class="quick-header">
      <div class="min-w-0">
        <h1 class="text-lg font-semibold">{{ $t('quickView.title') }}</h1>
        <p class="text-xs opacity-60 mt-1">{{ $t('quickView.session') }}</p>
      </div>
      <button class="btn btn-sm" @click="chooseFiles">{{ $t('quickView.add') }}</button>
      <button class="btn btn-sm btn-ghost" :disabled="!files.length || collecting" @click="clear">{{ $t('quickView.clear') }}</button>
    </header>

    <section class="quick-toolbar" :aria-label="$t('quickView.actions')">
      <span class="text-xs tabular-nums mr-auto" role="status">{{ $t('quickView.count', { total: files.length, selected: selected.size }) }}</span>
      <button class="btn btn-sm" :disabled="!selected.size || copying" @click="copy">{{ $t('quickView.copy') }}</button>
      <button class="btn btn-sm btn-ghost" :disabled="!selected.size" @click="removeSelected">{{ $t('quickView.remove') }}</button>
      <label class="quick-size text-xs opacity-70">{{ $t('quickView.size') }}
        <input v-model.number="tileSize" type="range" min="120" max="320" step="20" :aria-label="$t('quickView.size')" />
      </label>
    </section>

    <p v-if="message" class="px-6 py-2 text-sm bg-base-200" role="status">{{ message }}</p>
    <details v-if="errors.length" class="px-6 py-2 text-sm bg-base-200">
      <summary class="cursor-pointer">{{ $t('quickView.errors', { count: errors.length }) }}</summary>
      <ul class="max-h-32 overflow-auto mt-2 select-text"><li v-for="(error, i) in errors" :key="i">{{ error.path }}: {{ error.message }}</li></ul>
    </details>
    <div v-if="collecting" class="px-6 py-2 text-sm" role="status">{{ $t('quickView.loading') }}</div>

    <section v-if="files.length" ref="grid" class="quick-grid flex-1 min-h-0 overflow-auto" role="group"
      :aria-label="$t('quickView.title')" :style="{ '--tile-size': `${tileSize}px` }">
      <QuickViewTile v-for="file in files" :key="file.path" :file="file" :selected="selected.has(file.path)" :load="loadThumbnail"
        @select="selectFile(file.path, $event)" @preview="showPreview(file)" @pointerdown="pointerDown(file.path, $event)" />
    </section>
    <div v-else class="quick-empty flex-1" :class="{ 'is-over': over }">
      <IconPhotoAll class="w-12 h-12 opacity-30 mb-5" aria-hidden="true" />
      <h2 class="text-xl font-medium">{{ $t('quickView.drop') }}</h2>
      <p class="text-sm opacity-60 mt-2 max-w-sm text-center">{{ $t('quickView.emptyHelp') }}</p>
      <button class="btn btn-sm mt-6" @click="chooseFiles">{{ $t('quickView.add') }}</button>
    </div>
    <footer class="px-6 py-3 border-t border-base-content/10 text-xs opacity-60">{{ $t('quickView.hint') }}</footer>
    <div v-if="over && files.length" class="quick-drop-overlay" aria-hidden="true">{{ $t('quickView.drop') }}</div>

    <div v-if="preview" class="quick-preview" role="dialog" aria-modal="true" :aria-label="preview.name" @click.self="closePreview">
      <header class="flex items-center gap-4 p-4 bg-base-200">
        <span class="flex-1 truncate">{{ preview.name }}</span>
        <button ref="previewClose" class="btn btn-sm" @click="closePreview">{{ $t('quickView.close') }}</button>
      </header>
      <img v-if="previewSource" :src="previewSource" :alt="preview.name" />
      <p v-else class="m-auto" role="status">{{ previewError ? $t('quickView.unavailable') : $t('quickView.loading') }}</p>
    </div>
  </main>
</template>

<script setup lang="ts">
import { ref, onMounted, onBeforeUnmount, nextTick } from 'vue';
import { useI18n } from 'vue-i18n';
import { invoke } from '@tauri-apps/api/core';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { open } from '@tauri-apps/plugin-dialog';
import { getExternalDropPaths } from '@/common/externalDrop';
import { createDragPreview, startNativeFileDrag } from '@/common/nativeDrag';
import { isMac } from '@/common/utils';
import { IconPhotoAll } from '@/common/icons';
import { mergeQuickViewFiles, selectQuickViewFile, type QuickViewFile } from '@/common/quickView';
import QuickViewTile from '@/components/QuickViewTile.vue';

const { t } = useI18n();
const files = ref<QuickViewFile[]>([]);
const selected = ref(new Set<string>());
const tileSize = ref(180);
const collecting = ref(false), copying = ref(false), over = ref(false);
const message = ref('');
const errors = ref<{ path: string; message: string }[]>([]);
const grid = ref<HTMLElement>();
const preview = ref<QuickViewFile | null>(null), previewSource = ref<string | null>(null), previewError = ref(false);
const previewClose = ref<HTMLButtonElement>();
let previousFocus: HTMLElement | null = null;
let anchor: string | null = null, depth = 0, disposed = false, previewGeneration = 0;
let collectionQueue: Promise<void> = Promise.resolve();

function add(paths: string[]) {
  collectionQueue = collectionQueue.then(async () => {
    if (disposed) return;
    collecting.value = true; message.value = ''; errors.value = [];
    try {
      const result = await invoke<{ files: QuickViewFile[]; errors: { path: string; message: string }[]; truncated: boolean }>('quick_view_collect', { paths });
      if (disposed) return;
      const merged = mergeQuickViewFiles(files.value, result.files);
      files.value = merged.files; errors.value = result.errors;
      if (result.truncated || merged.truncated) message.value = t('quickView.limit');
      else if (!result.files.length && !result.errors.length) message.value = t('quickView.noImages');
    } catch (error) { if (!disposed) message.value = String(error); }
    finally { collecting.value = false; }
  });
  return collectionQueue;
}
async function chooseFiles() {
  try {
    const paths = await open({ multiple: true, directory: false, title: t('quickView.add') });
    if (paths) await add(Array.isArray(paths) ? paths : [paths]);
  } catch (error) { message.value = String(error); }
}
function dragEnter() { depth++; over.value = true; }
function dragOver(event: DragEvent) { if (event.dataTransfer) event.dataTransfer.dropEffect = 'copy'; }
function dragLeave() { depth = Math.max(0, depth - 1); if (!depth) over.value = false; }
async function drop(event: DragEvent) {
  depth = 0; over.value = false;
  // LAP's other windows also offer original file URLs. Accept them here;
  // merging by canonical path makes a drag returning to this session harmless.
  const paths = getExternalDropPaths(event.dataTransfer);
  // Snapshot the native pasteboard before any other asynchronous operation.
  const native = isMac ? invoke<{ filePaths?: string[] }>('get_drag_payload') : Promise.resolve(null);
  try {
    const payload = await native;
    const all = [...new Set([...paths, ...(payload?.filePaths || [])])];
    if (all.length) await add(all);
    else message.value = t('quickView.localOnly');
  } catch (error) { message.value = String(error); }
}
function selectFile(path: string, event: MouseEvent) {
  if (suppressClick) { suppressClick = false; return; }
  selected.value = selectQuickViewFile(files.value.map(file => file.path), selected.value, path, anchor, event.metaKey || event.ctrlKey, event.shiftKey);
  if (!event.shiftKey) anchor = path;
}
function removeSelected() {
  files.value = files.value.filter(file => !selected.value.has(file.path));
  selected.value = new Set(); anchor = null; message.value = '';
}
function clear() { files.value = []; selected.value = new Set(); anchor = null; errors.value = []; message.value = ''; }
async function copy() {
  if (!selected.value.size || copying.value) return;
  copying.value = true;
  try {
    const count = await invoke<number>('quick_view_copy', { paths: files.value.filter(file => selected.value.has(file.path)).map(file => file.path) });
    message.value = t('quickView.copied', { count });
  } catch (error) { message.value = String(error); }
  finally { copying.value = false; }
}

// Only cells near the viewport request thumbnails. Four decodes may be active;
// queued cells that scrolled away never decode, and offscreen sources are released.
let activeLoads = 0;
const loads: { path: string; active: () => boolean; resolve: (value: string | null) => void }[] = [];
function pump() {
  while (!disposed && activeLoads < 4 && loads.length) {
    const job = loads.shift()!;
    if (!job.active()) { job.resolve(null); continue; }
    activeLoads++;
    void invoke<{ dataUrl: string }>('quick_view_thumbnail', { path: job.path, size: 480 })
      .then(result => job.resolve(job.active() && !disposed ? result.dataUrl : null))
      .catch(() => job.resolve(null))
      .finally(() => { activeLoads--; pump(); });
  }
}
function loadThumbnail(path: string, active: () => boolean): Promise<string | null> {
  return new Promise(resolve => { loads.push({ path, active, resolve }); pump(); });
}
async function showPreview(file: QuickViewFile) {
  const generation = ++previewGeneration;
  previousFocus = document.activeElement as HTMLElement;
  preview.value = file; previewSource.value = null; previewError.value = false;
  await nextTick(); previewClose.value?.focus();
  try {
    const result = await invoke<{ dataUrl: string }>('quick_view_thumbnail', { path: file.path, size: 1600 });
    if (generation === previewGeneration && !disposed) previewSource.value = result.dataUrl;
  } catch { if (generation === previewGeneration) previewError.value = true; }
}
function closePreview() { previewGeneration++; preview.value = null; previewSource.value = null; previousFocus?.focus(); }

let pendingDrag: { path: string; x: number; y: number; image: HTMLImageElement | null } | null = null;
let suppressClick = false;
function pointerDown(path: string, event: PointerEvent) {
  if (event.button !== 0) return;
  suppressClick = false;
  pendingDrag = { path, x: event.clientX, y: event.clientY, image: (event.currentTarget as HTMLElement).querySelector('img') };
}
function pointerMove(event: PointerEvent) {
  if (!pendingDrag || !(event.buttons & 1) || Math.hypot(event.clientX - pendingDrag.x, event.clientY - pendingDrag.y) < 7) return;
  const drag = pendingDrag; pendingDrag = null; suppressClick = true;
  if (!selected.value.has(drag.path)) { selected.value = new Set([drag.path]); anchor = drag.path; }
  const paths = files.value.filter(file => selected.value.has(file.path)).map(file => file.path);
  void startNativeFileDrag(paths, createDragPreview(drag.image)).catch(error => { message.value = String(error); });
}
function pointerUp() { pendingDrag = null; }
function keyDown(event: KeyboardEvent) {
  if (preview.value) {
    if (event.key === 'Escape' || event.code === 'Space') { event.preventDefault(); closePreview(); }
    if (event.key === 'Tab') { event.preventDefault(); previewClose.value?.focus(); }
    return;
  }
  if ((event.target as HTMLElement)?.matches('input,textarea,select')) return;
  const modifier = event.metaKey || event.ctrlKey;
  if (modifier && event.key.toLowerCase() === 'a') { event.preventDefault(); selected.value = new Set(files.value.map(file => file.path)); }
  else if (modifier && event.key.toLowerCase() === 'c') { event.preventDefault(); void copy(); }
  else if (event.key === 'Escape') selected.value = new Set();
  else if (event.key === 'Delete' || event.key === 'Backspace') { event.preventDefault(); removeSelected(); }
  else if (event.code === 'Space' && selected.value.size) {
    event.preventDefault(); const file = files.value.find(file => selected.value.has(file.path)); if (file) void showPreview(file);
  }
}
onMounted(async () => {
  window.addEventListener('keydown', keyDown);
  window.addEventListener('pointermove', pointerMove);
  window.addEventListener('pointerup', pointerUp);
  window.addEventListener('pointercancel', pointerUp);
  try { await getCurrentWindow().show(); } catch (error) { console.error(error); }
});
onBeforeUnmount(() => {
  disposed = true; previewGeneration++; pendingDrag = null;
  loads.splice(0).forEach(job => job.resolve(null));
  window.removeEventListener('keydown', keyDown);
  window.removeEventListener('pointermove', pointerMove);
  window.removeEventListener('pointerup', pointerUp);
  window.removeEventListener('pointercancel', pointerUp);
});
</script>

<style scoped>
.quick-header { display: flex; gap: 12px; align-items: center; padding: 22px 24px 18px; border-bottom: 1px solid color-mix(in oklab, var(--color-base-content) 10%, transparent); }
.quick-header > div { flex: 1; }
.quick-toolbar { display: flex; align-items: center; gap: 10px; padding: 12px 24px; flex-wrap: wrap; }
.quick-size { display: flex; align-items: center; gap: 8px; margin-left: 12px; }
.quick-size input { width: 90px; accent-color: var(--color-primary); }
.quick-grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(var(--tile-size), 1fr)); align-content: start; padding: 10px 20px 24px; gap: 12px; }
.quick-empty { display: flex; flex-direction: column; align-items: center; justify-content: center; margin: 18px 24px; border: 1px dashed color-mix(in oklab, var(--color-base-content) 20%, transparent); border-radius: 8px; }
.quick-empty.is-over { background: var(--color-base-200); border-color: var(--color-primary); }
.quick-drop-overlay { position: fixed; inset: 10px; pointer-events: none; display: flex; align-items: center; justify-content: center; font-size: 24px; background: color-mix(in oklab, var(--color-base-200) 94%, transparent); border: 2px dashed var(--color-primary); border-radius: 8px; z-index: 20; }
.quick-preview { position: fixed; inset: 0; z-index: 30; display: flex; flex-direction: column; background: var(--color-base-300); }
.quick-preview > img { flex: 1; min-height: 0; width: 100%; object-fit: contain; padding: 16px; }
@media (max-width: 700px) { .quick-size { width: 100%; margin-left: 0; } .quick-header { padding: 16px; } }
</style>
