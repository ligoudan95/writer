<script setup lang="ts">
/** 跨文档全文搜索（FR-SEARCH-01，V1.1）：Ctrl+Shift+F 唤起；
 * ≥3 字 FTS5 trigram / ≤2 字 LIKE 回退（Rust 侧分流）；点击结果打开并定位 */
import { nextTick, ref } from "vue";
import { api, type SearchHit, type WebResult } from "../api";
import { s, openDoc } from "../store";
import { computed } from "vue";
import { ai } from "../aiflow";

const q = ref("");
const hits = ref<SearchHit[]>([]);
// 联网搜索（FR-AI-07，V1.2）：结果卡片不自动进入正文
const webMode = ref(false);
const webResults = ref<WebResult[] | null>(null);

async function askAiAbout(r: WebResult) {
  // 追问 AI 总结：以搜索结果为粘贴上下文打开讨论面板
  ai.chat.pasteText = `${r.title}
${r.url}
${r.snippet}`;
  ai.chat.contextMode = "paste";
  emit("close");
  ai.chatOpen = true;
}

function insertAsReference(list: WebResult[]) {
  const md = list.map((r) => `- [${r.title}](${r.url})`).join(String.fromCharCode(10));
  void import("../aiflow").then((m) => m.insertReplyToBody(md));
  emit("close");
}
const searching = ref(false);
const searched = ref(false);
const inputEl = ref<HTMLInputElement | null>(null);
const emit = defineEmits<{ (e: "close"): void }>();

// 搜索过滤（FR-SEARCH-02）：文件夹 / 标签 / 时间范围
const filterFolder = ref("");
const filterTag = ref("");
const filterDays = ref(0); // 0=不限 7/30/365
const folders = computed(() => {
  const byPath = (nodes: typeof s.tree): string[] => {
    const out: string[] = [];
    for (const n of nodes) {
      if (n.kind === "folder") {
        out.push(n.id);
        out.push(...byPath(n.children ?? []));
      }
    }
    return out;
  };
  return byPath(s.tree);
});
const allTags = computed(() => {
  const set = new Set<string>();
  const walk = (nodes: typeof s.tree) => {
    for (const n of nodes) {
      if (n.kind === "doc") n.tags?.forEach((t) => set.add(t));
      if (n.children) walk(n.children);
    }
  };
  walk(s.tree);
  return [...set].sort();
});
function activeFilter() {
  const f: import("../api").SearchFilter = {};
  if (filterFolder.value) f.folder = filterFolder.value;
  if (filterTag.value) f.tags = [filterTag.value];
  if (filterDays.value > 0) f.since = Math.floor(Date.now() / 1000) - filterDays.value * 86400;
  return Object.keys(f).length ? f : undefined;
}

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
    if (webMode.value) {
      webResults.value = await api.webSearch(query);
    } else {
      hits.value = await api.searchQuery(query, activeFilter());
    }
    searched.value = true;
  } catch (e) {
    webResults.value = null;
    s.toast = { msg: `联网搜索失败：${(e as { message?: string }).message ?? e}` };
    setTimeout(() => {
      if (s.toast?.msg.startsWith("联网搜索失败")) s.toast = null;
    }, 5000);
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

// 跨文档替换（FR-FND-02）：结果内替换，逐条确认；写经哈希护栏防覆盖他人修改
const replaceMode = ref(false);
const replaceWith = ref("");
const replaced = ref<Set<string>>(new Set());
const replacing = ref(false);

async function replaceOne(h: SearchHit) {
  if (replacing.value || replaced.value.has(h.path)) return;
  replacing.value = true;
  try {
    const d = await api.readDoc(h.path);
    const needle = q.value.trim().replace(/^"|"$/g, "");
    if (!d.body.includes(needle)) {
      // 内容已变（外部修改/过期索引）：跳过并提示
      s.toast = { msg: `跳过 ${h.title}：命中处已不存在（可能已被修改）` };
      setTimeout(() => (s.toast = null), 3500);
      replaced.value.add(h.path);
      return;
    }
    const body2 = d.body.split(needle).join(replaceWith.value);
    await api.writeDoc(h.path, body2, d.hash);
    replaced.value.add(h.path);
  } catch (e) {
    s.toast = { msg: `替换失败 ${h.title}：${(e as { message?: string }).message ?? e}` };
    setTimeout(() => (s.toast = null), 4500);
  } finally {
    replacing.value = false;
  }
}

function replaceAllPending() {
  // 逐条顺序替换（每条独立哈希护栏与失败提示——FND-02 逐条确认语义）
  for (const h of hits.value) {
    if (!replaced.value.has(h.path)) void replaceOne(h);
  }
}

function toggleReplaceMode() {
  replaceMode.value = !replaceMode.value;
  replaced.value = new Set();
  replaceWith.value = "";
}

function toggleWebMode() {
  webMode.value = !webMode.value;
  webResults.value = null;
  hits.value = [];
  searched.value = false;
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
        <button
          v-if="!webMode"
          class="icon-btn"
          :class="{ on: replaceMode }"
          :title="replaceMode ? '替换模式中（点击退出）' : '跨文档替换（逐条确认）'"
          :style="replaceMode ? 'color: var(--accent)' : ''"
          @click="toggleReplaceMode"
        >🔁</button>
        <button
          class="icon-btn"
          :class="{ on: webMode }"
          :title="webMode ? '联网搜索中（点击切回本地）' : '切换为联网搜索（需配置 Provider）'"
          :style="webMode ? 'color: var(--accent)' : ''"
          @click="toggleWebMode"
        >🌐</button>
        <button v-if="q" class="icon-btn" title="清空" @click="q = ''; hits = []; webResults = null; searched = false">✕</button>
      </div>
      <!-- 搜索过滤（FR-SEARCH-02） -->
      <div v-if="!webMode" class="gs-filters">
        <select v-model="filterFolder" @change="doSearch">
          <option value="">全库</option>
          <option v-for="f in folders" :key="f" :value="f">{{ f.replace("docs/", "") }}</option>
        </select>
        <select v-model="filterTag" @change="doSearch">
          <option value="">全部标签</option>
          <option v-for="t in allTags" :key="t" :value="t">#{{ t }}</option>
        </select>
        <select v-model="filterDays" @change="doSearch">
          <option :value="0">不限时间</option>
          <option :value="7">近 7 天</option>
          <option :value="30">近 30 天</option>
          <option :value="365">近一年</option>
        </select>
      </div>
      <div class="gs-results">
        <div v-if="searching" class="gs-empty">搜索中…</div>
        <div v-else-if="webMode && webResults === null" class="gs-empty">联网搜索：输入关键词查询互联网（需先在 设置 → AI 接口 配置搜索 Provider）</div>
        <div v-else-if="webMode && searched && !webResults?.length" class="gs-empty">没有找到「{{ q }}」的相关网页</div>
        <template v-else-if="webMode && webResults?.length">
          <div v-for="r in webResults" :key="r.url" class="gs-web">
            <div class="gs-title-row">
              <a :href="r.url" target="_blank" rel="noopener" class="gs-title">{{ r.title }}</a>
            </div>
            <div class="gs-snippet">{{ r.snippet }}</div>
            <div class="gs-web-ops">
              <button class="btn small ghost" @click="askAiAbout(r)">追问 AI 总结</button>
              <button class="btn small ghost" @click="insertAsReference([r])">插入为参考资料</button>
            </div>
          </div>
          <button class="btn small" @click="insertAsReference(webResults)">全部插入为参考资料</button>
        </template>
        <div v-else-if="searched && !hits.length" class="gs-empty">没有匹配「{{ q }}」的文档</div>
        <!-- 跨文档替换（FR-FND-02） -->
        <div v-if="replaceMode && hits.length" class="gs-replace-bar">
          <span class="dim">将「{{ q }}」替换为</span>
          <input v-model="replaceWith" placeholder="替换内容" @keydown.enter.prevent="replaceAllPending" />
          <button class="btn small primary" :disabled="replacing || !replaceWith" @click="replaceAllPending">
            逐条替换全部未处理项
          </button>
          <span class="dim">{{ replaced.size }}/{{ hits.length }} 已处理</span>
        </div>
        <button
          v-for="h in hits"
          :key="h.path"
          class="gs-hit"
          :class="{ done: replaced.has(h.path) }"
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
          <div v-if="replaceMode" class="gs-replace-ops">
            <template v-if="replaced.has(h.path)">
              <span class="dim">✓ 已替换</span>
            </template>
            <button v-else class="btn small" :disabled="replacing" @click.stop="replaceOne(h)">
              替换此篇
            </button>
          </div>
        </button>
      </div>
      <div class="gs-foot">Ctrl+Shift+F 唤起 · Esc 关闭 · 点击结果定位到正文</div>
    </div>
  </div>
</template>
