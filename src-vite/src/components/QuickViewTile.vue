<template>
  <button ref="tile" type="button" class="quick-tile" :class="{ 'is-selected': selected }"
    :aria-pressed="selected" :aria-label="file.name" :title="file.path" draggable="false"
    @click="$emit('select', $event)" @dblclick="$emit('preview')"
    @pointerdown="$emit('pointerdown', $event)" @dragstart.prevent>
    <span class="quick-tile-image">
      <img v-if="source" :src="source" :alt="file.name" draggable="false" />
      <span v-else-if="failed" class="text-xs opacity-60">{{ $t('quickView.unavailable') }}</span>
      <span v-else class="loading loading-spinner loading-xs opacity-30" aria-hidden="true"></span>
      <span v-if="selected" class="quick-tile-check" aria-hidden="true">✓</span>
    </span>
    <span class="quick-tile-name">{{ file.name }}</span>
  </button>
</template>

<script setup lang="ts">
import { ref, onMounted, onBeforeUnmount } from 'vue';
import type { QuickViewFile } from '@/common/quickView';
const props = defineProps<{ file: QuickViewFile; selected: boolean;
  load: (path: string, active: () => boolean) => Promise<string | null> }>();
defineEmits(['select', 'preview', 'pointerdown']);
const tile = ref<HTMLElement>();
const source = ref<string | null>(null);
const failed = ref(false);
let observer: IntersectionObserver | undefined;
let generation = 0;
onMounted(() => {
  observer = new IntersectionObserver(entries => {
    const visible = entries[0].isIntersecting;
    const current = ++generation;
    if (!visible) { source.value = null; return; }
    failed.value = false;
    void props.load(props.file.path, () => current === generation).then(value => {
      if (current === generation) { source.value = value; failed.value = !value; }
    }).catch(() => { if (current === generation) failed.value = true; });
  }, { rootMargin: '200px' });
  if (tile.value) observer.observe(tile.value);
});
onBeforeUnmount(() => { generation++; observer?.disconnect(); });
</script>

<style scoped>
.quick-tile { display: flex; min-width: 0; flex-direction: column; padding: 5px; gap: 7px; border-radius: 6px; border: 2px solid transparent; text-align: left; cursor: default; }
.quick-tile:hover { background: var(--color-base-200); }
.quick-tile:focus-visible { outline: 2px solid var(--color-primary); outline-offset: 2px; }
.quick-tile.is-selected { border-color: var(--color-primary); background: color-mix(in oklab, var(--color-primary) 12%, var(--color-base-200)); }
.quick-tile-image { position: relative; aspect-ratio: 4 / 3; display: flex; align-items: center; justify-content: center; background: var(--color-base-200); border-radius: 3px; overflow: hidden; }
.quick-tile-image img { width: 100%; height: 100%; object-fit: contain; pointer-events: none; }
.quick-tile-name { font-size: 12px; line-height: 1.4; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; padding: 0 3px 2px; }
.quick-tile-check { position: absolute; top: 7px; right: 7px; background: var(--color-primary); color: var(--color-primary-content); border-radius: 50%; width: 21px; height: 21px; font-size: 13px; text-align: center; }
</style>
