<script setup lang="ts">
/** 文库侧栏：文档树 + 库菜单（FR-DOC-01/FR-UI-05）+ 回收站常驻入口 + 收藏区/标签过滤（V1.1） */
import { computed, onMounted, onBeforeUnmount, ref } from "vue";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { s, createDoc, createFolder, importFiles, switchLibrary, openDoc } from "../store";
import { api, type TreeNode } from "../api";
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

// -- 收藏区 + 标签过滤（V1.1：frontmatter 元数据随文件同步） -----------------------
function collectStarred(nodes: TreeNode[], out: TreeNode[]) {
  for (const n of nodes) {
    if (n.kind === "doc" && n.starred) out.push(n);
    if (n.children?.length) collectStarred(n.children, out);
  }
}
const starred = computed(() => {
  const out: TreeNode[] = [];
  collectStarred(s.tree, out);
  return out;
});

function collectTags(nodes: TreeNode[], out: Set<string>) {
  for (const n of nodes) {
    if (n.kind === "doc") n.tags?.forEach((t) => out.add(t));
    if (n.children?.length) collectTags(n.children, out);
  }
}
const allTags = computed(() => {
  const set = new Set<string>();
  collectTags(s.tree, set);
  return [...set].sort();
});
const tagFilter = ref("");
function pickTag(t: string) {
  tagFilter.value = tagFilter.value === t ? "" : t;
}
/** 按标签过滤树（保留含命中文档的文件夹路径） */
function filterByTag(nodes: TreeNode[], tag: string): TreeNode[] {
  const out: TreeNode[] = [];
  for (const n of nodes) {
    if (n.kind === "folder") {
      const kids = filterByTag(n.children ?? [], tag);
      if (kids.length) out.push({ ...n, children: kids });
    } else if (n.tags?.includes(tag)) {
      out.push(n);
    }
  }
  return out;
}
const shownTree = computed(() =>
  tagFilter.value ? filterByTag(s.tree, tagFilter.value) : s.tree,
);
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
      <!-- 收藏区（V1.1，置顶常显） -->
      <div v-if="starred.length" class="starred-sec">
        <div class="starred-label">★ 收藏</div>
        <button
          v-for="n in starred"
          :key="'s-' + n.id"
          class="starred-item ellip"
          :class="{ active: s.current?.path === n.id }"
          :title="n.name"
          @click="openDoc(n.id)"
        >★ {{ n.name }}</button>
      </div>
      <!-- 标签过滤（V1.1） -->
      <div v-if="allTags.length" class="tag-filter">
        <button
          v-for="t in allTags.slice(0, 12)"
          :key="t"
          class="tree-tag clickable"
          :class="{ on: tagFilter === t }"
          @click="pickTag(t)"
        >{{ t }}</button>
      </div>
      <TreeList v-if="shownTree.length" :nodes="shownTree" :depth="0" />
      <div v-else-if="tagFilter" class="doctree-empty">没有「{{ tagFilter }}」标签的文档</div>
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
