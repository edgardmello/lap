<template>
  <aside
    class="keyword-panel bg-base-200 text-base-content"
    :aria-label="$t('keywords.title')"
    @keydown.stop
  >
    <header
      class="flex items-center gap-2 px-4 py-3 border-b border-base-content/10"
    >
      <h2 class="font-semibold text-sm flex-1">{{ $t("keywords.title") }}</h2>
      <button
        class="btn btn-xs btn-ghost"
        :aria-label="$t('libraryFilter.close')"
        @click="$emit('close')"
      >
        ×
      </button>
    </header>
    <div class="flex-1 min-h-0 overflow-auto">
      <section class="keyword-section">
        <h3>{{ $t("keywords.keywording") }}</h3>
        <p class="text-xs opacity-60 mb-3" role="status">
          {{ $t("keywords.selection", { count: fileIds.length }) }}
        </p>
        <div class="flex flex-wrap gap-1 mb-3">
          <button
            v-for="keyword in assigned"
            :key="keyword.id"
            class="keyword-chip"
            :disabled="busy"
            :title="keyword.name"
            @click="remove(keyword)"
          >
            {{ keywordLeaf(keyword.name)
            }}<span
              v-if="keyword.selectedCount < fileIds.length"
              class="opacity-60"
            >
              *</span
            ><span class="ml-1 opacity-50">×</span>
          </button>
        </div>
        <form @submit.prevent="addTyped">
          <textarea
            v-model="input"
            rows="2"
            class="textarea textarea-sm w-full resize-y"
            :placeholder="$t('keywords.placeholder')"
            :aria-label="$t('keywords.add')"
            :disabled="busy || !fileIds.length"
          ></textarea>
          <p class="text-[11px] opacity-50 mt-1 mb-2">
            {{ $t("keywords.inputHint") }}
          </p>
          <button
            class="btn btn-sm w-full"
            :disabled="busy || !fileIds.length || !input.trim()"
          >
            {{ $t("keywords.add") }}
          </button>
        </form>
        <p
          v-if="assigned.some((k) => k.selectedCount < fileIds.length)"
          class="text-[11px] opacity-60 mt-2"
        >
          {{ $t("keywords.mixed") }}
        </p>
        <div class="flex gap-2 items-center mt-4">
          <span class="text-xs mr-auto">{{ $t("keywords.label") }}</span>
          <button
            v-for="color in colors"
            :key="color"
            type="button"
            class="keyword-color"
            :disabled="busy || !fileIds.length"
            :style="{ background: colorValues[color] }"
            :aria-label="$t(`libraryFilter.colors.${color}`)"
            @click="setLabel(color)"
          ></button>
          <button
            class="btn btn-xs btn-ghost px-1"
            :disabled="busy || !fileIds.length"
            :aria-label="$t('keywords.clearLabel')"
            @click="setLabel('')"
          >
            ×
          </button>
        </div>
      </section>
      <p
        v-if="error"
        class="px-4 pb-3 text-xs text-error select-text"
        role="alert"
      >
        {{ error }}
      </p>
      <p v-if="status" class="px-4 pb-3 text-xs opacity-60" role="status">
        {{ status }}
      </p>
      <section class="keyword-section">
        <h3>{{ $t("keywords.suggestions") }}</h3>
        <p class="text-[11px] opacity-50 mb-3">
          {{ $t("keywords.suggestionHint") }}
        </p>
        <div v-if="suggested.length" class="grid grid-cols-2 gap-1">
          <button
            v-for="keyword in suggested"
            :key="keyword.id"
            class="keyword-suggestion"
            :disabled="busy || !fileIds.length"
            :title="keyword.name"
            @click="apply([keyword.id], [])"
          >
            {{ keywordLeaf(keyword.name) }}
          </button>
        </div>
        <p v-else class="text-xs opacity-50">
          {{
            $t(
              fileIds.length
                ? "keywords.noSuggestions"
                : "keywords.selectPhotos",
            )
          }}
        </p>
      </section>
      <section class="keyword-section">
        <h3>
          {{ $t("keywords.list") }}
          <span class="opacity-50 font-normal">{{ keywords.length }}</span>
        </h3>
        <input
          v-model="search"
          type="search"
          class="input input-sm w-full mb-3"
          :placeholder="$t('keywords.find')"
          :aria-label="$t('keywords.find')"
        />
        <div v-if="loading" class="text-xs opacity-50" role="status">
          {{ $t("keywords.loading") }}
        </div>
        <template v-for="group in groups" :key="group.name">
          <h4
            class="text-[11px] uppercase tracking-wider opacity-50 pt-3 pb-2 truncate"
          >
            {{ group.name }}
          </h4>
          <div
            v-for="keyword in group.items"
            :key="keyword.id"
            class="keyword-row"
            :style="{
              paddingLeft: `${Math.min(keywordDepth(keyword.name), 8) * 12}px`,
            }"
          >
            <input
              type="checkbox"
              class="checkbox checkbox-xs shrink-0"
              :checked="
                fileIds.length > 0 && keyword.selectedCount === fileIds.length
              "
              :indeterminate.prop="
                keyword.selectedCount > 0 &&
                keyword.selectedCount < fileIds.length
              "
              :disabled="busy || !fileIds.length"
              :aria-label="$t('keywords.apply', { name: keyword.name })"
              @change="toggle(keyword)"
            />
            <button
              v-if="hasChildren(keyword)"
              class="w-3 shrink-0 text-xs opacity-60"
              :aria-label="keyword.name"
              :aria-expanded="!collapsed.has(keyword.id)"
              @click="toggleCollapsed(keyword.id)"
            >
              {{ collapsed.has(keyword.id) ? "▸" : "▾" }}
            </button>
            <span v-else class="w-3 shrink-0"></span>
            <span class="truncate flex-1 text-xs" :title="keyword.name">{{
              keywordLeaf(keyword.name)
            }}</span>
            <span class="text-[11px] opacity-50 tabular-nums">{{
              keyword.count.toLocaleString()
            }}</span>
            <button
              class="keyword-arrow"
              :aria-label="$t('keywords.filter', { name: keyword.name })"
              :title="$t('keywords.filter', { name: keyword.name })"
              @click="$emit('filter', keyword.id)"
            >
              →
            </button>
          </div>
        </template>
        <p v-if="!loading && !groups.length" class="text-xs opacity-50">
          {{ $t("keywords.empty") }}
        </p>
      </section>
    </div>
  </aside>
</template>

<script setup lang="ts">
import { ref, computed, watch, onMounted, onBeforeUnmount } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { emit as tauriEmit, listen } from "@tauri-apps/api/event";
import { useI18n } from "vue-i18n";
import {
  keywordLeaf,
  keywordDepth,
  parseKeywordInput,
} from "@/common/libraryFilter";
const props = defineProps<{ fileIds: number[]; revision: number }>();
const emit = defineEmits(["close", "filter", "changed", "label-changed"]);
const { t } = useI18n();
interface Keyword {
  id: number;
  name: string;
  groupName: string;
  count: number;
  selectedCount: number;
}
const keywords = ref<Keyword[]>([]),
  suggestionIds = ref<number[]>([]),
  collapsed = ref(new Set<number>());
const input = ref(""),
  search = ref(""),
  error = ref(""),
  status = ref(""),
  busy = ref(false),
  loading = ref(false);
const colors = ["red", "yellow", "green", "blue", "purple"];
const colorValues: Record<string, string> = {
  red: "#d96463",
  yellow: "#d2ba62",
  green: "#69a07a",
  blue: "#668fb7",
  purple: "#a184bc",
};
let request = 0,
  disposed = false,
  stop: (() => void) | undefined,
  refreshTimer: ReturnType<typeof setTimeout>;
const assigned = computed(() =>
  keywords.value.filter((k) => k.selectedCount > 0),
);
const suggested = computed(
  () =>
    suggestionIds.value
      .map((id) => keywords.value.find((k) => k.id === id))
      .filter(Boolean) as Keyword[],
);
const groups = computed(() => {
  const map = new Map<string, Keyword[]>();
  const query = search.value.trim().toLocaleLowerCase();
  for (const keyword of keywords.value) {
    if (query && !keyword.name.toLocaleLowerCase().includes(query)) continue;
    if (
      !query &&
      keywords.value.some(
        (parent) =>
          collapsed.value.has(parent.id) &&
          keyword.name.startsWith(parent.name + " > "),
      )
    )
      continue;
    const name = keyword.groupName || t("keywords.ungrouped");
    if (!map.has(name)) map.set(name, []);
    map.get(name)!.push(keyword);
  }
  return [...map].map(([name, items]) => ({ name, items }));
});
function hasChildren(keyword: Keyword) {
  return keywords.value.some((child) =>
    child.name.startsWith(keyword.name + " > "),
  );
}
function toggleCollapsed(id: number) {
  const next = new Set(collapsed.value);
  next.has(id) ? next.delete(id) : next.add(id);
  collapsed.value = next;
}
async function refresh() {
  const id = ++request;
  loading.value = true;
  try {
    const result = await invoke<{ keywords: Keyword[]; suggestions: number[] }>(
      "get_keyword_panel",
      { fileIds: [...props.fileIds] },
    );
    if (id === request && !disposed) {
      keywords.value = result.keywords;
      suggestionIds.value = result.suggestions;
    }
  } catch (e) {
    if (id === request) error.value = String(e);
  } finally {
    if (id === request) loading.value = false;
  }
}
async function notify(states: any[]) {
  emit("changed", states);
  await tauriEmit("tags-changed");
  await refresh();
}
async function apply(addTagIds: number[], removeTagIds: number[]) {
  if (busy.value || !props.fileIds.length) return;
  busy.value = true;
  error.value = "";
  status.value = "";
  const ids = [...props.fileIds];
  try {
    const states = await invoke<any[]>("apply_tags_to_files", {
      fileIds: ids,
      addTagIds,
      removeTagIds,
    });
    await notify(states);
  } catch (e) {
    error.value = String(e);
  } finally {
    busy.value = false;
  }
}
function remove(keyword: Keyword) {
  const ids = keywords.value
    .filter(
      (k) => k.id === keyword.id || k.name.startsWith(keyword.name + " > "),
    )
    .map((k) => k.id);
  void apply([], ids);
}
function toggle(keyword: Keyword) {
  if (keyword.selectedCount === props.fileIds.length) remove(keyword);
  else void apply([keyword.id], []);
}
async function addTyped() {
  if (busy.value || !props.fileIds.length) return;
  const names = parseKeywordInput(input.value);
  if (!names.length) return;
  busy.value = true;
  error.value = "";
  const ids = [...props.fileIds];
  try {
    const states = await invoke<any[]>("add_keywords", { fileIds: ids, names });
    input.value = "";
    await notify(states);
  } catch (e) {
    error.value = String(e);
  } finally {
    busy.value = false;
  }
}
async function setLabel(label: string) {
  if (busy.value || !props.fileIds.length) return;
  busy.value = true;
  error.value = "";
  try {
    await invoke("set_library_color_label", {
      fileIds: [...props.fileIds],
      label,
    });
    status.value = t("keywords.labelSaved");
    emit("label-changed");
  } catch (e) {
    error.value = String(e);
  } finally {
    busy.value = false;
  }
}
watch(
  () => [props.fileIds.join(","), props.revision],
  () => {
    status.value = "";
    request++;
    clearTimeout(refreshTimer);
    refreshTimer = setTimeout(() => {
      void refresh();
    }, 120);
  },
  { immediate: true },
);
onMounted(async () => {
  const unlisten = await listen("tags-changed", () => {
    void refresh();
  });
  if (disposed) unlisten();
  else stop = unlisten;
});
onBeforeUnmount(() => {
  disposed = true;
  request++;
  clearTimeout(refreshTimer);
  stop?.();
});
</script>

<style scoped>
.keyword-panel {
  display: flex;
  flex-direction: column;
  width: 300px;
  min-width: 260px;
  max-width: 34vw;
  border-left: 1px solid
    color-mix(in oklab, var(--color-base-content) 10%, transparent);
}
.keyword-section {
  padding: 16px;
  border-bottom: 1px solid
    color-mix(in oklab, var(--color-base-content) 8%, transparent);
}
.keyword-section h3 {
  font-size: 12px;
  font-weight: 650;
  margin-bottom: 12px;
}
.keyword-chip {
  font-size: 11px;
  max-width: 100%;
  overflow: hidden;
  text-overflow: ellipsis;
  padding: 3px 7px;
  border-radius: 3px;
  background: var(--color-base-100);
  cursor: pointer;
}
.keyword-suggestion {
  font-size: 11px;
  text-align: left;
  overflow: hidden;
  white-space: nowrap;
  text-overflow: ellipsis;
  padding: 6px 8px;
  background: var(--color-base-100);
  border-radius: 3px;
  cursor: pointer;
}
.keyword-suggestion:hover,
.keyword-chip:hover {
  color: var(--color-primary);
}
.keyword-row {
  display: flex;
  align-items: center;
  gap: 5px;
  min-height: 29px;
}
.keyword-row:hover {
  background: var(--color-base-100);
}
.keyword-arrow {
  width: 22px;
  font-size: 14px;
  opacity: 0.5;
  cursor: pointer;
}
.keyword-arrow:hover {
  opacity: 1;
  color: var(--color-primary);
}
.keyword-color {
  width: 15px;
  height: 15px;
  border-radius: 2px;
  cursor: pointer;
}
.keyword-color:disabled {
  opacity: 0.35;
  cursor: default;
}
</style>
