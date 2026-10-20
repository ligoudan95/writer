import { reactive } from "vue";
import { api, type DocPayload, type SnapshotInfo, type TreeNode } from "./api";
import { docToMd, mdToDoc, type PMNode } from "./codec";

export type SaveState = "saved" | "dirty" | "saving" | "error" | "conflict";

export const s = reactive({
  ready: false,
  root: "",
  tree: [] as TreeNode[],
  selectedFolder: "",
  current: null as DocPayload | null,
  lastReadHash: "",
  saveState: "saved" as SaveState,
  saveError: "",
  savedAt: 0,
  counts: { words: 0, chars: 0, paragraphs: 0 },
  pendingRecovery: [] as DocPayload[],
  toast: null as { msg: string; undo?: () => void } | null,
  history: {
    open: false,
    list: [] as SnapshotInfo[],
    preview: "",
    previewTs: 0,
  },
  exportOpen: false,
});

// 编辑器桥：EditorPane 挂载后注入内容存取器（store 不依赖 TipTap）
let editorGetJson: () => PMNode | null = () => null;
export function bindEditor(fn: () => PMNode | null) {
  editorGetJson = fn;
}

let saveTimer: number | undefined;

/** FR-SAV-01：停止输入 2s 后自动保存 */
export function scheduleSave() {
  s.saveState = "dirty";
  clearTimeout(saveTimer);
  saveTimer = window.setTimeout(() => void saveNow(), 2000);
}

export async function saveNow(): Promise<boolean> {
  const cur = s.current;
  if (!cur || s.saveState === "saving") return true;
  const json = editorGetJson();
  if (!json) return true;
  if (s.saveState === "saved") return true;
  const body = docToMd(json);
  s.saveState = "saving";
  try {
    const r = await api.writeDoc(cur.path, body, s.lastReadHash);
    s.lastReadHash = r.hash;
    s.savedAt = r.savedAt;
    s.saveState = "saved";
    s.saveError = "";
    return true;
  } catch (e: unknown) {
    const err = e as { code?: string; message?: string };
    if (err?.code === "conflict") {
      s.saveState = "conflict";
      s.saveError = "文件已被外部修改，已阻止覆盖（FR-SYN-09 护栏）";
    } else {
      s.saveState = "error";
      s.saveError = err?.message ?? String(e);
    }
    return false;
  }
}

/** FR-SAV-01 边界时刻强制 flush：切换文档 / 关闭窗口 / 导出前调用 */
export async function flush(): Promise<boolean> {
  clearTimeout(saveTimer);
  if (s.saveState === "dirty") return saveNow();
  if (s.saveState === "saving") {
    await new Promise((r) => setTimeout(r, 50));
    return saveNow();
  }
  if (s.saveState === "conflict") return false;
  return true;
}

export async function refreshTree() {
  s.tree = await api.list();
}

/** 打开文档（先 flush；冲突时中止切换并提示）。force=true 跳过 flush——用于"重新加载磁盘版本"按钮：用户已明确选择放弃本地未保存修改 */
export async function openDoc(path: string, force = false) {
  const ok = force ? true : await flush();
  if (!ok) return;
  const d = await api.readDoc(path);
  s.current = d;
  s.lastReadHash = d.hash;
  s.saveState = "saved";
  s.saveError = "";
}

export async function createDoc(folder = s.selectedFolder) {
  const node = await api.createDoc(folder, "无标题");
  await refreshTree();
  await openDoc(node.id);
}

export async function renameDoc(path: string, title: string) {
  const t = title.trim();
  if (!t) return;
  if (s.current?.path === path) await flush();
  const r = await api.renameDoc(path, t);
  await refreshTree();
  if (s.current?.path === path && s.current) {
    s.current = { ...s.current, path: r.path, title: t };
    // rename 重写了文件（frontmatter title 变更），必须采纳新 hash，
    // 否则下一轮自动保存会触发假冲突（FR-SYN-09 护栏误判）
    s.lastReadHash = r.hash;
  }
}

/** FR-DOC-03 撤销式删除：无确认弹窗，5 秒内可撤销 */
export async function deleteNode(node: TreeNode) {
  const backup = await api.deleteNode(node.id);
  if (s.current?.path === node.id) {
    await flush().catch(() => undefined);
    s.current = null;
  }
  if (s.selectedFolder === node.id) s.selectedFolder = "";
  await refreshTree();
  const msg = `已移入回收站：${node.name}`;
  s.toast = {
    msg,
    undo: () => {
      void (async () => {
        await api.restoreFromTrash(backup, node.id);
        await refreshTree();
        s.toast = null;
      })();
    },
  };
  setTimeout(() => {
    if (s.toast?.msg === msg) s.toast = null;
  }, 5000);
}

export async function createFolder(parent = s.selectedFolder) {
  const name = `新文件夹${s.tree.length ? "" : ""}`;
  const node = await api.createFolder(parent, name);
  await refreshTree();
  s.selectedFolder = node.id;
}

export async function renameFolder(path: string, name: string) {
  const t = name.trim();
  if (!t) return;
  await flush().catch(() => undefined);
  await api.renameFolder(path, t);
  // 当前打开文档若在被改名的文件夹内，路径已变：关闭并刷新
  if (s.current?.path.startsWith(path + "/") || s.current?.path === path) {
    s.current = null;
  }
  if (s.selectedFolder === path) s.selectedFolder = "";
  await refreshTree();
}

export async function restorePending(p: DocPayload) {
  await api.writeDoc(p.path, p.body, "", true);
  s.pendingRecovery = s.pendingRecovery.filter((x) => x.path !== p.path);
  await refreshTree();
  if (s.current?.path === p.path) await openDoc(p.path);
}

export function discardPending(p: DocPayload) {
  s.pendingRecovery = s.pendingRecovery.filter((x) => x.path !== p.path);
}

// ---------------------------------------------------------------------------
// 版本历史（FR-SAV-03：每小时自动 + 手动快照 + 回滚前自动备份）
// ---------------------------------------------------------------------------

export async function openHistory() {
  s.history.open = true;
  await reloadHistory();
}

export async function reloadHistory() {
  if (!s.current) return;
  s.history.list = await api.snapshotList(s.current.path);
}

export async function closeHistory() {
  s.history.open = false;
  s.history.preview = "";
  s.history.previewTs = 0;
}

export async function manualSnapshot() {
  if (!s.current) return;
  await flush();
  await api.snapshotCreate(s.current.path, "manual", "手动快照");
  await reloadHistory();
}

export async function previewSnapshot(ts: number) {
  if (!s.current) return;
  s.history.preview = await api.snapshotRead(s.current.path, ts);
  s.history.previewTs = ts;
}

export async function restoreSnapshot(ts: number) {
  if (!s.current) return;
  await api.snapshotRestore(s.current.path, ts);
  await reloadHistory();
  await openDoc(s.current.path, true);
}

// ---------------------------------------------------------------------------
// 导入导出（FR-EXP / FR-IMP，M2）
// ---------------------------------------------------------------------------

export function openExport() {
  s.exportOpen = true;
}

export function closeExport() {
  s.exportOpen = false;
}

/** 编辑器当前内容（供导出器取 DocModel） */
export function currentJson(): PMNode | null {
  return editorGetJson();
}

/** 导入文件（FR-IMP-01：TXT/MD，TXT 转为空行分段） */
export async function importFiles() {
  const { open } = await import("@tauri-apps/plugin-dialog");
  const picked = (await open({
    multiple: true,
    filters: [{ name: "文本文档", extensions: ["txt", "md", "markdown"] }],
  })) as string[] | string | null;
  if (!picked) return;
  const list = Array.isArray(picked) ? picked : [picked];
  let n = 0;
  for (const p of list) {
    try {
      const f = await api.importRead(p);
      const body = /\.txt$/i.test(f.name) ? txtToBody(f.content) : f.content;
      await api.importDoc(s.selectedFolder, f.name, body);
      n++;
    } catch {
      // 单个失败不中断整批
    }
  }
  await refreshTree();
  const msg = `已导入 ${n} 篇文档`;
  s.toast = { msg };
  setTimeout(() => {
    if (s.toast?.msg === msg) s.toast = null;
  }, 3000);
}

function txtToBody(t: string): string {
  const paras = t
    .split(/\r?\n/)
    .map((l) => l.trim())
    .filter(Boolean);
  return paras.length ? paras.join("\n\n") + "\n" : "";
}

function firstDoc(nodes: TreeNode[]): TreeNode | null {
  for (const n of nodes) {
    if (n.kind === "doc") return n;
    const r = n.children?.length ? firstDoc(n.children) : null;
    if (r) return r;
  }
  return null;
}

/** FR-UI-04：直达上次会话（M1 = 打开第一篇）；onStep 用于启动屏进度诊断 */
export async function init(onStep?: (msg: string) => void) {
  onStep?.("正在打开文库…");
  const info = await api.startup();
  s.root = info.root;
  s.pendingRecovery = info.pending;
  onStep?.("读取文档树…");
  await refreshTree();
  const first = firstDoc(s.tree);
  if (first) {
    onStep?.("打开文档…");
    await openDoc(first.id);
  }
  s.ready = true;
}

/** 字数统计（FR-EDT-07 计数规则：CJK 逐字计，西文数字串计 1） */
export function updateCounts(json: PMNode) {
  let text = "";
  let paragraphs = 0;
  const walk = (n: PMNode) => {
    if (n.type === "paragraph") paragraphs++;
    if (n.text) text += n.text + "\n";
    n.content?.forEach(walk);
  };
  walk(json);
  const words =
    (text.match(/[\p{Script=Han}\p{Script=Hiragana}\p{Script=Katakana}]|[A-Za-z0-9]+/gu) ?? [])
      .length;
  s.counts.words = words;
  s.counts.chars = text.replace(/\n/g, "").length;
  s.counts.paragraphs = paragraphs;
}

export { mdToDoc };
