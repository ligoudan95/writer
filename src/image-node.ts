/**
 * 图片节点（FR-EDT-01：image 为方言成员）
 * inline 形态与 codec 一致：块级图片由"仅含图片的段落"承载，schema 只需 inline 组。
 * src 消毒在 codec 入口（safeUrl 白名单）完成，此处仅负责渲染。
 */
import { Node, mergeAttributes } from "@tiptap/core";

export const ImageNode = Node.create({
  name: "image",
  inline: true,
  group: "inline",
  draggable: true,

  addAttributes() {
    return {
      src: { default: null },
      alt: { default: null },
      title: { default: null },
    };
  },

  parseHTML() {
    return [{ tag: "img[src]" }];
  },

  renderHTML({ HTMLAttributes }) {
    return ["img", mergeAttributes(HTMLAttributes)];
  },
});
