<script setup lang="ts">
import { reactive, ref, watch, computed, onMounted, onBeforeUnmount } from "vue";
import { Editor, EditorContent } from "@tiptap/vue-3";
import StarterKit from "@tiptap/starter-kit";
import { Table, TableRow, TableHeader, TableCell } from "@tiptap/extension-table";
import TaskList from "@tiptap/extension-task-list";
import TaskItem from "@tiptap/extension-task-item";
import {
  s,
  scheduleSave,
  renameDoc,
  createDoc,
  openDoc,
  bindEditor,
  updateCounts,
  openHistory,
  reloadHistory,
  manualSnapshot,
  previewSnapshot,
  restoreSnapshot,
} from "../store";
import { mdToDoc, type PMNode } from "../codec";
import ExportPanel from "./ExportPanel.vue";

type HeadingLevel = 1 | 2 | 3 | 4 | 5 | 6;
const LEVELS: HeadingLevel[] = [1, 2, 3, 4, 5, 6];

// ---------------------------------------------------------------------------
// 编辑器实例（setup 作用域创建；文档切换只换内容，实例常驻）
// ---------------------------------------------------------------------------

const editor = new Editor({
  content: { type: "doc", content: [{ type: "paragraph" }] },
  extensions: [
    StarterKit.configure({ link: { openOnClick: false, autolink: true } }),
    TaskList,
    TaskItem.configure({ nested: true }),
    Table.configure({ resizable: false }),
    TableRow,
    TableHeader,
    TableCell,
  ],
  editorProps: {
    handleKeyDown(_view, event) {
      // Tab = 插入 2 字符缩进（中文正文用全角空格，代码块用半角）；Shift+Tab 移除行首缩进
      // 列表内保留 TipTap 默认的层级进出行为（2026-10-19 用户定案：取消自动缩进）
      if (event.key === "Tab" && !event.ctrlKey && !event.metaKey && !event.altKey) {
        if (editor.isActive("listItem") || editor.isActive("taskItem")) return false;
        event.preventDefault();
        if (event.shiftKey) {
          removeLineIndent();
        } else {
          const unit = editor.isActive("codeBlock") ? "  " : "　　";
          editor.chain().focus().insertContent(unit).run();
        }
        return true;
      }
      if ((event.ctrlKey || event.metaKey) && /^[1-6]$/.test(event.key)) {
        editor
          .chain()
          .focus()
          .toggleHeading({ level: Number(event.key) as HeadingLevel })
          .run();
        return true;
      }
      return false;
    },
  },
  onUpdate: () => {
    scheduleSave();
    scheduleCount();
  },
});

onMounted(() => {
  bindEditor(() => editor.getJSON() as unknown as PMNode);
  if (s.current) applyContent();
  else updateCounts({ type: "doc", content: [] });
  editor.on("transaction", syncToolbarState);
});

onBeforeUnmount(() => {
  editor.off("transaction", syncToolbarState);
  editor.destroy();
});

function applyContent() {
  if (!s.current) return;
  editor.commands.setContent(mdToDoc(s.current.body) as never, { emitUpdate: false });
  updateCounts(editor.getJSON() as unknown as PMNode);
}

/** Shift+Tab：移除当前段行首的缩进（全角空格/半角空格各按字符计） */
function removeLineIndent() {
  const { state } = editor;
  const { $from } = state.selection;
  const start = $from.start();
  const text = $from.parent.textContent;
  let cut = 0;
  if (text.startsWith("　　") || text.startsWith("　") || text.startsWith("  ")) cut = 2;
  else if (text.startsWith(" ")) cut = 1;
  if (cut > 0) editor.chain().focus().deleteRange({ from: start, to: start + cut }).run();
}

// ---------------------------------------------------------------------------
// 稿纸模式（字符网格：纵向每 1em 一线、横向每行一线，缩进与占位一目了然）
// ---------------------------------------------------------------------------

const gridOn = ref(localStorage.getItem("writer.grid") !== "0");
function toggleGrid() {
  gridOn.value = !gridOn.value;
  localStorage.setItem("writer.grid", gridOn.value ? "1" : "0");
}

watch(
  () => s.current?.path,
  (nv) => {
    if (nv && s.current) applyContent();
    else editor.commands.clearContent(false);
  },
);

// ---------------------------------------------------------------------------
// 工具栏状态（跟随选区高亮，FR-EDT-09）
// ---------------------------------------------------------------------------

const t = reactive({
  bold: false,
  italic: false,
  underline: false,
  strike: false,
  code: false,
  link: false,
  h: 0,
  bullet: false,
  ordered: false,
  task: false,
  quote: false,
  codeblock: false,
  inTable: false,
});

function syncToolbarState() {
  t.bold = editor.isActive("bold");
  t.italic = editor.isActive("italic");
  t.underline = editor.isActive("underline");
  t.strike = editor.isActive("strike");
  t.code = editor.isActive("code");
  t.link = editor.isActive("link");
  t.h = LEVELS.find((l) => editor.isActive("heading", { level: l })) ?? 0;
  t.bullet = editor.isActive("bulletList");
  t.ordered = editor.isActive("orderedList");
  t.task = editor.isActive("taskList");
  t.quote = editor.isActive("blockquote");
  t.codeblock = editor.isActive("codeBlock");
  t.inTable = editor.isActive("table");
}

const c = () => editor.chain().focus();

// ---------------------------------------------------------------------------
// 标题 / 字数
// ---------------------------------------------------------------------------

const titleLocal = ref("");
watch(
  () => s.current?.title,
  (nv) => {
    titleLocal.value = nv ?? "";
  },
  { immediate: true },
);

function onTitle() {
  if (!s.current) return;
  const v = titleLocal.value.trim();
  if (v && v !== s.current.title) void renameDoc(s.current.path, v);
}

let countTimer: number | undefined;
function scheduleCount() {
  clearTimeout(countTimer);
  countTimer = window.setTimeout(() => {
    updateCounts(editor.getJSON() as unknown as PMNode);
  }, 300);
}

const saveHint = computed(() => {
  switch (s.saveState) {
    case "saving":
      return "保存中…";
    case "dirty":
      return "未保存";
    case "error":
      return `保存失败：${s.saveError}`;
    case "conflict":
      return "文件已被外部修改";
    default:
      return s.savedAt ? `已保存 ${fmtTime(s.savedAt)}` : "已保存";
  }
});

function fmtTime(unix: number) {
  return new Date(unix * 1000).toTimeString().slice(0, 5);
}

function toggleLink() {
  if (t.link) {
    c().unsetLink().run();
    return;
  }
  const url = window.prompt("链接地址", "https://");
  if (url) c().setLink({ href: url }).run();
}

// ---------------------------------------------------------------------------
// 版本历史（FR-SAV-03：只读预览 + 字数差 + 回滚确认）
// ---------------------------------------------------------------------------

function fmtTs(ts: number) {
  return new Date(ts * 1000).toLocaleString("zh-CN", { hour12: false });
}

function srcLabel(src: string) {
  const map: Record<string, string> = {
    auto: "自动",
    manual: "手动",
    pre_rollback: "回滚前",
    pre_ai: "AI 前",
  };
  return map[src] ?? src;
}

function diffLabel(words: number) {
  const d = words - s.counts.words;
  if (d > 0) return `+${d} 字`;
  if (d < 0) return `${d} 字`;
  return "与当前相同";
}

async function onRestore(ts: number) {
  if (window.confirm("回滚到该快照？\n当前内容会先自动备份为「回滚前」快照，可再次回滚撤销。")) {
    await restoreSnapshot(ts);
  }
}

watch(
  () => s.current?.path,
  () => {
    if (s.history.open) void reloadHistory();
  },
);
</script>

<template>
  <div class="editorpane">
    <template v-if="s.current">
      <div class="editor-titlebar">
        <input
          v-model="titleLocal"
          class="doc-title"
          placeholder="无标题"
          @change="onTitle"
          @keydown.enter="($event.target as HTMLInputElement).blur()"
        />
        <span
          class="save-hint"
          :class="{ warn: s.saveState === 'error' || s.saveState === 'conflict' }"
        >
          {{ saveHint }}
        </span>
        <button class="btn small ghost" title="版本历史（每小时自动快照）" @click="openHistory()">
          历史
        </button>
        <button class="btn small ghost" title="导出（Ctrl+E）" @click="s.exportOpen = !s.exportOpen">
          导出
        </button>
      </div>

      <!-- 格式工具栏 -->
      <div class="toolbar">
        <button
          v-for="l in LEVELS"
          :key="'h' + l"
          class="tool"
          :class="{ on: t.h === l }"
          :title="`标题 ${l}（Ctrl+${l}）`"
          @click="c().toggleHeading({ level: l }).run()"
        >
          H{{ l }}
        </button>
        <span class="sep" />
        <button class="tool" :class="{ on: t.bold }" title="加粗（Ctrl+B）" @click="c().toggleBold().run()">B</button>
        <button class="tool it" :class="{ on: t.italic }" title="斜体（Ctrl+I）" @click="c().toggleItalic().run()">I</button>
        <button class="tool u" :class="{ on: t.underline }" title="下划线（Ctrl+U）" @click="c().toggleUnderline().run()">U</button>
        <button class="tool" :class="{ on: t.strike }" title="删除线" @click="c().toggleStrike().run()">S</button>
        <button class="tool" :class="{ on: t.code }" title="行内代码" @click="c().toggleCode().run()">‹›</button>
        <button class="tool" :class="{ on: t.link }" title="链接" @click="toggleLink">🔗</button>
        <span class="sep" />
        <button class="tool" :class="{ on: t.bullet }" title="无序列表" @click="c().toggleBulletList().run()">•—</button>
        <button class="tool" :class="{ on: t.ordered }" title="有序列表" @click="c().toggleOrderedList().run()">1.</button>
        <button class="tool" :class="{ on: t.task }" title="任务列表" @click="c().toggleTaskList().run()">☑</button>
        <button class="tool" :class="{ on: t.quote }" title="引用" @click="c().toggleBlockquote().run()">❝</button>
        <button class="tool" :class="{ on: t.codeblock }" title="代码块" @click="c().toggleCodeBlock().run()"> Code </button>
        <button class="tool" title="分割线" @click="c().setHorizontalRule().run()">—</button>
        <span class="sep" />
        <button
          class="tool"
          title="插入表格"
          @click="c().insertTable({ rows: 3, cols: 3, withHeaderRow: true }).run()"
        >
          ⊞ 表格
        </button>
        <span style="flex: 1" />
        <button class="tool" :class="{ on: gridOn }" title="稿纸模式（横线本 + 缩进参考线）" @click="toggleGrid">
          稿纸
        </button>
      </div>

      <!-- 表格浮动工具条（FR-EDT-09：表格编辑的操作载体） -->
      <div v-if="t.inTable" class="tablebar">
        <button class="tool" @click="c().addRowBefore().run()">↑插行</button>
        <button class="tool" @click="c().addRowAfter().run()">↓插行</button>
        <button class="tool" @click="c().deleteRow().run()">删行</button>
        <span class="sep" />
        <button class="tool" @click="c().addColumnBefore().run()">←插列</button>
        <button class="tool" @click="c().addColumnAfter().run()">→插列</button>
        <button class="tool" @click="c().deleteColumn().run()">删列</button>
        <span class="sep" />
        <button class="tool" @click="c().toggleHeaderRow().run()">表头</button>
        <button class="tool danger" @click="c().deleteTable().run()">删除表格</button>
      </div>

      <!-- 导出面板（FR-EXP，M2） -->
      <ExportPanel v-if="s.exportOpen" />

      <!-- 版本历史面板（FR-SAV-03） -->
      <div v-if="s.history.open" class="history-panel">
        <div class="history-head">
          <span class="history-title">版本历史</span>
          <span style="flex: 1"></span>
          <button class="btn small" @click="manualSnapshot()">手动快照</button>
          <button class="icon-btn" @click="s.history.open = false" title="关闭">✕</button>
        </div>
        <div class="history-list">
          <div
            v-for="snap in s.history.list"
            :key="snap.id"
            class="history-item"
            :class="{ active: s.history.previewTs === snap.ts }"
          >
            <div class="hi-line">
              <span class="hi-time">{{ fmtTs(snap.ts) }}</span>
              <span class="hi-src">{{ srcLabel(snap.source) }}</span>
              <span class="hi-diff">{{ diffLabel(snap.words) }}</span>
            </div>
            <div v-if="snap.note" class="hi-note">{{ snap.note }}</div>
            <div class="hi-ops">
              <button class="btn small ghost" @click="previewSnapshot(snap.ts)">预览</button>
              <button class="btn small" @click="onRestore(snap.ts)">回滚</button>
            </div>
          </div>
          <div v-if="!s.history.list.length" class="history-empty">
            暂无快照——每小时自动生成，或点上方"手动快照"
          </div>
        </div>
        <div v-if="s.history.preview" class="history-preview">
          <pre>{{ s.history.preview }}</pre>
        </div>
      </div>

      <!-- 冲突护栏提示（FR-SYN-09 初版）；force 跳过 flush，保证按钮在冲突态也可用 -->
      <div v-if="s.saveState === 'conflict'" class="conflict-banner">
        {{ s.saveError }}（本地未保存的修改将被放弃）
        <button class="btn small" @click="openDoc(s.current!.path, true)">重新加载磁盘版本</button>
      </div>

      <div class="editor-scroll" :class="{ 'editor-grid': gridOn }">
        <EditorContent :editor="editor" class="editor-content" />
      </div>
    </template>

    <div v-else class="editor-empty">
      <p>没有打开的文档</p>
      <button class="btn" @click="createDoc()">新建文档（Ctrl+N）</button>
    </div>
  </div>
</template>
