<script setup lang="ts">
/** 回收站面板（FR-DOC-03）：浏览 / 还原 / 彻底删除 / 清空；30 天提示 */
import { onMounted, ref } from "vue";
import { confirm } from "@tauri-apps/plugin-dialog";
import { api, type TrashEntry } from "../api";
import { s, refreshTree } from "../store";

const items = ref<TrashEntry[]>([]);
const loading = ref(false);

async function reload() {
  loading.value = true;
  try {
    items.value = await api.trashList();
  } finally {
    loading.value = false;
  }
}
onMounted(reload);

const overCount = () => items.value.filter((i) => i.over30d).length;

async function restore(it: TrashEntry) {
  // 优先回原位置（删除时记录）；记录缺失时回库根（FR-DOC-03 AC）
  await api.restoreFromTrash(it.name, it.origRel || `docs/${it.origName}`);
  await reload();
  await refreshTree();
  s.toast = { msg: `已还原：${it.origName}` };
  setTimeout(() => (s.toast = null), 3000);
}

async function remove(it: TrashEntry) {
  const ok = await confirm(`彻底删除「${it.origName}」？此操作不可恢复。`, { title: "彻底删除" });
  if (!ok) return;
  await api.trashDelete(it.name);
  await reload();
}

async function empty() {
  const ok = await confirm(`清空回收站（${items.value.length} 项）？此操作不可恢复。`, { title: "清空回收站" });
  if (!ok) return;
  const n = await api.trashEmpty();
  await reload();
  s.toast = { msg: `已清空 ${n} 项` };
  setTimeout(() => (s.toast = null), 3000);
}

const fmtSize = (n: number) => (n > 1024 ? `${Math.ceil(n / 1024)} KB` : `${n} B`);
const fmtTime = (t: number) => {
  if (!t) return "";
  const d = new Date(t * 1000);
  return `${d.getFullYear()}-${d.getMonth() + 1}-${d.getDate()}`;
};

const emit = defineEmits<{ (e: "close"): void }>();
</script>

<template>
  <div class="modal-mask" @click.self="emit('close')">
    <div class="trash-panel">
      <div class="nc-head">
        <span>回收站</span>
        <span class="muted" style="font-size: 12px">移入 .writer/trash（不同步）</span>
        <span style="flex: 1"></span>
        <button class="btn small ghost danger" :disabled="!items.length" @click="empty">清空</button>
        <button class="icon-btn" @click="emit('close')">✕</button>
      </div>
      <div v-if="overCount()" class="trash-warn">
        {{ overCount() }} 项已保留超过 30 天，建议清理以释放空间（不会自动删除）
      </div>
      <div class="nc-list">
        <div v-for="it in items" :key="it.name" class="nc-item">
          <span class="nc-kind">{{ it.isDir ? "📁" : "📄" }}</span>
          <span class="nc-msg">
            {{ it.origName }}
            <span v-if="it.over30d" class="badge-warn">30天+</span>
          </span>
          <span class="nc-time">{{ fmtTime(it.deletedAt) }}<template v-if="!it.isDir"> · {{ fmtSize(it.size) }}</template></span>
          <button class="btn small" @click="restore(it)">还原</button>
          <button class="btn small ghost danger" @click="remove(it)">彻底删除</button>
        </div>
        <div v-if="!items.length && !loading" class="nc-empty">回收站为空</div>
      </div>
    </div>
  </div>
</template>
