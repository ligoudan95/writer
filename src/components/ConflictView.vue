<script setup lang="ts">
import { onMounted, ref, watch } from "vue";
import { api } from "../api";
import { resolveConflict, s } from "../store";
import { marked } from "marked";

/** 冲突对比视图（FR-SYN-04）：左本地版（原文档），右冲突副本（远端版）；
 * 只读 + 三动作：保留此版 / 保留彼版 / 手动合并后清理 */
const localBody = ref("");
const remoteBody = ref("");
const active = ref<string | null>(null);
const visible = ref(true);

onMounted(() => {
  if (s.sync.conflicts.length) void open(s.sync.conflicts[0].copyPath);
});

watch(
  () => s.sync.conflicts.length,
  (n) => {
    if (n > 0 && !active.value) {
      void open(s.sync.conflicts[0].copyPath);
      visible.value = true;
    }
  },
);

async function open(copyPath: string) {
  active.value = copyPath;
  const conflict = s.sync.conflicts.find((c) => c.copyPath === copyPath);
  if (!conflict) return;
  const local = await api.readDoc(conflict.path);
  const remote = await api.readDoc(copyPath);
  localBody.value = local.body;
  remoteBody.value = remote.body;
}

/** 轻量只读预览：md → HTML（对比用途，无需编辑器实例） */
function renderPreview(body: string): string {
  return marked.parse(body, { async: false }) as string;
}

async function act(copyPath: string, keepRemote: boolean) {
  await resolveConflict(copyPath, keepRemote);
  active.value = null;
  if (s.sync.conflicts.length) await open(s.sync.conflicts[0].copyPath);
}

const localName = () => s.sync.conflicts.find((c) => c.copyPath === active.value)?.path ?? "本地版";
</script>

<template>
  <div v-if="visible && s.sync.conflicts.length" class="conflict-view">
    <div class="cv-head">
      <span class="cv-title">同步冲突（{{ s.sync.conflicts.length }} 个待处理）</span>
      <span style="flex: 1"></span>
      <button class="icon-btn" title="收起（冲突仍保留，可从状态栏再打开）" @click="visible = false">✕</button>
    </div>

    <div v-if="s.sync.conflicts.length > 1" class="cv-tabs">
      <button
        v-for="c in s.sync.conflicts"
        :key="c.copyPath"
        class="btn small ghost"
        :class="{ on: active === c.copyPath }"
        @click="open(c.copyPath)"
      >
        {{ c.path }}
      </button>
    </div>

    <div class="cv-panes">
      <div class="cv-pane">
        <div class="cv-pane-head">
          <span>本地版 · {{ localName() }}</span>
          <button class="btn small" :disabled="!active" @click="active && act(active, false)">保留此版</button>
        </div>
        <div class="cv-body prose" v-html="renderPreview(localBody)"></div>
      </div>
      <div class="cv-pane remote">
        <div class="cv-pane-head">
          <span>远程冲突副本（只读）</span>
          <button class="btn small" :disabled="!active" @click="active && act(active, true)">保留彼版</button>
        </div>
        <div class="cv-body prose" v-html="renderPreview(remoteBody)"></div>
      </div>
    </div>

    <div class="cv-foot">
      <button class="btn small ghost" :disabled="!active" @click="active && act(active, false)">
        手动合并后清理（编辑本地后删除副本）
      </button>
      <span class="cv-hint">保留此版 = 下次同步以本地覆盖远端；保留彼版 = 副本替换本地（先自动快照）</span>
    </div>
  </div>
</template>
