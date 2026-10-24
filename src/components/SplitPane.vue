<script setup lang="ts">
/** 分屏对照（FR-EDT-08 P2，V1.3）：右侧只读渲染另一篇文档。
 * 内容经 mdToDoc → exportHtml（schema 白名单消毒后的自有产物），不含脚本 */
import { ref, watch } from "vue";
import { api } from "../api";
import { s, openDoc, tabTitle } from "../store";
import { mdToDoc } from "../codec";
import { exportHtml } from "../exporter";

const html = ref("");
const title = ref("");
const path = ref("");

async function load() {
  const p = s.splitDoc;
  if (!p) return;
  path.value = p;
  title.value = tabTitle(p);
  const d = await api.readDoc(p);
  html.value = exportHtml(mdToDoc(d.body) as never);
}

watch(() => s.splitDoc, () => void load());
watch(() => s.reloadNonce, () => {
  if (s.splitDoc && s.current?.path === s.splitDoc) void load();
});
void load();

function close() {
  s.splitDoc = null;
}
</script>

<template>
  <aside class="splitpane">
    <div class="sp2-head">
      <span class="sp2-title" :title="path">对照：{{ title }}</span>
      <span style="flex: 1"></span>
      <button class="btn small ghost" title="在主编辑区打开（可编辑）" @click="openDoc(path)">编辑</button>
      <button class="icon-btn" title="关闭对照" @click="close">✕</button>
    </div>
    <div class="sp2-body pm-body" v-html="html"></div>
  </aside>
</template>
