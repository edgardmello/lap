<template>
  <div ref="root" class="relative shrink-0" :class="widthClass">
    <input
      ref="input"
      class="input input-bordered input-sm w-full pr-7"
      type="text"
      autocomplete="off"
      spellcheck="false"
      role="combobox"
      :aria-expanded="open"
      :placeholder="placeholder"
      :value="modelValue"
      @input="onInput"
      @change="emit('change')"
      @focus="openMenu"
      @click="openMenu"
      @blur="closeMenu"
      @keydown="onKeydown"
    />
    <button
      type="button"
      tabindex="-1"
      class="absolute right-1 top-1/2 -translate-y-1/2 w-5 h-5 flex items-center justify-center text-base-content/50 hover:text-base-content cursor-pointer"
      :aria-label="labels.show"
      @mousedown.prevent="toggleMenu"
    >
      <IconArrowDown class="w-3 h-3" />
    </button>

    <teleport to="body">
      <div
        v-if="open"
        class="fixed z-500 mt-1 max-h-60 overflow-y-auto rounded-box border border-base-content/30 bg-base-200/90 backdrop-blur-md shadow-lg text-sm text-base-content/70 p-1"
        :style="menuStyle"
        role="listbox"
      >
        <div v-if="loading && !models.length" class="px-2 py-1 text-base-content/50">{{ labels.loading }}</div>
        <div v-else-if="error" class="px-2 py-1 text-error whitespace-normal">{{ labels.failed }}: {{ error }}</div>
        <div v-else-if="!filtered.length" class="px-2 py-1 text-base-content/50">{{ models.length ? labels.noMatch : labels.empty }}</div>
        <button
          v-for="(model, index) in filtered"
          :key="model"
          type="button"
          role="option"
          :aria-selected="model === modelValue"
          class="w-full text-left px-2 py-1 rounded-box truncate cursor-pointer"
          :class="[
            index === highlighted ? 'bg-base-100/40 text-base-content' : 'hover:bg-base-100/30 hover:text-base-content',
            model === modelValue ? 'text-primary' : '',
          ]"
          :title="model"
          @mousedown.prevent="select(model)"
          @mouseenter="highlighted = index"
        >{{ model }}</button>
      </div>
    </teleport>
  </div>
</template>

<script setup>
// Text field with a list of the models a server serves. The list is fetched
// each time it opens, and any name can still be typed, since not every
// server lists all of its models.
import { computed, onBeforeUnmount, ref } from 'vue';
import { IconArrowDown } from '@/common/icons';

const props = defineProps({
  modelValue: { type: String, default: '' },
  placeholder: { type: String, default: '' },
  // async () => string[]; throws with the reason when the server can't list
  loadModels: { type: Function, required: true },
  labels: { type: Object, default: () => ({}) },
  widthClass: { type: String, default: 'w-56' },
});
const emit = defineEmits(['update:modelValue', 'change']);

const root = ref(null);
const input = ref(null);
const open = ref(false);
const loading = ref(false);
const error = ref('');
const models = ref([]);
const query = ref('');
const highlighted = ref(-1);
const menuStyle = ref({});
let loadToken = 0;

// Show every model on open; narrow the list only once the user types.
const filtered = computed(() => {
  const text = query.value.trim().toLowerCase();
  return text ? models.value.filter(model => model.toLowerCase().includes(text)) : models.value;
});

const placeMenu = () => {
  const rect = root.value?.getBoundingClientRect();
  if (!rect) return;
  const width = Math.max(rect.width, 224);
  const left = Math.max(8, Math.min(rect.right - width, window.innerWidth - width - 8));
  menuStyle.value = { top: `${rect.bottom}px`, left: `${left}px`, width: `${width}px` };
};

const load = async () => {
  const token = ++loadToken;
  loading.value = true;
  error.value = '';
  try {
    const list = await props.loadModels();
    if (token !== loadToken) return;
    models.value = Array.isArray(list) ? list : [];
  } catch (e) {
    if (token !== loadToken) return;
    models.value = [];
    error.value = e?.message || String(e);
  } finally {
    if (token === loadToken) loading.value = false;
  }
};

const openMenu = () => {
  if (open.value) return;
  query.value = '';
  highlighted.value = -1;
  placeMenu();
  open.value = true;
  window.addEventListener('resize', placeMenu);
  window.addEventListener('scroll', placeMenu, true);
  void load();
};

const closeMenu = () => {
  if (!open.value) return;
  open.value = false;
  loadToken++;
  loading.value = false;
  window.removeEventListener('resize', placeMenu);
  window.removeEventListener('scroll', placeMenu, true);
};

const toggleMenu = () => {
  if (open.value) {
    closeMenu();
  } else {
    input.value?.focus();
    openMenu();
  }
};

const onInput = (event) => {
  const value = event.target.value;
  emit('update:modelValue', value);
  query.value = value;
  highlighted.value = -1;
  if (!open.value) openMenu();
};

const select = (model) => {
  const changed = model !== props.modelValue;
  emit('update:modelValue', model);
  closeMenu();
  if (changed) emit('change');
};

const onKeydown = (event) => {
  if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
    event.preventDefault();
    if (!open.value) openMenu();
    const count = filtered.value.length;
    if (!count) return;
    const step = event.key === 'ArrowDown' ? 1 : -1;
    highlighted.value = (highlighted.value + step + count) % count;
  } else if (event.key === 'Enter' && open.value && highlighted.value >= 0 && filtered.value[highlighted.value]) {
    event.preventDefault();
    select(filtered.value[highlighted.value]);
  } else if (event.key === 'Escape' && open.value) {
    event.preventDefault();
    event.stopPropagation();
    closeMenu();
  }
};

onBeforeUnmount(closeMenu);
</script>
