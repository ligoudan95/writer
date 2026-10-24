<script setup lang="ts">
import { ref, watch } from "vue";
import { save } from "@tauri-apps/plugin-dialog";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import { s, closeExport, flush, currentJson, refreshTree } from "../store";
import { api } from "../api";
import { exportTxt, exportMd, exportDocx, exportHtml, dedupeTitleH1 } from "../exporter";
import { exportTokens } from "../layout";
import { mdToDoc, type PMNode } from "../codec";

type Fmt = "txt" | "md" | "docx" | "pdf";
// 导出记忆（FR-EXP-01）：跨会话持久化上次格式与目标路径 + 导出选项
const fmt = ref<Fmt>((localStorage.getItem("writer.export.fmt") as Fmt) || "md");
const crlf = ref(false);
const bom = ref(localStorage.getItem("writer.export.bom") === "1"); // TXT UTF-8 BOM（FR-EXP-02）
const withFm = ref(localStorage.getItem("writer.export.fm") === "1"); // MD 含 frontmatter（FR-EXP-03）
const busy = ref(false);
const lastPath = ref(localStorage.getItem("writer.export.lastPath") || "");

watch(fmt, (v) => localStorage.setItem("writer.export.fmt", v));
watch(bom, (v) => localStorage.setItem("writer.export.bom", v ? "1" : "0"));
watch(withFm, (v) => localStorage.setItem("writer.export.fm", v ? "1" : "0"));

const FORMATS: { id: Fmt; label: string }[] = [
  { id: "txt", label: "TXT" },
  { id: "md", label: "Markdown" },
  { id: "docx", label: "Word" },
  { id: "pdf", label: "PDF" },
];

async function doExport() {
  if (!s.current || busy.value) return;
  const ok = await flush();
  if (!ok) return;
  const json0 = currentJson();
  if (!json0) return;
  busy.value = true;
  try {
    // H1 去重（FR-DOC-05）：正文首 H1 与标题相同 → 导出件不重复输出标题
    const json = dedupeTitleH1(json0, s.current.title);
    const lt = exportTokens(); // 排版与屏幕同源（FR-UI-03）
    const base = (s.current.title || "未命名").replace(/[\\/:*?"<>|]/g, "_");
    const target = await save({
      defaultPath: lastPath.value || `${base}.${fmt.value}`,
      filters: [{ name: fmt.value.toUpperCase(), extensions: [fmt.value] }],
    });
    if (!target) return;
    if (fmt.value === "pdf") {
      // 渲染舱管线（FR-EXP-05）：分页 + COM 打印在 Rust 侧完成；排版 token 内嵌正文
      await api.pdfExport(s.current.title || base, exportHtml(json, lt), target);
    } else {
      let bytes: number[];
      if (fmt.value === "docx") {
        bytes = Array.from(await exportDocx(json, lt));
      } else if (fmt.value === "txt") {
        bytes = encodeText(exportTxt(json, crlf.value), bom.value);
      } else {
        bytes = encodeText(
          exportMd(json, withFm.value ? frontmatterOf(s.current.title, s.current.uuid, s.current.privacy) : ""),
          false,
        );
      }
      await api.exportWrite(target, bytes);
    }
    lastPath.value = target; // 记忆上次格式与路径（FR-EXP-01，跨会话）
    localStorage.setItem("writer.export.lastPath", target);
    const msg = `已导出：${target.split(/[\\/]/).pop()}`;
    s.toast = { msg };
    setTimeout(() => {
      if (s.toast?.msg === msg) s.toast = null;
    }, 4000);
    await revealItemInDir(target).catch(() => undefined);
  } catch (e) {
    // 导出失败反馈（FR-SET-03：目标被占用/无权限等明确告知，不再静默）
    s.toast = { msg: `导出失败：${(e as { message?: string }).message ?? e}` };
    setTimeout(() => {
      if (s.toast?.msg.startsWith("导出失败")) s.toast = null;
    }, 6000);
  } finally {
    busy.value = false;
  }
}

/** FR-EXP-01 批量导出：整个选中文件夹按文档树顺序逐篇导出（文件名=文档名） */
async function exportFolder() {
  if (busy.value) return;
  const folder = s.selectedFolder || "docs";
  await refreshTree();
  const collect = (nodes: typeof s.tree, inFolder: boolean): { path: string; title: string }[] => {
    const out: { path: string; title: string }[] = [];
    for (const n of nodes) {
      if (n.kind === "folder") {
        out.push(...collect(n.children ?? [], inFolder || n.id === folder));
      } else if (inFolder || folder === "docs") {
        out.push({ path: n.id, title: n.name });
      }
    }
    return out;
  };
  const docs = collect(s.tree, false);
  if (!docs.length) return;
  const { open } = await import("@tauri-apps/plugin-dialog");
  const dir = await open({ directory: true, title: `导出 ${docs.length} 篇到…` });
  if (!dir || Array.isArray(dir)) return;
  busy.value = true;
  let n = 0;
  let failed = 0;
  try {
    const lt = exportTokens();
    for (const d of docs) {
      try {
        const payload = await api.readDoc(d.path);
        const json = dedupeTitleH1(mdToDoc(payload.body) as PMNode, d.title);
        const safe = d.title.replace(/[\\/:*?"<>|]/g, "_");
        const target = `${dir}\\${safe}.${fmt.value}`;
        if (fmt.value === "pdf") {
          await api.pdfExport(d.title, exportHtml(json, lt), target);
        } else if (fmt.value === "docx") {
          await api.exportWrite(target, Array.from(await exportDocx(json, lt)));
        } else if (fmt.value === "txt") {
          await api.exportWrite(target, encodeText(exportTxt(json, crlf.value), bom.value));
        } else {
          await api.exportWrite(target, encodeText(exportMd(json, withFm.value ? frontmatterOf(d.title, payload.uuid, payload.privacy) : ""), false));
        }
        n++;
      } catch {
        failed++;
      }
    }
    s.toast = { msg: `批量导出完成：${n} 篇${failed ? `，${failed} 篇失败` : ""} → ${dir}` };
    setTimeout(() => (s.toast = null), 5000);
  } finally {
    busy.value = false;
  }
}

/** FR-EXP-08 合并导出：文档树顺序合并为单一文件（MD/TXT 用 --- 分隔，
 * DOCX/PDF 合并为单一 DocModel 经无头转换；每篇前置 H1 章节标题） */
async function exportMerged() {
  if (busy.value) return;
  const folder = s.selectedFolder || "docs";
  await refreshTree();
  const collect = (nodes: typeof s.tree, inFolder: boolean): { path: string; title: string }[] => {
    const out: { path: string; title: string }[] = [];
    for (const n of nodes) {
      if (n.kind === "folder") {
        out.push(...collect(n.children ?? [], inFolder || n.id === folder));
      } else if (inFolder || folder === "docs") {
        out.push({ path: n.id, title: n.name });
      }
    }
    return out;
  };
  const docs = collect(s.tree, false);
  if (!docs.length) return;
  busy.value = true;
  try {
    await flush();
    const parts: PMNode[] = [];
    const hr = { type: "horizontalRule" } as PMNode;
    for (let i = 0; i < docs.length; i++) {
      const d = docs[i];
      const payload = await api.readDoc(d.path);
      const json = dedupeTitleH1(mdToDoc(payload.body) as PMNode, d.title);
      if (i > 0) parts.push(hr);
      parts.push({ type: "heading", attrs: { level: 1 }, content: [{ type: "text", text: d.title }] } as PMNode);
      parts.push(...(json.content ?? []));
    }
    const merged = { type: "doc", content: parts } as PMNode;
    const book = (folder === "docs" ? "整库合集" : folder.split("/").pop() ?? "合集").replace(/[\/:*?"<>|]/g, "_");
    const lt = exportTokens();
    const target = await save({
      defaultPath: lastPath.value || `${book}.${fmt.value}`,
      filters: [{ name: fmt.value.toUpperCase(), extensions: [fmt.value] }],
    });
    if (!target) return;
    if (fmt.value === "pdf") {
      await api.pdfExport(book, exportHtml(merged, lt), target);
    } else if (fmt.value === "docx") {
      await api.exportWrite(target, Array.from(await exportDocx(merged, lt)));
    } else if (fmt.value === "txt") {
      await api.exportWrite(target, encodeText(exportTxt(merged, crlf.value), bom.value));
    } else {
      await api.exportWrite(target, encodeText(exportMd(merged, ""), false));
    }
    lastPath.value = target;
    localStorage.setItem("writer.export.lastPath", target);
    s.toast = { msg: `已合并导出 ${docs.length} 篇 → ${target.split(/[\/]/).pop()}` };
    setTimeout(() => (s.toast = null), 4000);
    await revealItemInDir(target).catch(() => undefined);
  } catch (e) {
    s.toast = { msg: `合并导出失败：${(e as { message?: string }).message ?? e}` };
    setTimeout(() => {
      if (s.toast?.msg.startsWith("合并导出失败")) s.toast = null;
    }, 6000);
  } finally {
    busy.value = false;
  }
}

function encodeText(text: string, withBom: boolean): number[] {
  const prefix = withBom ? [0xef, 0xbb, 0xbf] : [];
  return [...prefix, ...new TextEncoder().encode(text)];
}

/** MD 导出附 frontmatter（可选，FR-EXP-03）：从文档自身元数据构造最小集 */
function frontmatterOf(title: string, uuid?: string, privacy?: boolean): string {
  const lines = ["---", `title: ${title.replace(/\n/g, " ")}`];
  if (uuid) lines.push(`id: ${uuid}`);
  if (privacy) lines.push("privacy: true");
  lines.push("---", "");
  return lines.join("\n");
}
</script>

<template>
  <div class="history-panel export-panel">
    <div class="history-head">
      <span class="history-title">导出文档</span>
      <span style="flex: 1"></span>
      <button class="icon-btn" title="关闭" @click="closeExport()">✕</button>
    </div>
    <div class="export-body">
      <div class="fmt-row">
        <button
          v-for="f in FORMATS"
          :key="f.id"
          class="tool fmt"
          :class="{ on: fmt === f.id }"
          @click="fmt = f.id"
        >
          {{ f.label }}
        </button>
      </div>
      <label v-if="fmt === 'txt'" class="export-opt">
        <input v-model="crlf" type="checkbox" /> 使用 Windows 换行（CRLF）
      </label>
      <label v-if="fmt === 'txt'" class="export-opt">
        <input v-model="bom" type="checkbox" /> UTF-8 带 BOM（记事本正确识别）
      </label>
      <label v-if="fmt === 'md'" class="export-opt">
        <input v-model="withFm" type="checkbox" /> 包含 frontmatter（标题/编号/隐私标记）
      </label>
      <p class="export-hint">
        {{ fmt === "txt" ? "纯文本：剥离格式标记，保留标题与段落文字" : fmt === "md" ? "Markdown：标准方言直出（含表格/任务列表）" : fmt === "docx" ? "Word：内容保真排版（标题样式/列表/表格/引用）" : "PDF：A4 分页排版，含页码与文档标题页眉（离屏渲染舱）" }}
      </p>
      <button class="btn primary" :disabled="busy" @click="doExport()">
        {{ busy ? "导出中…" : "选择位置并导出（Ctrl+E）" }}
      </button>
      <button class="btn ghost" :disabled="busy" @click="exportFolder()">
        批量导出{{ s.selectedFolder ? "当前文件夹" : "整库" }}（{{ fmt.toUpperCase() }}）
      </button>
      <button class="btn ghost" :disabled="busy" @click="exportMerged()">
        合并导出为单文件（{{ fmt.toUpperCase() }}，章节标题 + 分隔线）
      </button>
    </div>
  </div>
</template>
