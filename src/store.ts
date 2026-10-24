import { reactive } from "vue";
import {
  api,
  type DocPayload,
  type SnapshotInfo,
  type SyncConflict,
  type SyncLogRow,
  type SyncReport,
  type TreeNode,
} from "./api";
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
  counts: { words: 0, chars: 0, paragraphs: 0, selWords: 0, selChars: 0 },
  pendingRecovery: [] as DocPayload[],
  toast: null as { msg: string; undo?: () => void } | null,
  history: {
    open: false,
    list: [] as SnapshotInfo[],
    preview: "",
    previewTs: 0,
  },
  exportOpen: false,
  /** 远端更新重载信号（FR-SYN-09）：path 不变时驱动编辑器重应用内容 */
  reloadNonce: 0,
  /** 全文搜索定位词（FR-SEARCH-01）：打开文档后由编辑器消费一次（光标+滚动到命中） */
  pendingLocate: "",
  /** 专注模式（V1.1，F11）：隐藏侧栏/工具栏，状态栏悬停浮现 */
  focusMode: localStorage.getItem("writer.focus") === "1",
  /** 多标签（V1.2，FR-EDT-08）：打开的文档页签（path 列表，当前为 current.path） */
  tabs: [] as string[],
  /** 分屏对照（V1.3，FR-EDT-08 P2）：右侧只读对照文档路径（null = 关闭） */
  splitDoc: null as string | null,
  // 云同步（FR-SYN，M3）
  sync: {
    phase: "idle" as string, // idle | scanning | planning | executing | settling
    running: false,
    last: null as SyncReport | null,
    conflicts: [] as SyncConflict[],
    history: [] as SyncLogRow[],
    historyOpen: false,
    settingsOpen: false,
    /** 保存后自动同步开关（FR-SYN-03，设置面板可关；scheduleAutoSync 消费） */
    autoSync: true,
    // 首次绑定方向对话框（FR-SYN-07）
    directionAsk: null as { localDocs: number; remoteDocs: number } | null,
    thirdpartyWarnings: [] as string[],
    /** 逐文档同步状态（FR-SYN-03 树图标数据源）：path → synced | pending */
    docStates: {} as Record<string, string>,
  },
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

// FR-SAV-01：每 30s 兜底保存（长句斟酌中不触发 2s 去抖的连续输入场景）
window.setInterval(() => {
  if (s.saveState === "dirty") void saveNow();
}, 30_000);

/** 取消挂起的 2s 去抖保存（AI 续写触发前调用，防未接受草稿随之落盘） */
export function cancelScheduledSave() {
  clearTimeout(saveTimer);
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
    // await 期间文档可能已被切换（openDoc→flush 早退放行）：本次写入已成功落盘，
    // 但 hash/状态归属新文档的管线，回写旧 hash 会让新文档下一轮保存假冲突
    if (s.current?.path !== cur.path) return true;
    s.lastReadHash = r.hash;
    s.savedAt = r.savedAt;
    const stateAfterSave: string = s.saveState; // await 期间可能被并发置 dirty（TS 收窄失效场景）
    if (stateAfterSave === "dirty") {
      // 保存期间又有新输入/AI 接受（scheduleAiSave 置 dirty）：不得回写 saved 吞掉
      // 未落盘内容——保持 dirty 并立即补一轮（否则接受草稿后关窗即丢稿）
      saveTimer = window.setTimeout(() => void saveNow(), 100);
      return true;
    }
    s.saveState = "saved";
    s.saveError = "";
    scheduleAutoSync(); // FR-SYN-03：保存后 10s 去抖自动同步
    return true;
  } catch (e: unknown) {
    // 失败状态同样不得落到已切换的新文档上
    if (s.current?.path !== cur.path) return false;
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
    // 等待在途保存真正结束（本地 IO 必然收敛）。不得改调 saveNow——
    // 它对 saving 态早退 true，会让 flush 假装完成，放行文档切换造成 hash 串档
    await new Promise((r) => setTimeout(r, 50));
    return flush();
  }
  if (s.saveState === "conflict") return false;
  return true;
}

export async function refreshTree() {
  s.tree = await api.list();
  refreshDocStates();
}

/** 逐文档同步状态（FR-SYN-03）；同步未启用时为空，不影响树渲染 */
function refreshDocStates() {
  void api
    .syncDocStates()
    .then((rows) => {
      const m: Record<string, string> = {};
      for (const r of rows) m[r.path] = r.state;
      s.sync.docStates = m;
    })
    .catch(() => undefined);
}

/** 打开文档（先 flush；冲突时中止切换并提示）。force=true 跳过 flush——用于"重新加载磁盘版本"按钮：用户已明确选择放弃本地未保存修改 */
export async function openDoc(path: string, force = false) {
  const ok = force ? true : await flush();
  if (!ok) return;
  // 多标签（FR-EDT-08）：登记页签（去重保序，当前文档置顶活动）
  if (!s.tabs.includes(path)) s.tabs.push(path);
  // 切换文档：清理在跑/挂起的 AI 流程（A 文档的草稿区间/润色区间不得写进 B 文档）
  void import("./aiflow").then((m) => m.resetAiForDocSwitch());
  const sameDoc = s.current?.path === path;
  const d = await api.readDoc(path);
  s.current = d;
  s.lastReadHash = d.hash;
  s.saveState = "saved";
  s.saveError = "";
  // 同路径重载（冲突护栏"重新加载磁盘版本"）：path watch 不触发，用 nonce 驱动编辑器重应用
  if (sameDoc) s.reloadNonce++;
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

/** 关闭当前文档的统一入口：先清 AI 流程（A 文档草稿/润色区间不得写坏后续文档），
 * 再丢弃 current。删除/文件夹改名/远端删除传播等所有 s.current=null 路径共用 */
function closeCurrentDoc() {
  void import("./aiflow").then((m) => m.resetAiForDocSwitch());
  if (s.current) closeTab(s.current.path, false);
  s.current = null;
}

/** 关闭页签（V1.2）：activateNext=true 时切到相邻页签（用户点 ✕ 场景） */
export function closeTab(path: string, activateNext = true) {
  const idx = s.tabs.indexOf(path);
  s.tabs = s.tabs.filter((p) => p !== path);
  if (activateNext && s.current?.path === path && s.tabs.length) {
    const next = s.tabs[Math.min(idx, s.tabs.length - 1)];
    void openDoc(next);
  }
}

/** 页签显示名：树中标题，树缺失时回退文件名 */
export function tabTitle(path: string): string {
  const find = (nodes: TreeNode[]): string | null => {
    for (const n of nodes) {
      if (n.id === path) return n.name;
      if (n.children) {
        const r = find(n.children);
        if (r) return r;
      }
    }
    return null;
  };
  return find(s.tree) ?? path.split("/").pop()?.replace(/\.md$/, "") ?? path;
}

/** 树拖拽等外部场景调用的公开包装 */
export function closeCurrentDocForTree() {
  closeCurrentDoc();
}

/** FR-DOC-03 撤销式删除：单篇无确认弹窗 5 秒可撤销；文件夹走模态确认（白名单） */
export async function deleteNode(node: TreeNode) {
  if (node.kind === "folder") {
    const { confirm } = await import("@tauri-apps/plugin-dialog");
    const ok = await confirm(
      `将把文件夹「${node.name}」及其全部内容移入回收站，确定删除？`,
      { title: "删除文件夹" },
    );
    if (!ok) return;
  }
  const backup = await api.deleteNode(node.id);
  // 当前打开文档位于被删文件夹内（含精确命中）：先 flush 再关闭，
  // 否则编辑器悬挂在回收站路径上，自动保存持续报错
  const inDeleted =
    s.current?.path === node.id ||
    (node.kind === "folder" && !!s.current?.path.startsWith(node.id + "/"));
  if (inDeleted) {
    await flush().catch(() => undefined);
    closeCurrentDoc();
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
    // 撤销窗口到期：冲刷因撤销保护而排队待显的通知
    void import("./notify").then((m) => m.onToastSlotFreed());
  }, 5000);
}

/** 灵感便签（V1.3）：快速一段文字入库 docs/便签/，不打断当前编辑 */
export async function createNote(text: string) {
  const t = text.trim();
  if (!t) return;
  try {
    await api.createFolder("", "便签");
  } catch {
    /* 已存在 */
  }
  const time = new Date();
  const stamp = `${time.getMonth() + 1}${String(time.getDate()).padStart(2, "0")}-${String(time.getHours()).padStart(2, "0")}${String(time.getMinutes()).padStart(2, "0")}`;
  const node = await api.createDoc("docs/便签", stamp);
  await api.writeDoc(node.id, `${t}
`, "", true); // force：覆盖新建占位正文，frontmatter 保留
  await refreshTree();
  s.toast = { msg: `已存便签：${stamp}` };
  setTimeout(() => (s.toast = null), 2500);
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
  // 当前打开文档若在被改名的文件夹内，路径已变：清理 AI 状态后关闭并刷新
  if (s.current?.path.startsWith(path + "/") || s.current?.path === path) {
    closeCurrentDoc();
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
  if (s.saveState === "conflict") {
    // 冲突中的本地未保存编辑不会进 pre_rollback 备份，直接回滚即静默丢失——熔断
    s.toast = { msg: "存在未解决的保存冲突，请先处理冲突（重新加载或等待保存）再回滚" };
    setTimeout(() => {
      if (s.toast?.msg.startsWith("存在未解决的保存冲突")) s.toast = null;
    }, 5000);
    return;
  }
  // 先落盘：pre_rollback 快照备份的是磁盘内容，未保存的编辑必须先 flush 才不会丢
  await flush().catch(() => undefined);
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

/** 导入文件（FR-IMP-01：TXT/MD/DOCX；TXT 转空行分段，DOCX 由 Rust 侧解析为 Markdown） */
export async function importFiles() {
  const { open } = await import("@tauri-apps/plugin-dialog");
  const picked = (await open({
    multiple: true,
    filters: [{ name: "文本文档", extensions: ["txt", "md", "markdown", "docx"] }],
  })) as string[] | string | null;
  if (!picked) return;
  const list = Array.isArray(picked) ? picked : [picked];
  let n = 0;
  const failed: string[] = [];
  for (const p of list) {
    try {
      const f = await api.importRead(p);
      const body = /\.txt$/i.test(f.name) ? txtToBody(f.content) : f.content;
      await api.importDoc(s.selectedFolder, f.name, body);
      n++;
    } catch (e) {
      failed.push(`${p.split(/[\\/]/).pop() ?? p}：${(e as { message?: string }).message ?? "读取/写入失败"}`);
    }
  }
  await refreshTree();
  const msg =
    `已导入 ${n} 篇文档` + (failed.length ? `，${failed.length} 篇失败（${failed.join("；")}）` : "");
  s.toast = { msg };
  setTimeout(() => {
    if (s.toast?.msg === msg) s.toast = null;
  }, failed.length ? 8000 : 3000);
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

function treeHasPath(nodes: TreeNode[], path: string): boolean {
  for (const n of nodes) {
    if (n.id === path) return true;
    if (n.children?.length && treeHasPath(n.children, path)) return true;
  }
  return false;
}

// -- 会话（FR-UI-04：直达上次会话——上次文档 + 光标 + 滚动；按库根分键） ------------

interface SessionPos {
  path: string;
  head: number;
  scrollTop: number;
}
let pendingSessionPos: SessionPos | null = null;
const sessionKey = (root: string) => `writer.session.${root}`;

/** 编辑器周期调用（EditorPane 5s interval）：持久化当前会话位置 */
export function saveSession(pos: { head: number; scrollTop: number } | null) {
  if (!s.current || !s.root || !pos) return;
  try {
    localStorage.setItem(
      sessionKey(s.root),
      JSON.stringify({ path: s.current.path, head: pos.head, scrollTop: pos.scrollTop } satisfies SessionPos),
    );
  } catch {
    /* 配额满等：静默 */
  }
}

/** EditorPane 应用内容后消费（一次性） */
export function consumePendingSessionPos(path: string): { head: number; scrollTop: number } | null {
  if (pendingSessionPos && pendingSessionPos.path === path) {
    const p = { head: pendingSessionPos.head, scrollTop: pendingSessionPos.scrollTop };
    pendingSessionPos = null;
    return p;
  }
  pendingSessionPos = null;
  return null;
}

/** 启动/库切换共用的就绪流程 */
async function applyStartup(info: import("./api").StartupInfo) {
  s.root = info.root;
  s.pendingRecovery = info.pending;
  s.sync.thirdpartyWarnings = info.thirdpartyWarnings ?? [];
  s.current = null;
  s.tree = [];
  s.saveState = "saved";
  s.lastReadHash = "";
  s.sync.conflicts = [];
  s.sync.last = null;
  s.sync.phase = "idle";
  s.sync.running = false;
  await refreshTree();

  // 会话恢复：上次文档 + 光标/滚动（FR-UI-04）；无会话则回退第一篇
  let opened = false;
  try {
    const raw = localStorage.getItem(sessionKey(s.root));
    if (raw) {
      const sess = JSON.parse(raw) as SessionPos;
      if (treeHasPath(s.tree, sess.path)) {
        pendingSessionPos = sess;
        await openDoc(sess.path);
        opened = true;
      }
    }
  } catch {
    /* 会话损坏：回退 */
  }
  if (!opened) {
    const first = firstDoc(s.tree);
    if (first) await openDoc(first.id);
  }

  // 启动告警入通知中心（磁盘对账修复 / 崩溃残留锁接管 / 双引擎）
  const { notify } = await import("./notify");
  for (const w of info.auditWarnings ?? []) notify("system", `磁盘对账：${w}`);
  if (info.tookOverStaleLock) {
    notify("system", "检测到上次异常退出的库锁残留，已自动接管（若另一窗口仍在使用本库请勿同时编辑）");
  }
  for (const w of info.thirdpartyWarnings ?? []) {
    notify("sync", `文库位于第三方同步盘（${w}）范围内，双引擎可能冲突`, {
      actionLabel: "同步设置",
      action: () => (s.sync.settingsOpen = true),
    });
  }
}

/** 库切换/新建（FR-DOC-01/FR-UI-05 菜单入口）：就绪流程 + 同步/AI 重接线（监听器已防重） */
export async function switchLibrary(path: string, create: boolean) {
  // 边界 flush（FR-SAV-01）：切库是保存边界，未落盘输入不得丢失
  clearTimeout(saveTimer);
  await flush().catch(() => undefined);
  // 清空 AI 会话绑定（旧库 convId 在新库 meta.db 是孤儿行，防串库）
  void import("./aiflow").then((m) => m.resetChatForLibrary());
  const info = create ? await api.libraryCreate(path) : await api.libraryOpen(path);
  await applyStartup(info);
  void initSync();
  void import("./aiflow").then((m) => m.initAi());
}

/** FR-UI-04：直达上次会话（M1 = 打开第一篇）；onStep 用于启动屏进度诊断 */
export async function init(onStep?: (msg: string) => void) {
  onStep?.("正在打开文库…");
  const info = await api.startup();
  onStep?.("读取文档树…");
  await applyStartup(info);
  s.ready = true;
  void initSync(); // 同步接线失败不影响主功能（3.3-1 离线优先）
  void import("./aiflow").then((m) => m.initAi()); // AI 接线（离线优先同理）
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

// ---------------------------------------------------------------------------
// 云同步（FR-SYN，M3）：自动同步去抖 / 事件监听 / 打开文档远端更新协议
// ---------------------------------------------------------------------------

// 编辑器位置桥：external_change 重载时保持光标与滚动（A15）
let editorGetPos: () => { head: number; scrollTop: number } | null = () => null;
let editorRestorePos: (head: number, scrollTop: number) => void = () => undefined;
export function bindEditorPos(getPos: () => { head: number; scrollTop: number } | null, restore: (head: number, scrollTop: number) => void) {
  editorGetPos = getPos;
  editorRestorePos = restore;
}

let autoSyncTimer: number | undefined;

/** FR-SYN-03：保存成功后 10s 去抖自动同步（输入不打扰，静默执行）。
 * 尊重设置面板的"保存后自动同步"开关（默认开） */
export function scheduleAutoSync() {
  if (s.sync.autoSync === false) return;
  clearTimeout(autoSyncTimer);
  autoSyncTimer = window.setTimeout(() => void triggerSync(), 10_000);
}

/** 手动同步（按钮 / Ctrl+Shift+S） */
export async function triggerSync(direction?: "upload" | "download") {
  const ack = await api.syncNow(direction);
  if (!ack.started && ack.reason) {
    s.toast = { msg: `同步未启动：${ack.reason}` };
    setTimeout(() => {
      if (s.toast?.msg.startsWith("同步未启动")) s.toast = null;
    }, 3000);
  }
}

/** 首次绑定方向选择（FR-SYN-07）后触发 */
export async function chooseDirection(d: "upload" | "download") {
  s.sync.directionAsk = null;
  await triggerSync(d);
}

export async function reloadSyncStatus() {
  const st = await api.syncStatus();
  s.sync.running = st.running;
  s.sync.phase = st.phase;
  s.sync.last = st.last;
  s.sync.conflicts = st.conflicts;
}

export async function openSyncHistory() {
  s.sync.historyOpen = true;
  s.sync.history = await api.syncHistory(200);
}

/** 冲突三动作（FR-SYN-04）：keep=false 保留此版/手动合并后清理；keep=true 保留彼版 */
export async function resolveConflict(copyPath: string, keepRemote: boolean) {
  await api.syncConflictResolve(copyPath, keepRemote);
  await refreshTree();
  await reloadSyncStatus();
  // 原文档可能被替换（保留彼版）→ 重载
  const orig = s.sync.conflicts.find((c) => c.copyPath === copyPath);
  if (orig && s.current?.path === orig.path) await openDoc(orig.path, true);
  s.toast = { msg: keepRemote ? "已保留远程版本（替换前已自动快照）" : "已保留本地版本，将于下次同步生效" };
  setTimeout(() => {
    if (s.toast) s.toast = null;
  }, 3500);
}

/** doc:external_change（FR-SYN-09）：无未保存修改自动重载；有修改走冲突态 */
async function onExternalChange(path: string, _hash: string) {
  await refreshTree();
  if (s.current?.path !== path) return;
  if (s.saveState === "dirty" || s.saveState === "saving" || s.saveState === "conflict") {
    // 有未保存输入：本地磁盘已被同步更新 → 保存护栏会在下次保存时拦截；
    // 直接提示用户走冲突流程（V1 载体：冲突横幅 + 重新加载按钮）
    s.saveState = "conflict";
    s.saveError = "远端有新版本已下载到磁盘，本地有未保存修改";
    return;
  }
  const pos = editorGetPos();
  const d = await api.readDoc(path);
  if (d.hash === s.lastReadHash) return; // 内容未变（无谓重载）
  s.current = d;
  s.lastReadHash = d.hash;
  s.saveState = "saved";
  s.saveError = "";
  s.reloadNonce++; // path 未变 → 用 nonce 驱动编辑器重应用内容
  if (pos) {
    await new Promise((r) => setTimeout(r, 0)); // 等 watcher 应用内容后再恢复光标
    editorRestorePos(pos.head, pos.scrollTop);
  }
  s.toast = { msg: "文档已因远端更新自动重载（撤销栈已清空）" };
  setTimeout(() => {
    if (s.toast?.msg.startsWith("文档已因远端")) s.toast = null;
  }, 4000);
}

/** 启动同步接线：init() 完成后调用一次；库切换重复调用时仅刷新状态（监听器防重） */
let syncListenersReady = false;
export async function initSync() {
  if (syncListenersReady) {
    await reloadSyncStatus().catch(() => undefined);
    const cfg = await api.syncConfigGet().catch(() => null);
    if (cfg) {
      s.sync.autoSync = cfg.autoSync;
      if (cfg.enabled) void triggerSync();
      setupPeriodicSync();
    }
    return;
  }
  // 半注册追踪：try 外声明，catch 块能卸掉已注册的监听器（否则重跑会双份监听）
  const unlistens: (() => void)[] = [];
  try {
    await reloadSyncStatus();
    const { listen } = await import("@tauri-apps/api/event");
    const { notify } = await import("./notify");
    // 库标识守卫：切库后旧批次事件不得污染新库（后端 sync:* / doc:* 事件均带 libRoot）
    const fromCurrentLib = (p: { libRoot?: string }) => !p.libRoot || p.libRoot === s.root;
    unlistens.push(
      await listen<{ phase: string; libRoot?: string }>("sync:state", (e) => {
        if (!fromCurrentLib(e.payload)) return;
        s.sync.phase = e.payload.phase;
        s.sync.running = e.payload.phase !== "idle";
      }),
    );
    unlistens.push(
      await listen<{ path: string; hash: string; libRoot?: string }>("doc:external_change", (e) => {
        if (!fromCurrentLib(e.payload)) return;
        void onExternalChange(e.payload.path, e.payload.hash);
      }),
    );
    unlistens.push(
      await listen<{ path: string; copyPath: string; libRoot?: string }>("sync:conflict", (e) => {
        if (!fromCurrentLib(e.payload)) return;
        if (!s.sync.conflicts.some((c) => c.copyPath === e.payload.copyPath)) {
          s.sync.conflicts.push({
            uuid: "",
            path: e.payload.path,
            copyPath: e.payload.copyPath,
          });
        }
        notify("conflict", "检测到同步冲突，已保留两个版本", {
          actionLabel: "去处理",
          action: () => (s.sync.conflicts.length = s.sync.conflicts.length),
        });
      }),
    );
    unlistens.push(
      await listen<{ localDocs: number; remoteDocs: number; libRoot?: string }>("sync:needs_direction", (e) => {
        if (!fromCurrentLib(e.payload)) return;
        s.sync.directionAsk = e.payload; // 非模态提示 + 显式选择（3.3-5 白名单允许）
      }),
    );
    unlistens.push(
      await listen<SyncReport>("sync:done", async (e) => {
        // 库标识过滤：切库后旧批次的事件不得污染新库状态（后端事件带 libRoot）
        if (e.payload.libRoot && e.payload.libRoot !== s.root) return;
        s.sync.running = false;
        s.sync.phase = "idle";
        s.sync.last = e.payload;
        resetSyncRetry(); // FR-SYN-05：成功即复位退避
        await refreshTree();
        await reloadSyncStatus();
      }),
    );
    unlistens.push(
      await listen<{ message: string; libRoot?: string }>("sync:warning", (e) => {
        if (e.payload.libRoot && e.payload.libRoot !== s.root) return;
        // 批次内告警：走 notify 管线（输入保护 + 通知中心沉淀），不直写 toast
        notify("sync", `同步提示：${e.payload.message}`);
      }),
    );
    unlistens.push(
      await listen<{ code?: string; message: string; libRoot?: string }>("sync:error", (e) => {
        if (e.payload.libRoot && e.payload.libRoot !== s.root) return;
        s.sync.running = false;
        // FR-SYN-05：可重试错误（网络/超时/限流）自动退避补同步——同步为状态驱动，
        // 离线期间的改动在网络恢复后随下一批次自动补齐，无需显式队列
        const code = e.payload.code ?? "";
        const retryable = ["", "net", "net_connect", "net_timeout", "sync_rate_limited"].includes(code);
        if (retryable) {
          const waitMin = scheduleSyncRetry();
          const tip =
            code === "sync_rate_limited"
              ? `坚果云限流或流量超限（429/507），${waitMin} 分钟后自动重试；改动不会丢失，恢复后自动补同步`
              : `网络暂不可用，${waitMin} 分钟后自动重试；离线期间的改动会在网络恢复后自动同步`;
          notify("sync", tip);
        } else {
          notify("sync", `同步失败：${e.payload.message}`, {
            actionLabel: "同步历史",
            action: () => void openSyncHistory(),
          });
        }
      }),
    );
    unlistens.push(
      await listen<{ path: string; libRoot?: string }>("sync:trashed", async (e) => {
        if (e.payload.libRoot && e.payload.libRoot !== s.root) return;
        // 对端删除传播：本端文档进回收站（关闭前 flush，<2s 未落盘输入不丢）
        await refreshTree();
        if (s.current?.path === e.payload.path) {
          await flush().catch(() => undefined);
          closeCurrentDoc();
        }
      }),
    );
    unlistens.push(
      await listen<{ from: string; to: string; libRoot?: string }>("doc:renamed", async (e) => {
        if (e.payload.libRoot && e.payload.libRoot !== s.root) return;
        // 远端改名跟随：当前打开文档路径变化（保存中的内容先落盘）
        await refreshTree();
        if (s.current?.path === e.payload.from) {
          await flush().catch(() => undefined);
          const d = await api.readDoc(e.payload.to);
          s.current = { ...d, path: e.payload.to };
          s.lastReadHash = d.hash;
        }
      }),
    );
    // 启动时自动同步一次（FR-SYN-03）+ 可配置定时同步（长开时对端更新可达）
    const cfg = await api.syncConfigGet();
    s.sync.autoSync = cfg.autoSync;
    if (cfg.enabled) void triggerSync();
    setupPeriodicSync();
    syncListenersReady = true;
    return () => unlistens.forEach((u) => u());
  } catch {
    // 半注册清理：中途失败时卸掉已注册的监听器（否则库切换重跑会双份监听）
    unlistens.forEach((u) => u());
    return () => undefined; // 同步接线失败不影响主功能（离线优先）
  }
}

// FR-SYN-03 定时同步：长开应用周期性拉取对端更新（默认 5 分钟；0=关）
let periodicTimer: number | undefined;
export function setupPeriodicSync() {
  clearInterval(periodicTimer);
  const minutes = Number(localStorage.getItem("writer.sync.interval") ?? 5);
  if (minutes > 0 && s.sync.autoSync) {
    periodicTimer = window.setInterval(() => {
      if (!s.sync.running && s.sync.autoSync) void triggerSync();
    }, minutes * 60_000);
  }
}

// ---------------------------------------------------------------------------
// FR-SYN-05（V1.1）：失败退避重试 + 网络恢复自动补同步
// 同步为状态驱动——离线改动无需显式入队，网络恢复后的首个批次自动补齐；
// 这里只做"失败后何时自动再试"：指数退避（30s → 1m → 5m → 15m → 1h 封顶），
// 成功或网络恢复事件即复位
// ---------------------------------------------------------------------------
let syncRetryAttempt = 0;
let syncRetryTimer: number | undefined;
const SYNC_BACKOFF_MIN = [0.5, 1, 5, 15, 60];

function scheduleSyncRetry(): number {
  const idx = Math.min(syncRetryAttempt, SYNC_BACKOFF_MIN.length - 1);
  const waitMin = SYNC_BACKOFF_MIN[idx];
  syncRetryAttempt++;
  clearTimeout(syncRetryTimer);
  syncRetryTimer = window.setTimeout(
    () => {
      if (!s.sync.running) void triggerSync();
    },
    waitMin * 60_000,
  );
  return waitMin;
}

function resetSyncRetry() {
  syncRetryAttempt = 0;
  clearTimeout(syncRetryTimer);
}

if (typeof window !== "undefined") {
  // 网络恢复：立即补同步（FR-SYN-05 恢复语义）
  window.addEventListener("online", () => {
    if (s.ready) {
      resetSyncRetry();
      void triggerSync();
    }
  });
}
