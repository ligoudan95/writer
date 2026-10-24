<script setup lang="ts">
import { ref, watch, onMounted, onBeforeUnmount } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import DocTree from "./components/DocTree.vue";
import EditorPane from "./components/EditorPane.vue";
import StatusBar from "./components/StatusBar.vue";
import ConflictView from "./components/ConflictView.vue";
import DirectionAsk from "./components/DirectionAsk.vue";
import ChatPanel from "./components/ChatPanel.vue";
import SettingsDialog from "./components/SettingsDialog.vue";
import GlobalSearch from "./components/GlobalSearch.vue";
import NotificationCenter from "./components/NotificationCenter.vue";
import TrashPanel from "./components/TrashPanel.vue";
import HelpOverlay from "./components/HelpOverlay.vue";
import { ai } from "./aiflow";
import { initTheme } from "./theme";
import { initLayout } from "./layout";
import { markTyping } from "./notify";
import { scheduleStartupCheck } from "./update";
import {
  s,
  init,
  restorePending,
  discardPending,
  flush,
  saveNow,
  createDoc,
  triggerSync,
  openDoc,
  closeTab,
  tabTitle,
} from "./store";

const bootMsg = ref("正在启动…");
const bootErr = ref("");
const settingsOpen = ref(false);
const settingsTab = ref<"appearance" | "editor" | "ai" | "sync" | "network" | "backup" | "about">("appearance");
const trashOpen = ref(false);
const helpOpen = ref(false);
const searchOpen = ref(false);

/** 打开设置中心（可指定接口配置标签页）。设置项已内嵌为标签页，
 * 旧浮层入口（ai.settingsOpen / s.sync.settingsOpen）在此收敛路由 */
function openSettings(tab?: typeof settingsTab.value) {
  if (tab) settingsTab.value = tab;
  settingsOpen.value = true;
}

watch(
  () => ai.settingsOpen,
  (v) => {
    if (v) {
      ai.settingsOpen = false;
      openSettings("ai");
    }
  },
);
watch(
  () => s.sync.settingsOpen,
  (v) => {
    if (v) {
      s.sync.settingsOpen = false;
      openSettings("sync");
    }
  },
);

let unlistenClose: (() => void) | null = null;

// -- 布局（FR-UI-02）：侧栏/讨论栏可拖宽、可隐藏；记忆 --------------------------------
const sidebarW = ref(Number(localStorage.getItem("writer.w.sidebar")) || 232);
const chatW = ref(Number(localStorage.getItem("writer.w.chat")) || 340);
const sidebarHidden = ref(localStorage.getItem("writer.h.sidebar") === "1");
let dragging: "" | "sidebar" | "chat" = "";

function startDrag(which: "sidebar" | "chat") {
  dragging = which;
}
function onDragMove(e: MouseEvent) {
  if (!dragging) return;
  if (dragging === "sidebar") {
    sidebarW.value = Math.min(460, Math.max(170, e.clientX));
  } else {
    chatW.value = Math.min(560, Math.max(260, window.innerWidth - e.clientX));
  }
}
function endDrag() {
  if (!dragging) return;
  dragging = "";
  localStorage.setItem("writer.w.sidebar", String(sidebarW.value));
  localStorage.setItem("writer.w.chat", String(chatW.value));
}

/** 窗口收窄时重夹栏宽：记忆宽度不得把编辑区压没（最小主区 420px） */
function onResize() {
  const maxChat = window.innerWidth - (sidebarHidden.value ? 0 : sidebarW.value) - 420;
  if (maxChat > 260 && chatW.value > maxChat) chatW.value = Math.floor(maxChat);
  const maxSide = window.innerWidth - 420 - (ai.chatOpen ? 260 : 0);
  if (maxSide > 170 && sidebarW.value > maxSide) sidebarW.value = Math.floor(maxSide);
}
function toggleSidebar() {
  sidebarHidden.value = !sidebarHidden.value;
  localStorage.setItem("writer.h.sidebar", sidebarHidden.value ? "1" : "0");
}

onMounted(async () => {
  initTheme();
  initLayout();
  // 稿纸对齐保障：强制缩放 100% 并禁用页面缩放热键——
  // 非整数缩放下行盒与背景平铺取整路径不同，误差逐行累积导致横线穿字
  void getCurrentWebviewWindow()
    .setZoom(1)
    .catch(() => undefined);
  window.addEventListener("wheel", blockZoomWheel, { passive: false });
  window.addEventListener("keydown", blockZoomKeys, true);
  window.addEventListener("mousemove", onDragMove);
  window.addEventListener("mouseup", endDrag);
  window.addEventListener("resize", onResize);
  window.addEventListener("unhandledrejection", onUnhandled);
  try {
    await init((m) => (bootMsg.value = m));
    // FR-SAV-01 边界 flush + FR-SYN-03 退出提示：关闭前强制保存（失败/超时不阻塞）；
    // 同步批次进行中先非模态提示再关（未完成部分下次启动继续）
    const win = getCurrentWindow();
    unlistenClose = await win.onCloseRequested(async (e) => {
      e.preventDefault();
      try {
        await Promise.race([flush(), new Promise((r) => setTimeout(r, 3000))]);
      } catch {
        // 保存异常也继续关窗
      }
      if (s.sync.running) {
        s.toast = { msg: "同步仍在进行，将中断并在下次启动继续…" };
        await new Promise((r) => setTimeout(r, 1200));
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
    scheduleStartupCheck(); // FR-SET-02：启动检查更新（非模态，写作中不打扰）
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
  window.removeEventListener("mousemove", onDragMove);
  window.removeEventListener("mouseup", endDrag);
  window.removeEventListener("resize", onResize);
});

async function onKey(e: KeyboardEvent) {
  // F1 帮助（FR-UI-07：本地内置）
  if (e.key === "F1") {
    e.preventDefault();
    helpOpen.value = !helpOpen.value;
    return;
  }
  // 输入保护（FR-UI-06）：任何编辑键都刷新"正在输入"时间戳
  markTyping();
  const ctrl = e.ctrlKey || e.metaKey;
  if (!ctrl) {
    // F11 专注模式（V1.1）
    if (e.key === "F11") {
      e.preventDefault();
      s.focusMode = !s.focusMode;
      localStorage.setItem("writer.focus", s.focusMode ? "1" : "0");
    }
    return;
  }
  const k = e.key.toLowerCase();
  if (k === "s") {
    e.preventDefault();
    if (e.shiftKey) {
      await triggerSync(); // Ctrl+Shift+S 手动同步（附录 A）
    } else {
      await saveNow();
    }
  } else if (k === "n") {
    e.preventDefault();
    await createDoc();
  } else if (k === "e") {
    e.preventDefault();
    s.exportOpen = !s.exportOpen;
  } else if (k === "a" && e.shiftKey) {
    // Ctrl+Shift+A 打开 AI 面板（附录 A）
    e.preventDefault();
    void import("./aiflow").then((m) => m.openChat());
  } else if (k === "f" && e.shiftKey) {
    // Ctrl+Shift+F 全文搜索（FR-SEARCH-01）
    e.preventDefault();
    searchOpen.value = !searchOpen.value;
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
  <div v-else class="app" :class="{ focus: s.focusMode }">
    <!-- 崩溃恢复横幅（FR-SAV-02） -->
    <div v-if="s.pendingRecovery.length" class="recovery">
      <div v-for="p in s.pendingRecovery" :key="p.path" class="recovery-row">
        <span>检测到未保存的崩溃恢复内容：{{ p.title }}</span>
        <button class="btn small" @click="restorePending(p)">恢复</button>
        <button class="btn small ghost" @click="discardPending(p)">丢弃</button>
      </div>
    </div>

    <!-- 双引擎检测警告（FR-SYN-01：库位于第三方同步盘内） -->
    <div v-if="s.sync.thirdpartyWarnings.length" class="thirdparty-warn">
      ⚠ 文库位于第三方同步盘（{{ s.sync.thirdpartyWarnings.join("、") }}）范围内，两套同步引擎可能互相冲突——
      建议移动文库位置，或停用其中一方的同步。
      <button class="icon-btn" title="本次忽略" @click="s.sync.thirdpartyWarnings = []">✕</button>
    </div>

    <button
      v-if="sidebarHidden && !s.focusMode"
      class="side-peek icon-btn"
      title="展开文库侧栏"
      @click="toggleSidebar"
    >☰</button>
    <div class="mid">
      <aside v-if="!sidebarHidden && !s.focusMode" class="sidebar" :style="{ width: sidebarW + 'px', flex: 'none' }">
        <DocTree @open-trash="trashOpen = true" @open-settings="openSettings()" />
      </aside>
      <div
        v-if="!sidebarHidden && !s.focusMode"
        class="splitter"
        title="拖动调整宽度"
        @mousedown.prevent="startDrag('sidebar')"
      >
        <button class="split-btn" title="收起侧栏" @click="toggleSidebar">‹</button>
      </div>

      <main class="main">
        <!-- 多标签栏（V1.2，FR-EDT-08） -->
        <div v-if="s.tabs.length && !s.focusMode" class="tabbar">
          <button
            v-for="t in s.tabs"
            :key="t"
            class="tab"
            :class="{ active: s.current?.path === t }"
            :title="t"
            @click="openDoc(t)"
            @mousedown.middle.prevent="closeTab(t)"
          >
            <span class="tab-name">{{ tabTitle(t) }}</span>
            <span
              class="tab-close"
              title="关闭页签"
              @click.stop="closeTab(t)"
            >✕</span>
          </button>
        </div>
        <EditorPane />
      </main>

      <!-- AI 讨论侧栏（FR-AI-04）：可拖宽/关闭 -->
      <template v-if="ai.chatOpen && ai.enabled">
        <div class="splitter" title="拖动调整宽度" @mousedown.prevent="startDrag('chat')"></div>
        <ChatPanel :style="{ width: chatW + 'px', flex: 'none' }" />
      </template>
    </div>

    <!-- 冲突对比视图（FR-SYN-04：非模态，主区上方浮层） -->
    <ConflictView />

    <!-- 首次绑定方向选择（FR-SYN-07：模态白名单） -->
    <DirectionAsk />

    <!-- 设置中心（含 AI 接口 / 云同步 / 网络出口标签页）/ 通知中心 / 回收站 / 帮助（M5） -->
    <SettingsDialog v-if="settingsOpen" :key="settingsTab" :initial-tab="settingsTab" @close="settingsOpen = false" />
    <GlobalSearch v-if="searchOpen" @close="searchOpen = false" />
    <NotificationCenter />
    <TrashPanel v-if="trashOpen" @close="trashOpen = false" />
    <HelpOverlay v-if="helpOpen" @close="helpOpen = false" />

    <StatusBar
      @open-settings="openSettings()"
      @open-trash="trashOpen = true"
      @open-help="helpOpen = true"
    />

    <!-- 撤销式删除通知（FR-DOC-03） -->
    <div v-if="s.toast" class="toast">
      <span>{{ s.toast.msg }}</span>
      <button v-if="s.toast.undo" class="btn small" @click="s.toast.undo?.()">撤销</button>
    </div>
  </div>
</template>
