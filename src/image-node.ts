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
      // V1.2（FR-EDT-11）：宽度百分比 25-100 / 对齐 l|c|r（方言存于 alt，见 codec）
      width: { default: null, renderHTML: (attrs) => (attrs.width ? { style: `width:${attrs.width}%` } : {}) },
      align: { default: null, renderHTML: () => ({}) },
    };
  },

  parseHTML() {
    return [{ tag: "img[src]" }];
  },

  // V1.2（FR-EDT-11）：对齐渲染——行内节点自身无法设置块级对齐，
  // 以包裹 span 的 display:block + text-align 承载（序列化仍走 alt 方言）
  addNodeView() {
    return (props: { node: { attrs: Record<string, unknown> } }) => {
      const wrap = document.createElement("span");
      const img = document.createElement("img");
      const apply = () => {
        const a = props.node.attrs;
        if (a.src) img.src = String(a.src);
        if (a.alt) img.alt = String(a.alt);
        img.style.width = a.width ? `${a.width}%` : "";
        wrap.style.display = ["l", "c", "r"].includes(String(a.align ?? "")) ? "block" : "";
        wrap.style.textAlign =
          a.align === "c" ? "center" : a.align === "r" ? "right" : a.align === "l" ? "left" : "";
      };
      apply();
      wrap.appendChild(img);
      return {
        dom: wrap,
        update(node: { attrs: Record<string, unknown> }) {
          (props as { node: { attrs: Record<string, unknown> } }).node.attrs = node.attrs;
          apply();
          return true;
        },
      };
    };
  },

  renderHTML({ HTMLAttributes }) {
    return ["img", mergeAttributes(HTMLAttributes)];
  },
});
