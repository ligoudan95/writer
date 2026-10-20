<script setup lang="ts">
import { ref } from "vue";
import { save } from "@tauri-apps/plugin-dialog";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import { s, closeExport, flush, currentJson } from "../store";
import { api } from "../api";
import { exportTxt, exportMd, exportDocx, exportHtml } from "../exporter";

type Fmt = "txt" | "md" | "docx" | "pdf";
const fmt = ref<Fmt>("md");
const crlf = ref(false);
const busy = ref(false);
const lastPath = ref("");

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
  const json = currentJson();
  if (!json) return;
  busy.value = true;
  try {
    const base = (s.current.title || "未命名").replace(/[\\/:*?"<>|]/g, "_");
    const target = await save({
      defaultPath: lastPath.value || `${base}.${fmt.value}`,
      filters: [{ name: fmt.value.toUpperCase(), extensions: [fmt.value] }],
    });
    if (!target) return;
    if (fmt.value === "pdf") {
      // 渲染舱管线（FR-EXP-05）：分页 + COM 打印在 Rust 侧完成
      await api.pdfExport(s.current.title || base, exportHtml(json), target);
    } else {
      let bytes: number[];
      if (fmt.value === "docx") {
        bytes = Array.from(await exportDocx(json));
      } else {
        const text = fmt.value === "txt" ? exportTxt(json, crlf.value) : exportMd(json);
        bytes = Array.from(new TextEncoder().encode(text));
      }
      await api.exportWrite(target, bytes);
    }
    lastPath.value = target; // 记忆上次格式与路径（FR-EXP-01）
    const msg = `已导出：${target.split(/[\\/]/).pop()}`;
    s.toast = { msg };
    setTimeout(() => {
      if (s.toast?.msg === msg) s.toast = null;
    }, 4000);
    await revealItemInDir(target).catch(() => undefined);
  } finally {
    busy.value = false;
  }
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
      <p class="export-hint">
        {{ fmt === "txt" ? "纯文本：剥离格式标记，保留标题与段落文字" : fmt === "md" ? "Markdown：标准方言直出（含表格/任务列表）" : fmt === "docx" ? "Word：内容保真排版（标题样式/列表/表格/引用）" : "PDF：A4 分页排版，含页码与文档标题页眉（离屏渲染舱）" }}
      </p>
      <button class="btn primary" :disabled="busy" @click="doExport()">
        {{ busy ? "导出中…" : "选择位置并导出（Ctrl+E）" }}
      </button>
    </div>
  </div>
</template>
