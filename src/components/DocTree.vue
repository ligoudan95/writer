<script setup lang="ts">
import { s, createDoc, createFolder, importFiles } from "../store";
import TreeList from "./TreeList.vue";

function newDoc() {
  void createDoc();
}
function newFolder() {
  void createFolder();
}
</script>

<template>
  <div class="doctree">
    <div class="doctree-head">
      <span class="doctree-title">文库</span>
      <span class="doctree-actions">
        <button class="icon-btn" title="新建文档 (Ctrl+N)" @click="newDoc">＋文档</button>
        <button class="icon-btn" title="新建文件夹" @click="newFolder">＋夹</button>
        <button class="icon-btn" title="导入 TXT / Markdown" @click="importFiles()">导入</button>
      </span>
    </div>
    <div class="doctree-body">
      <TreeList v-if="s.tree.length" :nodes="s.tree" :depth="0" />
      <div v-else class="doctree-empty">
        文库是空的
        <button class="btn small" @click="newDoc">新建第一篇文档</button>
      </div>
    </div>
    <!-- 回收站常驻入口（FR-DOC-03） -->
    <div class="doctree-foot">回收站 · {{ s.root }}</div>
  </div>
</template>
