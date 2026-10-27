<script setup lang="ts">
import { reactive, ref, watch, computed, onMounted, onBeforeUnmount } from "vue";
import { Editor, EditorContent } from "@tiptap/vue-3";
import StarterKit from "@tiptap/starter-kit";
import { Table, TableRow, TableHeader, TableCell } from "@tiptap/extension-table";
import { ImageNode } from "../image-node";
import Highlight from "@tiptap/extension-highlight";

// 表格节点扩展：列对齐（表级 align + 单元格 style）必须入 schema，
// 否则 setContent 时未知属性被丢弃、往返丢对齐（FR-EDT-01 方言 / FR-EDT-09 合并）
const TableWithAlign = Table.extend({
  addAttributes() {
    return {
      ...this.parent?.(),
      align: { default: null, renderHTML: () => ({}), parseHTML: () => null },
    };
  },
});
const styledCell = (Base: typeof TableCell | typeof TableHeader) =>
  Base.extend({
    addAttributes() {
      return {
        ...this.parent?.(),
        style: { default: null },
      };
    },
  });
import TaskList from "@tiptap/extension-task-list";
import TaskItem from "@tiptap/extension-task-item";
import {
  s,
  scheduleSave,
  renameDoc,
  createDoc,
  openDoc,
  bindEditor,
  bindEditorPos,
  updateCounts,
  openHistory,
  reloadHistory,
  manualSnapshot,
  previewSnapshot,
  restoreSnapshot,
  saveSession,
  consumePendingSessionPos,
} from "../store";
import { mdToDoc, docToMd, type PMNode } from "../codec";
import { diffOps, opsToBlocks } from "../diff";
import { AiDraft } from "../aidraft-mark";
import {
  ai,
  setAiEditor,
  bindAiDocJson,
  acceptContinue,
  dismissContinue,
  maybeCollapseDraft,
  restoreCollapsedDraft,
  cancelRunning,
  openPolishUI,
  startContinue,
  openChat,
} from "../aiflow";
import { api } from "../api";
import { openUrl } from "@tauri-apps/plugin-opener";
import { confirm as dialogConfirm } from "@tauri-apps/plugin-dialog";
import { markTyping } from "../notify";
import PolishPanel from "./PolishPanel.vue";
import ExportPanel from "./ExportPanel.vue";

type HeadingLevel = 1 | 2 | 3 | 4 | 5 | 6;
const LEVELS: HeadingLevel[] = [1, 2, 3, 4, 5, 6];

// ---------------------------------------------------------------------------
// 编辑器实例（setup 作用域创建；文档切换只换内容，实例常驻）
// ---------------------------------------------------------------------------

const editor = new Editor({
  content: { type: "doc", content: [{ type: "paragraph" }] },
  extensions: [
    StarterKit.configure({
      link: { openOnClick: false, autolink: true },
      // 撤销栈上限（FR-EDT-03 v1.7.1：500 步，淘汰区段由快照兜底）
      undoRedo: { depth: 500 },
    }),
    TaskList,
    TaskItem.configure({ nested: true }),
    TableWithAlign.configure({ resizable: false }),
    TableRow,
    styledCell(TableHeader),
    styledCell(TableCell),
    ImageNode,
    Highlight,
    AiDraft,
  ],
  editorProps: {
    // 粘贴清理（FR-IMP-02，V1.2）：默认合并格式（schema 白名单天然剥离背景色/
    // 字体等冗余样式，此处再剥 script/style 与行内样式做纵深防御）；
    // Ctrl+Shift+V 仅保留文本
    transformPastedHTML(html: string): string {
      return html
        .replace(/<style[\s\S]*?<\/style>/gi, "")
        .replace(/<script[\s\S]*?<\/script>/gi, "")
        .replace(/<!--[\s\S]*?-->/g, "")
        .replace(/\s(?:style|class|width|height|bgcolor|color|face|size)="[^"]*"/gi, "");
    },
    handlePaste(_view: unknown, event: ClipboardEvent) {
      if (!(event as ClipboardEvent & { shiftKey?: boolean }).shiftKey) return false;
      const text = event.clipboardData?.getData("text/plain");
      if (!text) return false;
      event.preventDefault();
      editor.chain().focus().insertContent(text).run();
      return true;
    },
    handleKeyDown(_view, event) {
      // AI 续写草稿态（FR-AI-03）：Tab 接受 / Esc 放弃（优先于缩进）；
      // 收起态 Tab = 取回（不能对已移除的区间走 accept，否则草稿文本静默丢失）
      if (ai.cont.state === "draft") {
        if (event.key === "Tab" && !event.ctrlKey && !event.metaKey && !event.altKey) {
          event.preventDefault();
          if (ai.cont.collapsed) restoreCollapsedDraft();
          else acceptContinue();
          return true;
        }
        if (event.key === "Escape") {
          event.preventDefault();
          dismissContinue();
          return true;
        }
      } else if (ai.cont.collapsed && event.key === "Tab" && !event.ctrlKey && !event.metaKey && !event.altKey) {
        event.preventDefault();
        restoreCollapsedDraft();
        return true;
      }
      // 润色处理中：任意按键 → 取消请求并立即解锁（不吞输入，FR-AI-02）
      if (ai.polish.state === "running") {
        cancelRunning("polish");
        return false;
      }
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
      // AI 入口：Ctrl+Shift+P 润色 / Ctrl+Shift+Enter 续写
      if ((event.ctrlKey || event.metaKey) && event.shiftKey && event.key.toLowerCase() === "p") {
        event.preventDefault();
        openPolishUI();
        return true;
      }
      if ((event.ctrlKey || event.metaKey) && event.shiftKey && event.key === "Enter") {
        event.preventDefault();
        void startContinue();
        return true;
      }
      return false;
    },
    // FR-EDT-02：Ctrl+点击链接 → 系统默认浏览器打开（首次外发确认一次）
    handleClick(_view, pos, event) {
      const me = event as MouseEvent;
      if (!(me.ctrlKey || me.metaKey)) return false;
      const link = editor.state.doc.resolve(pos).marks().find((m) => m.type.name === "link");
      const href = link?.attrs.href;
      if (typeof href === "string" && href) {
        void openExternalLink(href);
        return true;
      }
      return false;
    },
  },
  onUpdate: () => {
    if (aiSuppressSave) return; // 草稿操作期间抑制自动保存
    markTyping(); // 输入保护（FR-UI-06）：通知中心据此延后非紧急通知
    // 用户在草稿插入点之前输入 → 草稿收起为"待插入"（FR-AI-03）
    maybeCollapseDraft(editor.state.selection.head);
    scheduleSave();
    scheduleCount();
  },
});

// AI 编辑器桥实现（aiflow 不依赖 TipTap；本文件是唯一耦合点）
let aiSuppressSave = false;

function deepMark(nodes: PMNode["content"]): PMNode["content"] {
  const mark = { type: "aiDraft" } as never;
  // text 与 image（等无 text 的行内节点）都加 mark：纯图片草稿段也必须能被 strip
  const walk = (n: PMNode): PMNode => ({
    ...n,
    marks: n.text || n.type === "image" ? [...(n.marks ?? []), mark as never] : n.marks,
    content: n.content ? n.content.map(walk) : undefined,
  });
  return nodes?.map(walk);
}

function nodesOf(md: string): PMNode["content"] {
  const doc = mdToDoc(md) as PMNode;
  return (doc.content ?? []) as PMNode["content"];
}

/** 深拷贝并剔除 AI 草稿块：草稿经 insertContent 以整块插入（段落级），
 * 段落内任何 aiDraft mark 即整段为草稿（含其中的 image——mark 只加在 text 上），
 * 未接受前不得进入保存/导出/上下文 */
function stripDraftNodes(json: PMNode): PMNode {
  const hasDraftMark = (n: PMNode): boolean =>
    !!n.marks?.some((m) => m.type === "aiDraft") || !!(n.content ?? []).some(hasDraftMark);
  const walk = (n: PMNode): PMNode | null => {
    if ((n.type === "paragraph" || n.type === "heading") && hasDraftMark(n)) return null;
    if (n.type === "text" && n.marks?.some((m) => m.type === "aiDraft")) return null;
    if (!n.content) return n;
    const content = n.content.map(walk).filter((x): x is PMNode => !!x);
    return { ...n, content };
  };
  const content = (json.content ?? []).map(walk).filter((x): x is PMNode => !!x);
  return { ...json, content };
}

setAiEditor({
  getSelection: () => {
    const { from, to, empty } = editor.state.selection;
    if (empty) return null;
    const slice = editor.state.doc.slice(from, to);
    const md = docToMd({ type: "doc", content: slice.content.toJSON() as never } as PMNode);
    return { from, to, text: md };
  },
  replaceRangeWithMd: (from, to, md) => {
    try {
      editor
        .chain()
        .focus()
        .insertContentAt({ from, to }, nodesOf(md) as never, { updateSelection: true })
        .run();
    } catch {
      // 润色区间失效（文档已变）：放弃应用，调用方仍会收尾面板状态
    }
  },
  insertDraft: (md) => {
    try {
      const anchor = editor.state.selection.to;
      aiSuppressSave = true;
      editor
        .chain()
        .insertContentAt(anchor, deepMark(nodesOf(md)) as never)
        .run();
      return { from: anchor, to: editor.state.selection.to };
    } catch {
      return null; // 区间失效（内容已被替换）：调用方按 null 容错
    } finally {
      aiSuppressSave = false;
    }
  },
  updateDraft: (from, to, md) => {
    try {
      aiSuppressSave = true;
      editor
        .chain()
        .insertContentAt({ from, to }, deepMark(nodesOf(md)) as never)
        .run();
      return { from, to: editor.state.selection.to };
    } catch {
      return null;
    } finally {
      aiSuppressSave = false;
    }
  },
  acceptDraft: (from, to) => {
    try {
      aiSuppressSave = true;
      editor
        .chain()
        .command(({ tr }) => {
          tr.removeMark(from, to, "aiDraft" as never);
          return true;
        })
        .run();
    } catch {
      // 区间失效：接受操作退化为无操作（草稿文本随编辑器状态已不存在）
    } finally {
      aiSuppressSave = false;
    }
  },
  removeDraft: (from, to) => {
    try {
      aiSuppressSave = true;
      editor.chain().deleteRange({ from, to }).run();
    } catch {
      // 区间失效：草稿已不存在
    } finally {
      aiSuppressSave = false;
    }
  },
  getCursor: () => editor.state.selection.to,
  contextAround: (n) => {
    const pos = editor.state.selection.to;
    const size = editor.state.doc.content.size;
    const before = editor.state.doc.textBetween(Math.max(0, pos - n), pos, "\n");
    const after = editor.state.doc.textBetween(pos, Math.min(size, pos + n), "\n");
    const all = editor.state.doc.textBetween(0, pos, "\n");
    const h = all.lastIndexOf("\n# ");
    const chapterStart = h >= 0 ? all.slice(h + 1, Math.min(all.length, h + 1 + n)) : all.slice(0, Math.min(all.length, n));
    return { before, after, chapterStart };
  },
  chapterText: () => {
    // 光标所在章节：光标前文本中最近一个 H1 起至光标，从头起截 6000 字封顶
    //（此前 textBetween 起点恒为 0 属笔误：长文拿到的是文档开头而非当前章节）
    const before = editor.state.doc.textBetween(0, editor.state.selection.to, "\n");
    const h = before.lastIndexOf("\n# ");
    const start = h >= 0 ? h + 1 : 0;
    return before.slice(start, start + 6000);
  },
  insertAtCursor: (md) => {
    editor
      .chain()
      .focus()
      .insertContentAt(editor.state.selection.to, nodesOf(md) as never)
      .run();
  },
  suppressSave: (on) => {
    aiSuppressSave = on;
  },
});
bindAiDocJson(() => stripDraftNodes(editor.getJSON() as unknown as PMNode));

onMounted(() => {
  // 保存/导出/上下文取 DocModel 时剔除未接受的 AI 草稿文本（mark 白名单只剥 mark 不剥文字）
  bindEditor(() => stripDraftNodes(editor.getJSON() as unknown as PMNode));
  // 位置桥：远端更新重载时保持光标与滚动（FR-SYN-09 / A15）
  const scroller = () => document.querySelector(".editor-scroll");
  bindEditorPos(
    () => {
      try {
        return {
          head: editor.state.selection.head,
          scrollTop: scroller()?.scrollTop ?? 0,
        };
      } catch {
        return null;
      }
    },
    (head, scrollTop) => {
      const size = editor.state.doc.content.size;
      const pos = Math.min(Math.max(head, 0), size);
      editor.commands.setTextSelection(pos);
      requestAnimationFrame(() => {
        scroller()?.scrollTo({ top: scrollTop });
      });
    },
  );
  editorGetPosRef = () => {
    try {
      return {
        head: editor.state.selection.head,
        scrollTop: scroller()?.scrollTop ?? 0,
      };
    } catch {
      return null;
    }
  };
  if (s.current) applyContent();
  else updateCounts({ type: "doc", content: [] });
  editor.on("transaction", syncToolbarState);
  editor.on("transaction", updateSelectionCounts);
  editor.on("transaction", () => {
    docVersion.value++;
  });
});

onBeforeUnmount(() => {
  editor.off("transaction", syncToolbarState);
  editor.off("transaction", updateSelectionCounts);
  window.clearInterval(sessionTimer);
  editor.destroy();
});

function applyContent() {
  if (!s.current) return;
  editor.commands.setContent(mdToDoc(s.current.body) as never, { emitUpdate: false });
  updateCounts(editor.getJSON() as unknown as PMNode);
  // 会话恢复（FR-UI-04）：打开后一次性恢复上次光标与滚动
  const pos = consumePendingSessionPos(s.current.path);
  if (pos) {
    const size = editor.state.doc.content.size;
    editor.commands.setTextSelection(Math.min(Math.max(pos.head, 0), size));
    requestAnimationFrame(() => {
      document.querySelector(".editor-scroll")?.scrollTo({ top: pos.scrollTop });
    });
    return;
  }
  // 全文搜索定位（FR-SEARCH-01）：首个命中词 → 光标 + 滚动（一次性消费）
  const locate = s.pendingLocate;
  if (locate) {
    s.pendingLocate = "";
    const target = locate.toLowerCase();
    let found = false;
    editor.state.doc.descendants((node, nodePos) => {
      if (found || !node.isText || !node.text) return !found;
      const i = node.text.toLowerCase().indexOf(target);
      if (i >= 0) {
        found = true;
        editor
          .chain()
          .setTextSelection(nodePos + i)
          .scrollIntoView()
          .run();
      }
      return !found;
    });
  }
}

/** 选区统计（FR-EDT-07）：选区变化时刷新状态栏选区字数。
 * 记忆化 (from,to,doc 内容版本)：键盘自动重复/带选区格式化不重复做 O(选区长) 扫描 */
let lastSelKey = "";
function updateSelectionCounts() {
  const { from, to, empty } = editor.state.selection;
  const key = empty ? "-" : `${from}:${to}:${editor.state.doc.content.size}`;
  if (key === lastSelKey) return;
  lastSelKey = key;
  if (empty) {
    if (s.counts.selWords || s.counts.selChars) {
      s.counts.selWords = 0;
      s.counts.selChars = 0;
    }
    return;
  }
  const text = editor.state.doc.textBetween(from, to, "\n");
  const words = (text.match(/[\p{Script=Han}\p{Script=Hiragana}\p{Script=Katakana}]|[A-Za-z0-9]+/gu) ?? []).length;
  s.counts.selWords = words;
  s.counts.selChars = text.replace(/\n/g, "").length;
}

/** 会话位置持久化（FR-UI-04）：每 5 秒与光标移动后的去抖时刻 */
const sessionTimer = window.setInterval(() => {
  const pos = editorGetPosRef();
  saveSession(pos);
}, 5000);

let editorGetPosRef: () => { head: number; scrollTop: number } | null = () => null;

/** FR-EDT-02：外部链接打开（系统默认浏览器）；首次外发确认一次，确认后本机不再询问 */
async function openExternalLink(href: string) {
  if (!/^https?:\/\//i.test(href)) {
    s.toast = { msg: "仅支持打开 http/https 链接" };
    setTimeout(() => {
      if (s.toast?.msg === "仅支持打开 http/https 链接") s.toast = null;
    }, 3000);
    return;
  }
  if (localStorage.getItem("writer.extlink.ok") !== "1") {
    const ok = await dialogConfirm(`即将在系统默认浏览器打开外部链接：\n${href}\n\n允许？（仅首次询问）`, {
      title: "打开外部链接",
    });
    if (!ok) return;
    localStorage.setItem("writer.extlink.ok", "1");
  }
  try {
    await openUrl(href);
  } catch {
    s.toast = { msg: "打开链接失败" };
    setTimeout(() => {
      if (s.toast?.msg === "打开链接失败") s.toast = null;
    }, 3000);
  }
}

/** Shift+Tab：移除当前段行首的缩进（按实际缩进单位删，不得误删正文首字） */
function removeLineIndent() {
  const { state } = editor;
  const { $from } = state.selection;
  const start = $from.start();
  const text = $from.parent.textContent;
  let cut = 0;
  if (text.startsWith("　　")) cut = 2;
  else if (text.startsWith("　")) cut = 1; // 单全角空格 = 只删它自己
  else if (text.startsWith("  ")) cut = 2;
  else if (text.startsWith(" ")) cut = 1;
  if (cut > 0) editor.chain().focus().deleteRange({ from: start, to: start + cut }).run();
}

// ---------------------------------------------------------------------------
// 稿纸模式（字符网格：纵向每 1em 一线、横向每行一线，缩进与占位一目了然）

// 专注模式（V1.1，F11）：隐藏侧栏/工具栏，状态栏淡化悬停浮现
function toggleFocus() {
  s.focusMode = !s.focusMode;
  localStorage.setItem("writer.focus", s.focusMode ? "1" : "0");
}
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

// 远端更新重载（FR-SYN-09）：path 不变、内容已换 → nonce 驱动重应用（撤销栈清空）
watch(
  () => s.reloadNonce,
  () => {
    if (s.current) applyContent();
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
  highlight: false,
  code: false,
  link: false,
  h: 0,
  bullet: false,
  ordered: false,
  task: false,
  quote: false,
  codeblock: false,
  inTable: false,
  inImage: false,
  imgWidth: 0,
  imgAlign: "",
  hasSel: false,
});

function syncToolbarState() {
  t.bold = editor.isActive("bold");
  t.italic = editor.isActive("italic");
  t.underline = editor.isActive("underline");
  t.strike = editor.isActive("strike");
  t.highlight = editor.isActive("highlight");
  t.code = editor.isActive("code");
  t.link = editor.isActive("link");
  t.h = LEVELS.find((l) => editor.isActive("heading", { level: l })) ?? 0;
  t.bullet = editor.isActive("bulletList");
  t.ordered = editor.isActive("orderedList");
  t.task = editor.isActive("taskList");
  t.quote = editor.isActive("blockquote");
  t.codeblock = editor.isActive("codeBlock");
  t.inTable = editor.isActive("table");
  t.inImage = editor.isActive("image");
  t.imgWidth = Number(editor.getAttributes("image").width) || 0;
  t.imgAlign = String(editor.getAttributes("image").align ?? "");
  t.hasSel = !editor.state.selection.empty;
}

const c = () => editor.chain().focus();

// 图片操作（FR-EDT-11，V1.2）
const imgPreview = ref("");
function setImgWidth(w: number) {
  c().updateAttributes("image", { width: w }).run();
}
function setImgAlign(a: string) {
  const cur = String(editor.getAttributes("image").align ?? "");
  c().updateAttributes("image", { align: cur === a ? null : a }).run();
}
function previewImage() {
  imgPreview.value = String(editor.getAttributes("image").src ?? "");
}
async function saveImageAs() {
  const src = String(editor.getAttributes("image").src ?? "");
  if (!src) return;
  try {
    const { readFile, writeFile } = await import("@tauri-apps/plugin-fs");
    const { save } = await import("@tauri-apps/plugin-dialog");
    const ext = src.split(".").pop()?.split("?")[0] ?? "png";
    const dest = await save({ defaultPath: `writer-image.${ext}` });
    if (!dest) return;
    // 仅支持本地路径（http 外链另存为需网络下载，V1 不做）
    if (!/^(attachments\/|[A-Za-z]:\|\\)/.test(src) && !src.startsWith("attachments")) {
      s.toast = { msg: "仅支持本地库内图片另存（外链请右键浏览器保存）" };
      setTimeout(() => (s.toast = null), 3500);
      return;
    }
    const data = await readFile(src);
    await writeFile(dest, data);
    s.toast = { msg: "图片已另存" };
    setTimeout(() => (s.toast = null), 2500);
  } catch (e) {
    s.toast = { msg: `另存失败：${(e as { message?: string }).message ?? e}` };
    setTimeout(() => (s.toast = null), 4000);
  }
}

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

async function togglePrivacy() {
  const cur = s.current;
  if (!cur) return;
  const next = !cur.privacy;
  const r = await api.docSetPrivacy(cur.path, next);
  s.current = { ...cur, privacy: next };
  s.lastReadHash = r.hash;
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

// 敏感词/错别字检查（V1.3）
const lintOpen = ref(false);
const lintHits = ref<import("../api").LintHit[]>([]);
const lintScopeAll = ref(false);
const lintBusy = ref(false);
const lintCustom = ref("");
const KIND_TIP: Record<string, string> = {
  sensitive: "极限词（广告法红线，发布场景注意）",
  custom: "自定义敏感词",
  duplication: "疑似叠词笔误",
};
async function toggleLint() {
  lintOpen.value = !lintOpen.value;
  if (lintOpen.value) {
    lintCustom.value = await api.lintCustomWords();
    void runLint();
  }
}
async function runLint() {
  lintBusy.value = true;
  try {
    lintHits.value = await api.lintCheck(lintScopeAll.value || !s.current ? "" : s.current.path);
  } finally {
    lintBusy.value = false;
  }
}
async function saveLintCustom() {
  await api.lintSetCustomWords(lintCustom.value);
  void runLint();
}
function jumpLint(hit: import("../api").LintHit) {
  if (s.current?.path !== hit.path) void openDoc(hit.path);
  s.pendingLocate = hit.word;
}

// 大纲视图与拖拽排序（V1.2）：章节区块 = 标题起至下一同级/更高级标题前
const outlineOpen = ref(false);
// ProseMirror 的 state/doc 不是 Vue 响应式对象：computed 读取它不会建立依赖，
// 只会在 outlineOpen 切换时算一次——之后编辑器任何变化都不刷新（过期快照），
// 拖拽排序用过期位置会删改错误文本区间。用事务计数器驱动重算。
const docVersion = ref(0);
const outline = computed(() => {
  docVersion.value; // 响应式依赖：每次编辑器事务自增，驱动本 computed 重算
  if (!outlineOpen.value) return [];
  const hs: { level: number; pos: number }[] = [];
  editor.state.doc.descendants((node, pos) => {
    if (node.type.name === "heading") hs.push({ level: Number(node.attrs.level ?? 1), pos });
    return true;
  });
  const size = editor.state.doc.content.size;
  return hs.map((h, i) => {
    let to = size;
    for (let j = i + 1; j < hs.length; j++) {
      if (hs[j].level <= h.level) {
        to = hs[j].pos;
        break;
      }
    }
    const node = editor.state.doc.nodeAt(h.pos);
    return { id: i, level: h.level, text: node?.textContent ?? "(空标题)", from: h.pos, to };
  });
});
function outlineJump(o: { from: number }) {
  editor.chain().focus().setTextSelection(o.from).scrollIntoView().run();
}
const outlineDrag = ref(-1);
const outlineDrop = ref<{ id: number; mode: "" | "before" | "after" }>({ id: -1, mode: "" });
async function moveSection(srcId: number, dstId: number, mode: "before" | "after") {
  const list = outline.value;
  const src = list.find((x) => x.id === srcId);
  const dst = list.find((x) => x.id === dstId);
  if (!src || !dst || srcId === dstId) return;
  const size = editor.state.doc.content.size;
  if (src.from < 0 || src.to > size || dst.from > size || dst.to > size) return; // 过期防御
  // 数据红线：拖拽整段移动属结构性改写，改前强制快照（失败即中止拖拽）
  if (s.current) {
    try {
      await api.snapshotCreate(s.current.path, "pre_outline", "大纲拖拽前自动备份");
    } catch (e) {
      s.toast = { msg: `拖拽已取消：安全快照失败（${(e as { message?: string }).message ?? e}）` };
      setTimeout(() => {
        if (s.toast?.msg.startsWith("拖拽已取消")) s.toast = null;
      }, 5000);
      return;
    }
  }
  const tr = editor.state.tr;
  const slice = editor.state.doc.slice(src.from, src.to);
  let insertAt = mode === "before" ? dst.from : dst.to;
  // 位置修正：删除源后，源之后的插入点前移
  const len = src.to - src.from;
  if (insertAt >= src.to) insertAt -= len;
  else if (insertAt > src.from) return; // 拖进自己的区块内：无效操作
  tr.delete(src.from, src.to);
  tr.insert(insertAt, slice.content);
  editor.view.dispatch(tr);
  scheduleSave();
}

// 快照 diff 视图（V1.2）：快照 vs 当前正文，块级增删预览
const historyDiff = computed(() => {
  if (!s.history.preview) return [];
  const cur = s.current ? docToMd(stripDraftNodes(editor.getJSON() as unknown as PMNode)) : "";
  const ops = diffOps(cur, s.history.preview);
  return opsToBlocks(ops, 20);
});

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
        <template v-if="ai.enabled">
          <button
            class="btn small ghost"
            :class="{ warn: s.current?.privacy }"
            :title="s.current?.privacy ? '隐私文档：AI 已拦截（点击关闭标记）' : '标记为隐私（AI 永不发送其内容）'"
            @click="togglePrivacy"
          >
            {{ s.current?.privacy ? "🔒" : "🔓" }}
          </button>
          <button class="btn small ghost" title="AI 讨论（Ctrl+Shift+A）" @click="void openChat()">讨论</button>
          <button class="btn small ghost" title="AI Provider 设置" @click="ai.settingsOpen = !ai.settingsOpen">AI</button>
        </template>
      </div>

      <!-- 格式工具栏 -->
      <div v-show="!s.focusMode" class="toolbar">
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
        <button class="tool" :class="{ on: t.highlight }" title="高亮（==文本==）" @click="c().toggleHighlight().run()">🖍</button>
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
        <template v-if="ai.enabled">
          <button
            class="tool"
            :class="{ on: t.hasSel }"
            title="AI 润色（选中文字后 Ctrl+Shift+P）"
            @click="openPolishUI()"
          >
            ✨ 润色
          </button>
          <button class="tool" title="AI 续写（Ctrl+Shift+Enter，草稿中 Tab 接受 / Esc 放弃）" @click="void startContinue()">
            ⤵ 续写
          </button>
        </template>
        <button class="tool" :class="{ on: gridOn }" title="稿纸模式（横线本 + 缩进参考线）" @click="toggleGrid">
          稿纸
        </button>
        <button class="tool" :class="{ on: outlineOpen }" title="大纲（章节导航与拖拽排序）" @click="outlineOpen = !outlineOpen">
          大纲
        </button>
        <button class="tool" :class="{ on: lintOpen }" title="敏感词/错别字检查（V1.3）" @click="toggleLint">
          检查
        </button>
        <button class="tool" :class="{ on: s.focusMode }" title="专注模式（F11：隐藏侧栏/工具栏，状态栏悬停浮现）" @click="toggleFocus()">
          专注
        </button>
      </div>

      <!-- 大纲视图（V1.2）：章节树 + 点击定位 + 拖拽排序 -->
      <div v-if="outlineOpen" class="outline-panel">
        <div class="history-head">
          <span class="history-title">大纲</span>
          <span style="flex: 1"></span>
          <button class="icon-btn" @click="outlineOpen = false">✕</button>
        </div>
        <div class="outline-list">
          <div
            v-for="o in outline"
            :key="o.id"
            class="outline-item"
            :style="{ paddingLeft: 8 + (o.level - 1) * 14 + 'px' }"
            draggable="true"
            @dragstart="outlineDrag = o.id"
            @dragover.prevent="outlineDrop = { id: o.id, mode: ($event as DragEvent).offsetY < ($event.currentTarget as HTMLElement).offsetHeight / 2 ? 'before' : 'after' }"
            @dragleave="outlineDrop = { id: -1, mode: '' }"
            @drop.prevent="moveSection(outlineDrag, o.id, outlineDrop.mode === 'after' ? 'after' : 'before')"
            @click="outlineJump(o)"
          >
            <span class="outline-lv">{{ o.level }}</span>
            <span class="outline-text">{{ o.text }}</span>
            <span
              v-if="outlineDrop.id === o.id"
              class="outline-line"
              :class="outlineDrop.mode"
            ></span>
          </div>
          <div v-if="!outline.length" class="history-empty">文档暂无标题（用 # 起章后可在此拖拽排序）</div>
        </div>
      </div>

      <!-- 图片浮动操作条（FR-EDT-11，V1.2） -->
      <div v-if="t.inImage" class="tablebar">
        <button
          v-for="w in [25, 50, 75, 100]"
          :key="w"
          class="tool"
          :class="{ on: t.imgWidth === w }"
          @click="setImgWidth(w)"
        >{{ w }}%</button>
        <span class="sep" />
        <button class="tool" :class="{ on: t.imgAlign === 'l' }" title="左对齐" @click="setImgAlign('l')">⇤</button>
        <button class="tool" :class="{ on: t.imgAlign === 'c' }" title="居中" @click="setImgAlign('c')">↔</button>
        <button class="tool" :class="{ on: t.imgAlign === 'r' }" title="右对齐" @click="setImgAlign('r')">⇥</button>
        <span class="sep" />
        <button class="tool" title="放大查看" @click="previewImage">🔍</button>
        <button class="tool" title="另存为" @click="saveImageAs">💾</button>
      </div>

      <!-- 图片放大预览 -->
      <div v-if="imgPreview" class="modal-mask" @click.self="imgPreview = ''">
        <img :src="imgPreview" class="img-preview" @click="imgPreview = ''" />
      </div>

      <!-- 敏感词/错别字检查（V1.3） -->
      <div v-if="lintOpen" class="outline-panel" style="right: 14px; left: auto">
        <div class="history-head">
          <span class="history-title">敏感词 / 错别字</span>
          <span style="flex: 1"></span>
          <label class="switch" style="font-size: 11.5px" title="关闭 = 仅当前文档">
            <input v-model="lintScopeAll" type="checkbox" @change="runLint" /> 全库
          </label>
          <button class="icon-btn" @click="lintOpen = false">✕</button>
        </div>
        <div class="outline-list">
          <button v-for="(h, i) in lintHits" :key="i" class="lint-item" @click="jumpLint(h)">
            <span class="lint-word" :class="h.kind" :title="KIND_TIP[h.kind]">{{ h.word }}</span>
            <span class="muted">×{{ h.count }} · {{ s.current?.path === h.path ? "本文档" : h.path.split("/").pop() }}</span>
            <div class="lint-ctx">{{ h.context }}</div>
          </button>
          <div v-if="!lintBusy && !lintHits.length" class="history-empty">
            {{ lintScopeAll ? "全库未检出" : "当前文档未检出" }} ✓
          </div>
          <div v-if="lintBusy" class="history-empty">检查中…</div>
        </div>
        <div style="border-top: 1px solid var(--border); padding: 6px 10px">
          <input
            v-model="lintCustom"
            placeholder="自定义敏感词（逗号分隔），回车保存"
            style="width: 100%; border: 1px solid var(--border); border-radius: 6px; background: var(--bg); color: var(--fg); padding: 4px 8px; font-size: 12px"
            @keydown.enter.prevent="saveLintCustom"
          />
        </div>
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
        <button
          class="tool"
          title="选中多个单元格合并；光标在合并单元格内则拆分（HTML 表格方言承载）"
          @click="c().mergeOrSplit().run()"
        >
          合并/拆分
        </button>
        <button class="tool danger" @click="c().deleteTable().run()">删除表格</button>
      </div>

      <!-- 导出面板（FR-EXP，M2） -->
      <ExportPanel v-if="s.exportOpen" />

      <!-- AI 润色面板（FR-AI-02，M4） -->
      <PolishPanel v-if="ai.polishOpen || ai.polish.state !== 'idle'" />

      <!-- AI 续写草稿提示条（FR-AI-03：草稿态视觉区分 + 收起态取回） -->
      <div v-if="ai.cont.state !== 'idle' || ai.cont.collapsed" class="cont-bar">
        <template v-if="ai.cont.state === 'running'">⤵ 续写生成中…（Esc 取消）</template>
        <template v-else-if="ai.cont.state === 'draft'">
          ⤵ 灰色斜体为 AI 草稿：<b>Tab 接受</b> · Esc 放弃 · 插入点前输入将收起草稿
        </template>
        <template v-else-if="ai.cont.collapsed">⤵ 有一段待插入草稿：<b>Tab 取回</b></template>
        <span v-if="ai.cont.error" class="cont-err">{{ ai.cont.error }}</span>
      </div>

      <!-- 隐私文档常驻标识（FR-AI-06） -->
      <div v-if="s.current?.privacy" class="privacy-banner">
        🔒 隐私文档——内容永不被发送给 AI（润色/续写/文档级上下文自动拦截）
      </div>

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
        <div v-if="historyDiff.length" class="history-diff">
          <div class="hd-head">与当前差异（<span class="ins">+新增</span> / <span class="del">-缺失</span>）</div>
          <div class="hd-body">
            <template v-for="(b, i) in historyDiff" :key="i">
              <div v-if="b.before" class="hd-block del" :title="b.after ? '已修改（原文）' : '快照中有、当前没有'">
                <pre>- {{ b.before }}</pre>
              </div>
              <div v-if="b.after" class="hd-block ins" :title="b.before ? '已修改（快照）' : '快照独有'">
                <pre>+ {{ b.after }}</pre>
              </div>
            </template>
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
