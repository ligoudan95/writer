<script setup lang="ts">
/** 跨文档全文搜索（FR-SEARCH-01，V1.1）：Ctrl+Shift+F 唤起；
 * ≥3 字 FTS5 trigram / ≤2 字 LIKE 回退（Rust 侧分流）；点击结果打开并定位 */
import { nextTick, ref } from "vue";
import { api, type SearchHit } from "../api";
import { s, openDoc } from "../store";

const q = ref("");
const hits = ref<SearchHit[]>([]);
const searching = ref(false);
const searched = ref(false);
const inputEl = ref<HTMLInputElement | null>(null);
const emit = defineEmits<{ (e: "close"): void }>();

let timer: number | undefined;
function onInput() {
  clearTimeout(timer);
  if (!q.value.trim()) {
    hits.value = [];
    searched.value = false;
    return;
  }
  timer = window.setTimeout(() => void doSearch(), 220);
}

async function doSearch() {
  const query = q.value.trim();
  if (!query) return;
  searching.value = true;
  try {
    hits.value = await api.searchQuery(query);
    searched.value = true;
  } finally {
    searching.value = false;
  }
}

async function jump(hit: SearchHit) {
  emit("close");
  await openDoc(hit.path);
  s.pendingLocate = q.value.trim(); // 编辑器内容应用后定位首个命中
}

/** 高亮：FTS snippet 的 ⟦⟧ 标记统一转为前端 mark（LIKE 片段按原文重找） */
function segs(snippet: string): { t: string; hit: boolean }[] {
  const query = q.value.trim().replace(/^"|"$/g, "");
  const out: { t: string; hit: boolean }[] = [];
  for (const raw of snippet.split("⟦")) {
    if (!raw) continue;
    const i = raw.indexOf("⟧");
    if (i >= 0) {
      out.push({ t: raw.slice(0, i), hit: true });
      const rest = raw.slice(i + 1);
      if (rest) out.push(...markQuery(rest, query));
    } else {
      out.push(...markQuery(raw, query));
    }
  }
  return out;
}

function markQuery(text: string, query: string): { t: string; hit: boolean }[] {
  if (!query) return [{ t: text, hit: false }];
  const lower = text.toLowerCase();
  const lq = query.toLowerCase();
  const out: { t: string; hit: boolean }[] = [];
  let start = 0;
  while (true) {
    const i = lower.indexOf(lq, start);
    if (i < 0) break;
    if (i > start) out.push({ t: text.slice(start, i), hit: false });
    out.push({ t: text.slice(i, i + query.length), hit: true });
    start = i + query.length;
  }
  if (start < text.length) out.push({ t: text.slice(start), hit: false });
  return out;
}

function fmtPath(p: string) {
  return p.replace(/^docs\//, "").replace(/\.md$/, "");
}

nextTick(() => inputEl.value?.focus());
</script>

<template>
  <div class="modal-mask transparent" @click.self="emit('close')">
    <div class="gs-panel">
      <div class="gs-bar">
        <span class="gs-icon">🔍</span>
        <input
          ref="inputEl"
          v-model="q"
          placeholder="搜索全库文档（支持“引号精确短语”，两字中文可用）"
          @input="onInput"
          @keydown.enter.prevent="doSearch"
          @keydown.esc="emit('close')"
        />
        <button v-if="q" class="icon-btn" title="清空" @click="q = ''; hits = []; searched = false">✕</button>
      </div>
      <div class="gs-results">
        <div v-if="searching" class="gs-empty">搜索中…</div>
        <div v-else-if="searched && !hits.length" class="gs-empty">没有匹配「{{ q }}」的文档</div>
        <button
          v-for="h in hits"
          :key="h.path"
          class="gs-hit"
          @click="jump(h)"
        >
          <div class="gs-title-row">
            <span class="gs-title">{{ h.title }}</span>
            <span class="gs-path">{{ fmtPath(h.path) }}</span>
          </div>
          <div class="gs-snippet">
            <template v-for="(sg, i) in segs(h.snippet)" :key="i">
              <mark v-if="sg.hit">{{ sg.t }}</mark>
              <template v-else>{{ sg.t }}</template>
            </template>
          </div>
        </button>
      </div>
      <div class="gs-foot">Ctrl+Shift+F 唤起 · Esc 关闭 · 点击结果定位到正文</div>
    </div>
  </div>
</template>
