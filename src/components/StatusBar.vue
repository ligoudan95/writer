<script setup lang="ts">
import { ref } from "vue";
import { openSyncHistory, s } from "../store";

type ThemeMode = "light" | "dark" | "system";
const mode = ref<ThemeMode>((localStorage.getItem("writer.theme") as ThemeMode) || "system");

function applyTheme() {
  const dark =
    mode.value === "dark" ||
    (mode.value === "system" && window.matchMedia("(prefers-color-scheme: dark)").matches);
  document.documentElement.dataset.theme = dark ? "dark" : "light";
}
applyTheme();
window.matchMedia("(prefers-color-scheme: dark)").addEventListener("change", applyTheme);

function cycle() {
  mode.value = mode.value === "system" ? "light" : mode.value === "light" ? "dark" : "system";
  localStorage.setItem("writer.theme", mode.value);
  applyTheme();
}

// -- 同步指示（FR-SYN-03 状态可视化） ----------------------------------------
const PHASE_LABEL: Record<string, string> = {
  scanning: "扫描中…",
  planning: "规划中…",
  executing: "同步中…",
  settling: "结算中…",
  idle: "",
};

const phaseText = () => PHASE_LABEL[s.sync.phase] ?? (s.sync.running ? "同步中…" : "");

const lastText = () => {
  const r = s.sync.last;
  if (!r) return "未同步";
  const parts: string[] = [];
  if (r.uploaded) parts.push(`↑${r.uploaded}`);
  if (r.downloaded) parts.push(`↓${r.downloaded}`);
  if (r.moved) parts.push(`→${r.moved}`);
  if (r.trashedLocal) parts.push(`删${r.trashedLocal}`);
  if (!parts.length) return "已同步";
  return parts.join(" ");
};

function toggleHistory() {
  s.sync.historyOpen = !s.sync.historyOpen;
  if (s.sync.historyOpen) void openSyncHistory();
}

function fmtTime(unix: number) {
  return new Date(unix * 1000).toTimeString().slice(0, 5);
}

const ACTION_LABEL: Record<string, string> = {
  put: "上传",
  get: "下载",
  move: "改名",
  delete: "删除",
  trash: "回收站",
  conflict: "冲突",
};
</script>

<template>
  <footer class="statusbar">
    <span class="sb-item">{{ s.counts.words }} 字</span>
    <span class="sb-item">{{ s.counts.chars }} 字符</span>
    <span class="sb-item">{{ s.counts.paragraphs }} 段</span>

    <!-- 同步状态（点击展开历史/入口） -->
    <span
      class="sb-item sync-item"
      :class="{ busy: s.sync.running, warn: s.sync.conflicts.length > 0 }"
      :title="s.sync.conflicts.length ? `${s.sync.conflicts.length} 个冲突待处理` : '云同步'"
      @click="toggleHistory"
    >
      <span class="sync-dot">{{ s.sync.conflicts.length ? "⚠" : s.sync.running ? "⟳" : "☁" }}</span>
      <span v-if="phaseText()">{{ phaseText() }}</span>
      <span v-else class="dim">{{ lastText() }}</span>
    </span>

    <!-- 同步历史面板（含传输量，FR-SYN-03） -->
    <div v-if="s.sync.historyOpen" class="sync-popover">
      <div class="sp-head">
        <span>同步历史</span>
        <span style="flex: 1"></span>
        <button class="btn small ghost" @click="s.sync.settingsOpen = true">设置</button>
        <button class="icon-btn" @click="s.sync.historyOpen = false">✕</button>
      </div>
      <div class="sp-list">
        <div v-for="(row, i) in s.sync.history" :key="i" class="sp-row">
          <span class="sp-time">{{ fmtTime(row.time) }}</span>
          <span class="sp-file" :title="row.file">{{ row.file }}</span>
          <span class="sp-action">{{ ACTION_LABEL[row.action] ?? row.action }}·{{ row.direction === "up" ? "↑" : "↓" }}</span>
          <span class="sp-bytes">{{ row.bytes ? `${(row.bytes / 1024).toFixed(1)}K` : "" }}</span>
          <span class="sp-result" :class="{ bad: row.result !== 'ok' }">{{ row.result }}</span>
        </div>
        <div v-if="!s.sync.history.length" class="sp-empty">暂无同步记录</div>
      </div>
    </div>

    <span class="sb-grow" />
    <span v-if="s.root" class="sb-item dim" :title="s.root">{{ s.root }}</span>
    <button class="icon-btn" :title="`主题：${mode}（点击切换）`" @click="cycle()">
      {{ mode === "dark" ? "🌙" : mode === "light" ? "☀" : "◐" }}
    </button>
  </footer>
</template>
