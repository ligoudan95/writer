/** 树拖拽跨递归实例共享状态（FR-DOC-02） */
import { reactive } from "vue";
import type { TreeNode } from "./api";

export const dnd = reactive({
  node: null as TreeNode | null,
});
