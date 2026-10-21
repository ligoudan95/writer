<script setup lang="ts">
/** 文档树：选中/重命名/删除 + 拖拽（FR-DOC-02：拖到文件夹=移动；同级上下=排序，步长10） */
import { reactive, ref } from "vue";
import { s, openDoc, deleteNode, renameDoc, renameFolder, refreshTree } from "../store";
import { api, type TreeNode } from "../api";
import { dnd } from "../dnd";

const props = defineProps<{ nodes: TreeNode[]; depth: number }>();

const editing = ref<{ id: string; value: string } | null>(null);

/** 拖放目标指示：{ id, mode: into | before | after } */
const drop = reactive<{ id: string; mode: "" | "into" | "before" | "after" }>({ id: "", mode: "" });

/** 逐文档同步标记（FR-SYN-03）：冲突 > 待同步 > 已同步（已同步不占视觉） */
function syncMark(path: string): { cls: string; icon: string; tip: string } | null {
  if (s.sync.conflicts.some((c) => c.path === path || c.copyPath === path)) {
    return { cls: "conflict", icon: "⚠", tip: "同步冲突待处理" };
  }
  const st = s.sync.docStates[path];
  if (st === "pending") return { cls: "pending", icon: "●", tip: "待同步" };
  return null;
}

function select(node: TreeNode) {
  if (node.kind === "folder") {
    s.selectedFolder = s.selectedFolder === node.id ? "" : node.id;
  } else {
    void openDoc(node.id);
  }
}

function startRename(node: TreeNode) {
  editing.value = { id: node.id, value: node.name };
}

function commitRename(node: TreeNode) {
  const val = editing.value?.value?.trim();
  editing.value = null;
  if (!val || val === node.name) return;
  if (node.kind === "doc") void renameDoc(node.id, val);
  else void renameFolder(node.id, val);
}

function onEnter(node: TreeNode, e: KeyboardEvent) {
  if (e.key === "Enter") commitRename(node);
  if (e.key === "Escape") editing.value = null;
}

// -- 拖拽（FR-DOC-02） -----------------------------------------------------------

const parentOf = (id: string) => {
  const i = id.lastIndexOf("/");
  return i < 0 ? "" : id.slice(0, i);
};

function onDragStart(node: TreeNode, e: DragEvent) {
  dnd.node = node;
  if (e.dataTransfer) {
    e.dataTransfer.effectAllowed = "move";
    e.dataTransfer.setData("text/plain", node.id); // 兼容性占位
  }
}

function onDragOver(node: TreeNode, e: DragEvent) {
  if (!dnd.node || dnd.node.id === node.id) return;
  const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
  const upper = e.clientY - r.top < r.height / 2;
  if (node.kind === "folder") {
    // 文件夹：整体移入；同父文件夹排序意义有限（文件夹恒排前），不做半区
    drop.id = node.id;
    drop.mode = "into";
  } else {
    const sameParent = parentOf(dnd.node.id) === parentOf(node.id) && dnd.node.kind === "doc";
    drop.id = node.id;
    drop.mode = sameParent ? (upper ? "before" : "after") : "into"; // 异父文档上 → 移入其所在文件夹
  }
  e.preventDefault();
  if (e.dataTransfer) e.dataTransfer.dropEffect = "move";
}

function onDragLeave() {
  drop.id = "";
  drop.mode = "";
}

async function onDrop(node: TreeNode, e: DragEvent) {
  e.preventDefault();
  const src = dnd.node;
  const mode = drop.mode;
  dnd.node = null;
  drop.id = "";
  drop.mode = "";
  if (!src || !mode) return;

  try {
    if (mode === "into") {
      // 文件夹目标 = 移入；文档目标 = 移入其所在文件夹（与高亮语义一致，不得静默无效）
      const destFolder = node.kind === "folder" ? node.id : parentOf(node.id); // "" = docs 根
      if (node.kind === "folder" && (node.id === src.id || node.id.startsWith(src.id + "/"))) {
        return; // 不能移入自身内部
      }
      if (src.kind === "doc") {
        const np = await api.moveDoc(src.id, destFolder);
        if (s.current?.path === src.id) s.current = { ...s.current, path: np };
      } else if (node.kind === "folder") {
        await api.moveFolder(src.id, node.id);
        // 被移动文件夹内的当前文档：路径已变，关闭（下次从树重新打开）
        if (s.current?.path.startsWith(src.id + "/")) {
          void import("../store").then((m) => m.closeCurrentDocForTree());
        }
      } else {
        return; // 文件夹拖到文档上（非文件夹目标）：文件夹不进文件
      }
    } else {
      // 同级排序：目标顺序 = 兄弟文档列表去掉源后插入到目标前/后
      const folder = parentOf(node.id);
      const siblings = props.nodes.filter((n) => n.kind === "doc").map((n) => n.id);
      const list = siblings.filter((id) => id !== src.id);
      const idx = list.indexOf(node.id);
      if (idx < 0) return;
      list.splice(mode === "before" ? idx : idx + 1, 0, src.id);
      const newPaths = await api.reorderDocs(folder, list);
      // 打开中的文档路径跟随
      const map = new Map<string, string>();
      list.forEach((old, i) => map.set(old, newPaths[i]));
      if (s.current && map.has(s.current.path)) {
        s.current = { ...s.current, path: map.get(s.current.path)! };
      }
    }
    await refreshTree();
  } catch (err) {
    s.toast = { msg: `移动/排序失败：${(err as { message?: string }).message ?? err}` };
    setTimeout(() => {
      if (s.toast?.msg.startsWith("移动/排序失败")) s.toast = null;
    }, 5000);
  }
}
</script>

<template>
  <div class="treelist">
    <div v-for="node in nodes" :key="node.id" class="treeitem-wrap">
      <div
        class="treeitem"
        :class="{
          active: node.kind === 'doc' && s.current?.path === node.id,
          folder_open: node.kind === 'folder' && s.selectedFolder === node.id,
          dragging: dnd.node?.id === node.id,
          'drop-target': drop.id === node.id && drop.mode === 'into',
          'drop-line': drop.id === node.id && (drop.mode === 'before' || drop.mode === 'after'),
        }"
        :style="{ paddingLeft: 8 + depth * 14 + 'px' }"
        draggable="true"
        @click="select(node)"
        @dblclick="startRename(node)"
        @dragstart="onDragStart(node, $event)"
        @dragover="onDragOver(node, $event)"
        @dragleave="onDragLeave"
        @drop="onDrop(node, $event)"
        @dragend="dnd.node = null"
      >
        <span class="treeitem-icon">{{ node.kind === "folder" ? "▸" : node.privacy ? "🔒" : "📄" }}</span>
        <input
          v-if="editing?.id === node.id"
          v-model="editing.value"
          class="treeitem-edit"
          @keydown="onEnter(node, $event)"
          @blur="commitRename(node)"
          @click.stop
          autofocus
        />
        <span v-else class="treeitem-name" :title="node.name">{{ node.name }}</span>
        <span
          v-if="node.kind === 'doc' && syncMark(node.id)"
          class="treeitem-sync"
          :class="syncMark(node.id)!.cls"
          :title="syncMark(node.id)!.tip"
          >{{ syncMark(node.id)!.icon }}</span
        >
        <span class="treeitem-ops" @click.stop>
          <button class="icon-btn dim" title="重命名" @click="startRename(node)">✎</button>
          <button class="icon-btn dim" title="移入回收站" @click="deleteNode(node)">✕</button>
        </span>
      </div>
      <TreeList
        v-if="node.kind === 'folder' && node.children?.length"
        :nodes="node.children"
        :depth="depth + 1"
      />
    </div>
  </div>
</template>
