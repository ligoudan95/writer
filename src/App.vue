<script setup lang="ts">
import { ref, onMounted, onBeforeUnmount } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import DocTree from "./components/DocTree.vue";
import EditorPane from "./components/EditorPane.vue";
import StatusBar from "./components/StatusBar.vue";
import { s, init, restorePending, discardPending, flush, saveNow, createDoc } from "./store";

const bootMsg = ref("正在启动…");
const bootErr = ref("");

let unlistenClose: (() => void) | null = null;

onMounted(async () => {
  // 稿纸对齐保障：强制缩放 100% 并禁用页面缩放热键——
  // 非整数缩放下行盒与背景平铺取整路径不同，误差逐行累积导致横线穿字
  void getCurrentWebviewWindow()
    .setZoom(1)
    .catch(() => undefined);
  window.addEventListener("wheel", blockZoomWheel, { passive: false });
  window.addEventListener("keydown", blockZoomKeys, true);
  window.addEventListener("unhandledrejection", onUnhandled);
  try {
    await init((m) => (bootMsg.value = m));
    // FR-SAV-01 边界 flush：关闭窗口前强制保存——但保存失败/超时绝不阻塞关窗
    // （journal 机制兜底：未落盘内容下次启动可恢复）
    const win = getCurrentWindow();
    unlistenClose = await win.onCloseRequested(async (e) => {
      e.preventDefault();
      try {
        await Promise.race([flush(), new Promise((r) => setTimeout(r, 3000))]);
      } catch {
        // 保存异常也继续关窗
      }
      try {
        await win.destroy();
      } catch {
        // ignore
      }
      // 兜底：destroy 后进程若因未决 IPC 残留，1.2s 后强制退出（正常路径来不及触发）
      setTimeout(() => {
        void invoke("force_exit");
      }, 1200);
    });
    window.addEventListener("keydown", onKey);
  } catch (e: unknown) {
    const err = e as { code?: string; message?: string };
    bootErr.value = `[${err?.code ?? "error"}] ${err?.message ?? String(e)}`;
  }
});

function onUnhandled(e: PromiseRejectionEvent) {
  if (!s.ready) {
    const r = e.reason as { code?: string; message?: string };
    bootErr.value = `[unhandled] ${r?.message ?? String(e.reason)}`;
  }
}

/** 禁用 Ctrl+滚轮页面缩放（稿纸行线对齐的天敌） */
function blockZoomWheel(e: WheelEvent) {
  if (e.ctrlKey || e.metaKey) e.preventDefault();
}

/** 禁用 Ctrl+加/减/0 缩放热键 */
function blockZoomKeys(e: KeyboardEvent) {
  if ((e.ctrlKey || e.metaKey) && ["+", "-", "=", "0"].includes(e.key)) {
    e.preventDefault();
  }
}

onBeforeUnmount(() => {
  unlistenClose?.();
  window.removeEventListener("keydown", onKey);
  window.removeEventListener("unhandledrejection", onUnhandled);
  window.removeEventListener("wheel", blockZoomWheel);
  window.removeEventListener("keydown", blockZoomKeys, true);
});

async function onKey(e: KeyboardEvent) {
  const ctrl = e.ctrlKey || e.metaKey;
  if (!ctrl) return;
  const k = e.key.toLowerCase();
  if (k === "s") {
    e.preventDefault();
    await saveNow();
  } else if (k === "n") {
    e.preventDefault();
    await createDoc();
  } else if (k === "e") {
    e.preventDefault();
    s.exportOpen = !s.exportOpen;
  }
}
</script>

<template>
  <div v-if="!s.ready" class="boot">
    <template v-if="!bootErr">{{ bootMsg }}</template>
    <template v-else>
      <p class="boot-err">{{ bootMsg }}</p>
      <p class="boot-err detail">{{ bootErr }}</p>
    </template>
  </div>
  <div v-else class="app">
    <!-- 崩溃恢复横幅（FR-SAV-02） -->
    <div v-if="s.pendingRecovery.length" class="recovery">
      <div v-for="p in s.pendingRecovery" :key="p.path" class="recovery-row">
        <span>检测到未保存的崩溃恢复内容：{{ p.title }}</span>
        <button class="btn small" @click="restorePending(p)">恢复</button>
        <button class="btn small ghost" @click="discardPending(p)">丢弃</button>
      </div>
    </div>

    <aside class="sidebar">
      <DocTree />
    </aside>

    <main class="main">
      <EditorPane />
    </main>

    <StatusBar />

    <!-- 撤销式删除通知（FR-DOC-03） -->
    <div v-if="s.toast" class="toast">
      <span>{{ s.toast.msg }}</span>
      <button v-if="s.toast.undo" class="btn small" @click="s.toast.undo?.()">撤销</button>
    </div>
  </div>
</template>
