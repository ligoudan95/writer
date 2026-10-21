<script setup lang="ts">
/** 状态栏：统计（含阅读时长/选区，FR-EDT-07）+ 同步指示 + 通知中心入口 + 主题/帮助 */
import { openSyncHistory, s } from "../store";
import { theme, setTheme } from "../theme";
import { notifyCenter, openCenter } from "../notify";

defineEmits<{ (e: "open-settings"): void; (e: "open-trash"): void; (e: "open-help"): void }>();

// -- 统计（FR-EDT-07：400 字/分钟阅读时长；选区统计由 EditorPane 写入 s.counts.sel*） ---
const readMin = () => Math.max(1, Math.round(s.counts.words / 400));

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

function cycleTheme() {
  setTheme(theme.mode === "system" ? "light" : theme.mode === "light" ? "dark" : "system");
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
    <span class="sb-item dim" title="按 400 字/分钟估算">≈{{ readMin() }} 分钟</span>
    <span v-if="s.counts.selWords" class="sb-item sel" title="选区统计">
      选区 {{ s.counts.selWords }} 字 / {{ s.counts.selChars }} 字符
    </span>

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

    <!-- 通知中心（FR-UI-06）：未读角标 -->
    <button class="icon-btn" title="通知中心" @click="openCenter()">
      🔔<span v-if="notifyCenter.unread" class="nc-badge">{{ notifyCenter.unread }}</span>
    </button>
    <button
      class="icon-btn"
      :title="`主题：${theme.mode === 'light' ? '亮色' : theme.mode === 'dark' ? '暗色' : '跟随系统'}（点击切换）`"
      @click="cycleTheme()"
    >
      {{ theme.systemDarkNow ? "🌙" : "☀" }}
    </button>
    <button class="icon-btn" title="帮助与快捷键（F1）" @click="$emit('open-help')">?</button>
  </footer>
</template>
