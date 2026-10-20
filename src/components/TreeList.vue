<script setup lang="ts">
import { ref } from "vue";
import { s, openDoc, deleteNode, renameDoc, renameFolder } from "../store";
import type { TreeNode } from "../api";

const props = defineProps<{ nodes: TreeNode[]; depth: number }>();

const editing = ref<{ id: string; value: string } | null>(null);

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
</script>

<template>
  <div class="treelist">
    <div v-for="node in nodes" :key="node.id" class="treeitem-wrap">
      <div
        class="treeitem"
        :class="{
          active: node.kind === 'doc' && s.current?.path === node.id,
          folder_open: node.kind === 'folder' && s.selectedFolder === node.id,
        }"
        :style="{ paddingLeft: 8 + depth * 14 + 'px' }"
        @click="select(node)"
        @dblclick="startRename(node)"
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
