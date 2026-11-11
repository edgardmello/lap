<template>
  <section
    ref="root"
    class="library-filter bg-base-200 text-base-content border-b border-base-content/10"
    :aria-label="$t('libraryFilter.title')"
    @keydown.stop
  >
    <header class="flex items-center gap-1 px-3 py-2 text-xs">
      <span class="font-semibold mr-auto"
        >{{ $t("libraryFilter.title") }}
        <span v-if="active" class="text-primary">({{ active }})</span></span
      >
      <button
        v-for="section in ['text', 'attribute', 'metadata']"
        :key="section"
        type="button"
        class="btn btn-xs btn-ghost"
        :class="{ 'bg-base-100 text-primary': sections.includes(section) }"
        :aria-pressed="sections.includes(section)"
        @click="toggleSection(section)"
      >
        {{ $t(`libraryFilter.${section}`) }}
      </button>
      <button class="btn btn-xs btn-ghost" @click="reset">
        {{ $t("libraryFilter.none") }}
      </button>
      <button
        class="btn btn-xs btn-ghost ml-2"
        :aria-label="$t('libraryFilter.close')"
        @click="$emit('close')"
      >
        ×
      </button>
    </header>
    <p v-if="!supported" class="px-3 pb-3 text-xs opacity-60">
      {{ $t("libraryFilter.queryOnly") }}
    </p>
    <template v-else>
      <div
        v-if="sections.includes('text')"
        class="flex gap-2 px-3 pb-3 flex-wrap"
      >
        <select
          class="select select-xs"
          :aria-label="$t('libraryFilter.field')"
          :value="modelValue.field"
          @change="set('field', ($event.target as HTMLSelectElement).value)"
        >
          <option
            v-for="field in ['all', 'filename', 'keywords', 'description']"
            :key="field"
            :value="field"
          >
            {{ $t(`libraryFilter.fields.${field}`) }}
          </option>
        </select>
        <select
          class="select select-xs"
          :aria-label="$t('libraryFilter.match')"
          :value="modelValue.mode"
          @change="set('mode', ($event.target as HTMLSelectElement).value)"
        >
          <option
            v-for="mode in ['all', 'any', 'contains', 'not', 'starts', 'ends']"
            :key="mode"
            :value="mode"
          >
            {{ $t(`libraryFilter.modes.${mode}`) }}
          </option>
        </select>
        <input
          v-model="draft"
          type="search"
          class="input input-sm flex-1 min-w-40"
          :placeholder="$t('libraryFilter.search')"
          :aria-label="$t('libraryFilter.search')"
        />
      </div>
      <div
        v-if="sections.includes('attribute')"
        class="flex items-center flex-wrap gap-3 px-3 pb-3 text-xs"
      >
        <label class="flex items-center gap-2"
          ><span>{{ $t("libraryFilter.rating") }}</span>
          <select
            class="select select-xs"
            :value="modelValue.ratingMin ?? ''"
            @change="set('ratingMin', nullableNumber($event))"
          >
            <option value="">{{ $t("libraryFilter.all") }}</option>
            <option v-for="n in 5" :key="n" :value="n">
              {{ "★".repeat(n) }} ≥
            </option>
          </select>
        </label>
        <label class="flex items-center gap-2"
          ><span>{{ $t("libraryFilter.flag") }}</span>
          <select
            class="select select-xs"
            :value="modelValue.flag ?? ''"
            @change="set('flag', nullableNumber($event))"
          >
            <option value="">{{ $t("libraryFilter.all") }}</option>
            <option
              v-for="(flag, i) in ['unreviewed', 'picked', 'rejected']"
              :key="flag"
              :value="i"
            >
              {{ $t(`libraryFilter.${flag}`) }}
            </option>
          </select>
        </label>
        <label class="flex items-center gap-2"
          ><input
            type="checkbox"
            class="checkbox checkbox-xs"
            :checked="modelValue.favorite"
            @change="set('favorite', !modelValue.favorite)"
          />{{ $t("libraryFilter.favorite") }}</label
        >
        <button
          v-for="label in colors"
          :key="label"
          class="filter-color"
          :class="{ chosen: modelValue.labels.includes(label) }"
          :style="{ background: colorValues[label] }"
          :aria-label="$t(`libraryFilter.colors.${label}`)"
          :aria-pressed="modelValue.labels.includes(label)"
          @click="choose('labels', label, $event)"
        ></button>
      </div>
      <p v-if="error" class="px-3 pb-2 text-xs text-error" role="alert">
        {{ error }}
      </p>
      <div
        v-if="sections.includes('metadata')"
        class="filter-columns"
        :aria-busy="loading"
      >
        <section
          v-for="dimension in dimensions"
          :key="dimension"
          class="filter-column"
        >
          <h3
            class="px-3 py-2 font-semibold text-xs border-y border-base-content/10"
          >
            {{ $t(`libraryFilter.dimensions.${dimension}`) }}
          </h3>
          <div class="filter-values">
            <button
              class="facet-row"
              :class="{
                chosen:
                  !modelValue[dimension].length &&
                  !(dimension === 'keywordIds' && modelValue.withoutKeywords),
              }"
              @click="clearDimension(dimension)"
            >
              <span>{{ $t("libraryFilter.all") }}</span
              ><span>{{ totals[dimension]?.toLocaleString() }}</span>
            </button>
            <button
              v-for="option in facets[dimension] || []"
              :key="option.value"
              class="facet-row"
              :class="{
                chosen:
                  dimension === 'keywordIds'
                    ? option.value
                      ? modelValue.keywordIds.includes(Number(option.value))
                      : modelValue.withoutKeywords
                    : modelValue[dimension].includes(option.value),
              }"
              :title="labelFor(dimension, option.value)"
              :aria-pressed="
                dimension === 'keywordIds'
                  ? option.value
                    ? modelValue.keywordIds.includes(Number(option.value))
                    : modelValue.withoutKeywords
                  : modelValue[dimension].includes(option.value)
              "
              @click="choose(dimension, option.value, $event)"
            >
              <span class="truncate">{{
                labelFor(dimension, option.value)
              }}</span
              ><span>{{ option.count.toLocaleString() }}</span>
            </button>
          </div>
        </section>
      </div>
      <p
        v-if="sections.includes('metadata')"
        class="text-[11px] opacity-50 px-3 py-1"
      >
        {{ $t("libraryFilter.combineHint") }}
      </p>
    </template>
  </section>
</template>

<script setup lang="ts">
import { ref, watch, computed, onMounted, onBeforeUnmount } from "vue";
import { useI18n } from "vue-i18n";
import { invoke } from "@tauri-apps/api/core";
import {
  emptyLibraryFilter,
  activeLibraryFilterCount,
  toggleFacet,
  type LibraryFilter,
} from "@/common/libraryFilter";
const props = defineProps<{
  modelValue: LibraryFilter;
  query: any;
  smartQuery?: any;
  collectionId?: number | null;
  supported: boolean;
  revision: number;
}>();
const emit = defineEmits(["update:modelValue", "height", "close"]);
const { t } = useI18n();
const root = ref<HTMLElement>();
const sections = ref(["text"]);
const draft = ref(props.modelValue.text);
const dimensions = [
  "years",
  "cameras",
  "lenses",
  "labels",
  "keywordIds",
  "fileTypes",
] as const;
const colors = ["red", "yellow", "green", "blue", "purple"];
const colorValues: Record<string, string> = {
  red: "#d96463",
  yellow: "#d2ba62",
  green: "#69a07a",
  blue: "#668fb7",
  purple: "#a184bc",
};
const facets = ref<Record<string, { value: string; count: number }[]>>({});
const keywords = ref<Record<string, string>>({});
const totals = ref<Record<string, number>>({});
const loading = ref(false),
  error = ref("");
let request = 0,
  timer: ReturnType<typeof setTimeout>,
  filterTimer: ReturnType<typeof setTimeout>,
  observer: ResizeObserver;
const active = computed(() => activeLibraryFilterCount(props.modelValue));
function set(key: keyof LibraryFilter, value: any) {
  emit("update:modelValue", { ...props.modelValue, [key]: value });
}
function nullableNumber(event: Event) {
  const v = (event.target as HTMLSelectElement).value;
  return v === "" ? null : Number(v);
}
function toggleSection(section: string) {
  sections.value = sections.value.includes(section)
    ? sections.value.filter((s) => s !== section)
    : [...sections.value, section];
}
function reset() {
  clearTimeout(filterTimer);
  draft.value = "";
  emit("update:modelValue", emptyLibraryFilter());
}
function clearDimension(dimension: string) {
  emit("update:modelValue", {
    ...props.modelValue,
    [dimension]: [],
    ...(dimension === "keywordIds" ? { withoutKeywords: false } : {}),
  });
}
function choose(dimension: string, value: string, event: MouseEvent) {
  if (dimension === "keywordIds" && value === "") {
    emit("update:modelValue", {
      ...props.modelValue,
      keywordIds: [],
      withoutKeywords: !props.modelValue.withoutKeywords,
    });
    return;
  }
  const next = toggleFacet(
    props.modelValue[dimension],
    dimension === "keywordIds" ? Number(value) : value,
    event.metaKey || event.ctrlKey || event.shiftKey,
  );
  emit("update:modelValue", {
    ...props.modelValue,
    [dimension]: next,
    ...(dimension === "keywordIds" ? { withoutKeywords: false } : {}),
  });
}
function labelFor(dimension: string, value: string) {
  if (value === "")
    return t(
      dimension === "keywordIds"
        ? "libraryFilter.withoutKeywords"
        : "libraryFilter.unknown",
    );
  if (dimension === "keywordIds") return keywords.value[value] || value;
  if (dimension === "labels") return t(`libraryFilter.colors.${value}`);
  return value;
}
watch(draft, (value) => {
  clearTimeout(filterTimer);
  filterTimer = setTimeout(() => {
    if (value !== props.modelValue.text) set("text", value);
  }, 250);
});
watch(
  () => props.modelValue.text,
  (value) => {
    if (value !== draft.value) draft.value = value;
  },
);
watch(
  () => [
    props.query,
    props.smartQuery,
    props.collectionId,
    props.supported,
    props.revision,
    sections.value.includes("metadata"),
  ],
  () => {
    clearTimeout(timer);
    const id = ++request;
    if (!props.supported || !sections.value.includes("metadata")) return;
    timer = setTimeout(async () => {
      loading.value = true;
      error.value = "";
      try {
        const [values, panel] = await Promise.all([
          invoke<
            Record<
              string,
              { values: { value: string; count: number }[]; total: number }
            >
          >("get_library_facets", {
            params: props.query,
            smartParams: props.smartQuery || null,
            collectionId: props.collectionId || null,
          }),
          invoke<{ keywords: { id: number; name: string }[] }>(
            "get_keyword_panel",
            { fileIds: [] },
          ),
        ]);
        if (id === request) {
          facets.value = Object.fromEntries(
            Object.entries(values).map(([key, column]) => [key, column.values]),
          );
          totals.value = Object.fromEntries(
            Object.entries(values).map(([key, column]) => [key, column.total]),
          );
          keywords.value = Object.fromEntries(
            panel.keywords.map((k) => [k.id, k.name]),
          );
        }
      } catch (e) {
        if (id === request) error.value = String(e);
      } finally {
        if (id === request) loading.value = false;
      }
    }, 120);
  },
  { deep: true, immediate: true },
);
onMounted(() => {
  observer = new ResizeObserver((entries) =>
    emit("height", entries[0].contentRect.height),
  );
  if (root.value) observer.observe(root.value);
});
onBeforeUnmount(() => {
  request++;
  clearTimeout(timer);
  clearTimeout(filterTimer);
  observer?.disconnect();
  emit("height", 0);
});
</script>

<style scoped>
.filter-columns {
  display: grid;
  grid-template-columns: repeat(6, minmax(140px, 1fr));
  overflow-x: auto;
  border-top: 1px solid
    color-mix(in oklab, var(--color-base-content) 8%, transparent);
}
.filter-column {
  min-width: 0;
  border-right: 1px solid
    color-mix(in oklab, var(--color-base-content) 10%, transparent);
}
.filter-values {
  height: 170px;
  overflow: auto;
  padding: 4px;
}
.facet-row {
  display: flex;
  justify-content: space-between;
  gap: 10px;
  width: 100%;
  text-align: left;
  padding: 4px 8px;
  font-size: 12px;
  border-radius: 3px;
  cursor: pointer;
}
.facet-row:hover {
  background: var(--color-base-100);
}
.facet-row.chosen {
  background: color-mix(in oklab, var(--color-primary) 18%, transparent);
  color: var(--color-base-content);
}
.facet-row > span:last-child {
  opacity: 0.55;
  font-variant-numeric: tabular-nums;
  flex-shrink: 0;
}
.filter-color {
  width: 17px;
  height: 17px;
  border: 2px solid transparent;
  border-radius: 2px;
  cursor: pointer;
}
.filter-color.chosen {
  outline: 2px solid var(--color-primary);
  outline-offset: 2px;
}
</style>
