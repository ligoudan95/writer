import { Mark, mergeAttributes } from "@tiptap/core";

/**
 * AI 续写草稿态 mark（FR-AI-03）：灰/斜体视觉区分。
 * codec 对未知 mark 不序列化（wrapByMarks 白名单），接受时移除 mark 即为正文。
 */
export const AiDraft = Mark.create({
  name: "aiDraft",
  excludes: "",
  renderHTML({ HTMLAttributes }) {
    return ["span", mergeAttributes(HTMLAttributes, { class: "ai-draft" }), 0];
  },
});
