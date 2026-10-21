<script setup lang="ts">
/** 文库侧栏：文档树 + 库菜单（打开/新建/最近库，FR-DOC-01/FR-UI-05）+ 回收站常驻入口 */
import { onMounted, onBeforeUnmount, ref } from "vue";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { s, createDoc, createFolder, importFiles, switchLibrary } from "../store";
import { api } from "../api";
import TreeList from "./TreeList.vue";

const emit = defineEmits<{ (e: "open-trash"): void; (e: "open-settings"): void }>();

const libMenuOpen = ref(false);
const recent = ref<string[]>([]);

function toggleMenu() {
  libMenuOpen.value = !libMenuOpen.value;
  if (libMenuOpen.value) void api.libraryRecentList().then((r) => (recent.value = r));
}

/** 点击树外任意位置关闭库菜单（编辑器/状态栏等） */
function onDocClick(e: MouseEvent) {
  const menu = document.querySelector(".lib-menu");
  if (libMenuOpen.value && menu && !menu.contains(e.target as Node)) {
    libMenuOpen.value = false;
  }
}

async function pickAndSwitch(create: boolean) {
  libMenuOpen.value = false;
  const picked = await openDialog({ directory: true, multiple: false, title: create ? "选择新建文库的位置" : "选择要打开的文库文件夹" });
  if (!picked || Array.isArray(picked)) return;
  try {
    await switchLibrary(picked, create);
  } catch (e) {
    s.toast = { msg: `打开/新建失败：${(e as { message?: string }).message ?? e}` };
    setTimeout(() => (s.toast = null), 5000);
  }
}

async function openRecent(path: string) {
  libMenuOpen.value = false;
  try {
    await switchLibrary(path, false);
  } catch (e) {
    s.toast = { msg: `打开失败：${(e as { message?: string }).message ?? e}` };
    setTimeout(() => (s.toast = null), 5000);
  }
}

function newDoc() {
  void createDoc();
}
function newFolder() {
  void createFolder();
}
onMounted(() => {
  document.addEventListener("click", onDocClick);
  void api.libraryRecentList().then((r) => (recent.value = r));
});
onBeforeUnmount(() => document.removeEventListener("click", onDocClick));
</script>

<template>
  <div class="doctree">
    <div class="doctree-head">
      <button class="icon-btn" title="文库菜单（打开 / 新建 / 最近）" @click="toggleMenu">☰</button>
      <span class="doctree-title">文库</span>
      <span class="doctree-actions">
        <button class="icon-btn" title="新建文档 (Ctrl+N)" @click="newDoc">＋文档</button>
        <button class="icon-btn" title="新建文件夹" @click="newFolder">＋夹</button>
        <button class="icon-btn" title="导入 TXT / Markdown / DOCX" @click="importFiles()">导入</button>
      </span>
    </div>

    <div v-if="libMenuOpen" class="lib-menu" @click.stop>
      <button class="lib-item" @click="pickAndSwitch(false)">打开其他文库…</button>
      <button class="lib-item" @click="pickAndSwitch(true)">新建文库…</button>
      <div v-if="recent.length" class="lib-sep"></div>
      <div v-if="recent.length" class="lib-label">最近打开</div>
      <button v-for="r in recent" :key="r" class="lib-item ellip" @click="openRecent(r)">{{ r }}</button>
    </div>

    <div class="doctree-body" @click="libMenuOpen = false">
      <TreeList v-if="s.tree.length" :nodes="s.tree" :depth="0" />
      <div v-else class="doctree-empty">
        文库是空的
        <button class="btn small" @click="newDoc">新建第一篇文档</button>
      </div>
    </div>
    <!-- 回收站常驻入口（FR-DOC-03）+ 设置入口（FR-SET-01） -->
    <div class="doctree-foot">
      <button class="foot-btn" title="回收站" @click="emit('open-trash')">🗑 回收站</button>
      <span class="foot-path">{{ s.root }}</span>
      <button class="foot-btn" title="设置" @click="emit('open-settings')">⚙</button>
    </div>
  </div>
</template>
