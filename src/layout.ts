/**
 * 编辑区排版 token 单源（FR-UI-03）：字体/字号/行距/段间距/页宽/首行缩进。
 * 编辑区 CSS 变量与导出模板（PDF/DOCX 默认排版）同源消费，保证"所见即所得"。
 * 应用级默认存 localStorage（应用作用域，跨库共享）；文档级覆盖经 frontmatter layout 键。
 * 注意（v1.7 定案）：缩进与段间距属排版型属性，仅作用于 PDF/DOCX 导出与屏幕显示，
 * 不改写存储正文（Tab 缩进以真实字符存储的路径不受此影响）。
 */
import { reactive, watch } from "vue";

export interface LayoutTokens {
  fontFamily: string;
  fontSize: number; // px
  lineHeight: number; // 倍数
  paraGap: number; // px（段间距，仅显示与排版型导出）
  pageWidth: number; // px（0 = 全宽）
  indent: boolean; // 首行缩进（显示层；PDF/DOCX 导出生效）
}

export const DEFAULT_LAYOUT: LayoutTokens = {
  fontFamily: '"Microsoft YaHei", "PingFang SC", sans-serif',
  fontSize: 17,
  lineHeight: 1.9,
  paraGap: 10,
  pageWidth: 780,
  indent: false,
};

const KEY = "writer.layout";

function load(): LayoutTokens {
  try {
    const raw = localStorage.getItem(KEY);
    if (raw) return { ...DEFAULT_LAYOUT, ...JSON.parse(raw) };
  } catch {
    /* 损坏配置回默认 */
  }
  return { ...DEFAULT_LAYOUT };
}

export const layout = reactive<LayoutTokens>(load());

export const FONT_CANDIDATES = [
  { id: '"Microsoft YaHei", sans-serif', name: "微软雅黑" },
  { id: '"SimSun", serif', name: "宋体" },
  { id: '"KaiTi", serif', name: "楷体" },
  { id: '"SimHei", sans-serif', name: "黑体" },
  { id: '"Source Han Sans SC", "思源黑体", sans-serif', name: "思源黑体" },
  { id: '"LXGW WenKai", "霞鹜文楷", serif', name: "霞鹜文楷（需自装）" },
  { id: 'Georgia, "Times New Roman", serif', name: "Georgia / 西文衬线" },
];

function apply() {
  const el = document.documentElement;
  el.style.setProperty("--ed-font", layout.fontFamily);
  el.style.setProperty("--ed-size", `${layout.fontSize}px`);
  el.style.setProperty("--ed-line-height", String(layout.lineHeight));
  el.style.setProperty("--ed-para-gap", `${layout.paraGap}px`);
  el.style.setProperty("--ed-page-width", layout.pageWidth > 0 ? `${layout.pageWidth}px` : "100%");
  el.dataset.edIndent = layout.indent ? "1" : "0";
}

export function initLayout() {
  apply();
  watch(
    layout,
    () => {
      apply();
      localStorage.setItem(KEY, JSON.stringify({ ...layout }));
    },
    { deep: true },
  );
}

/** 导出模板消费（exportHtml/DOCX）：屏幕 token 直接作为导出默认排版 */
export function exportTokens(): LayoutTokens {
  return { ...layout };
}
