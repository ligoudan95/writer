/**
 * AI 流程中心（FR-AI-02/03/04/06/08，M4）。
 * 编辑器交互经 bridge 注入（aiflow 不依赖 TipTap）；网络全部经 Rust NetGateway（ai.* 命令）。
 * 隐私拦截（A7）：隐私文档的内容永不进入任何请求——发送前在 TS 侧拦截，零网络发送。
 */

import { reactive } from "vue";
import { api, type AiProvider, type ChatMsg } from "./api";
import type { DiffBlock, DiffOp } from "./diff";
import { applyBlocks, diffOps, opsToBlocks } from "./diff";
import { s } from "./store";
import { docToMd, type PMNode } from "./codec";

// ---------------------------------------------------------------------------
// 编辑器桥（EditorPane 挂载时注入；store/aiflow 不 import TipTap）
// ---------------------------------------------------------------------------

export interface AiEditorBridge {
  getSelection: () => { from: number; to: number; text: string } | null;
  /** 单事务替换范围并采纳为新内容（一次 Ctrl+Z 整体还原，AC A5） */
  replaceRangeWithMd: (from: number, to: number, md: string) => void;
  /** 在光标处插入草稿段（aiDraft mark），返回草稿范围 */
  insertDraft: (md: string) => { from: number; to: number } | null;
  /** 替换现有草稿范围内容（保持 mark） */
  updateDraft: (from: number, to: number, md: string) => { from: number; to: number } | null;
  /** 接受草稿：移除 mark，文本留存（单事务，可一次撤销） */
  acceptDraft: (from: number, to: number) => void;
  /** 删除草稿范围 */
  removeDraft: (from: number, to: number) => void;
  getCursor: () => number;
  /** 光标前/后各 N 字 + 章节开头（FR-AI-03 上下文策略） */
  contextAround: (n: number) => { before: string; after: string; chapterStart: string };
  /** 当前选区所在章节文本（讨论上下文用） */
  chapterText: () => string;
  /** 在光标处插入正文（讨论回复插入） */
  insertAtCursor: (md: string) => void;
  /** 撤销栈手动保护：草稿操作期间抑制自动保存 */
  suppressSave: (on: boolean) => void;
}

/** FR-AI-03/04：发送前上下文用量展示（字符口径；面板消费） */
function kindLabel(kind: string): string {
  return kind === "polish" ? "润色" : kind === "continue" ? "续写" : "讨论";
}

let ed: AiEditorBridge | null = null;
export function setAiEditor(b: AiEditorBridge) {
  ed = b;
}

// ---------------------------------------------------------------------------
// 状态
// ---------------------------------------------------------------------------

export const POLISH_STYLES = [
  { id: "concise", name: "精简", prompt: "请精简以下文字，去除冗余，保持原意与语气。" },
  { id: "enhance", name: "文笔增强", prompt: "请增强以下文字的文笔：用词更精准、句式更有表现力，不改变含义。" },
  { id: "colloquial", name: "口语化", prompt: "请把以下文字改写为自然流畅的口语化表达，保持原意。" },
  { id: "formal", name: "书面化", prompt: "请把以下文字改写为严谨的书面语，保持原意。" },
  { id: "custom", name: "自定义", prompt: "请按用户要求修改以下文字。" },
];

function loadGlobalEnabled(): boolean {
  return localStorage.getItem("writer.ai.off") !== "1";
}

export const ai = reactive({
  /** 全局 AI 开关（FR-AI-06：关闭后隐藏全部入口） */
  enabled: loadGlobalEnabled(),
  providers: [] as AiProvider[],
  settingsOpen: false,
  chatOpen: false,
  job: null as string | null,

  // 润色（FR-AI-02）
  polishOpen: false,
  polish: {
    state: "idle" as "idle" | "running" | "review",
    jobId: "",
    style: "concise",
    extra: "",
    from: 0,
    to: 0,
    original: "",
    polished: "",
    ops: null as DiffOp[] | null,
    blocks: [] as DiffBlock[],
    accepts: [] as boolean[],
    providerName: "",
    error: "",
  },

  // 续写（FR-AI-03）
  cont: {
    state: "idle" as "idle" | "running" | "draft",
    jobId: "",
    anchor: 0,
    draftFrom: 0,
    draftTo: 0,
    text: "",
    /** 用户在插入点前输入后，草稿收起为"待插入" */
    collapsed: false,
    error: "",
  },

  // 讨论（FR-AI-04）
  chat: {
    convId: "",
    docUuid: "",
    jobId: "",
    messages: [] as { role: string; content: string }[],
    streaming: false,
    streamText: "",
    contextMode: "none" as "none" | "selection" | "chapter" | "doc" | "paste",
    pasteText: "",
    error: "",
  },

  /** 最近一次请求的上下文字符量（发送前预览口径，面板展示） */
  lastRequestChars: 0,
});

export function defaultProvider(): AiProvider | null {
  return ai.providers.find((p) => p.enabled && p.isDefault) ?? ai.providers.find((p) => p.enabled) ?? null;
}

export function toggleGlobal(v?: boolean) {
  ai.enabled = v ?? !ai.enabled;
  localStorage.setItem("writer.ai.off", ai.enabled ? "0" : "1");
}

// ---------------------------------------------------------------------------
// 隐私拦截（FR-AI-06 / A7）：拦截点在"内容进入请求"之前
// ---------------------------------------------------------------------------

/** 文档内容是否可作为上下文发送；隐私文档返回 false 并提示 */
function privacyGate(what: string): boolean {
  if (s.current?.privacy) {
    s.toast = { msg: `该文档已标记为隐私，${what}已被拦截（未发送任何内容）` };
    setTimeout(() => {
      if (s.toast?.msg.includes("已被拦截")) s.toast = null;
    }, 5000);
    return false;
  }
  return true;
}

// ---------------------------------------------------------------------------
// 事件路由（ai:chunk/done/error/cancelled → 各流程）
// ---------------------------------------------------------------------------

let listenersReady = false;

export async function initAi() {
  try {
    ai.providers = await api.aiProviderList();
  } catch {
    /* 库未就绪时静默 */
  }
  if (listenersReady) return () => undefined; // 库切换重复调用：监听器防重
  try {
    const { listen } = await import("@tauri-apps/api/event");
    const un: (() => void)[] = [];
    un.push(
      await listen<{ jobId: string; kind: string; text: string }>("ai:chunk", (e) => {
        const p = e.payload;
        if (p.kind === "polish") return; // 润色流式仅计数，diff 只对完整响应计算
        if (p.kind === "continue" && p.jobId === ai.cont.jobId) {
          ai.cont.text += p.text;
          refreshDraft();
        } else if (p.kind === "chat" && p.jobId === ai.chat.jobId) {
          ai.chat.streamText += p.text;
        }
      }),
    );
    un.push(
      await listen<{ jobId: string; kind: string; content: string }>("ai:done", (e) => {
        const p = e.payload;
        // jobId 门控：切换文档/新作业后到达的陈旧事件不得污染当前状态
        if (p.kind === "polish" && p.jobId === ai.polish.jobId) onPolishDone(p.content);
        else if (p.kind === "continue" && p.jobId === ai.cont.jobId) onContinueDone();
        else if (p.kind === "chat" && p.jobId === ai.chat.jobId) onChatDone(p.content);
      }),
    );
    un.push(
      await listen<{ jobId: string; kind: string; code: string; message: string }>("ai:error", (e) => {
        const p = e.payload;
        const msg = `[${p.code}] ${p.message}`;
        if (p.kind === "polish" && p.jobId === ai.polish.jobId) {
          ai.polish.state = "idle";
          ai.polish.error = msg;
        } else if (p.kind === "continue" && p.jobId === ai.cont.jobId) {
          if (ai.cont.state === "running") ai.cont.state = "idle";
          ai.cont.error = msg;
          try {
            cleanupDraft();
          } catch {
            /* 草稿区间失效 */
          }
        } else if (p.kind === "chat" && p.jobId === ai.chat.jobId) {
          ai.chat.streaming = false;
          ai.chat.error = msg;
        } else {
          return; // 非当前作业的错误不广播
        }
        // AI 失败非模态沉淀通知中心（FR-UI-06：面板可能已关，历史不丢）
        void import("./notify").then((m) => m.notify("ai", `${kindLabel(p.kind)}失败：${msg}`));
      }),
    );
    un.push(
      await listen<{ jobId: string; kind: string }>("ai:cancelled", (e) => {
        // 按 kind + jobId 收尾：迟到的取消事件不得复位新作业的状态
        //（cancelled 在流循环收到下一个 SSE 项时才发出，可晚于新作业启动数秒）
        const p = e.payload;
        if (p.kind === "polish" && p.jobId === ai.polish.jobId && ai.polish.state === "running") {
          ai.polish.state = "idle";
        } else if (p.kind === "continue" && p.jobId === ai.cont.jobId && ai.cont.state === "running") {
          ai.cont.state = "idle";
        } else if (p.kind === "chat" && p.jobId === ai.chat.jobId) {
          ai.chat.streaming = false;
        }
      }),
    );
    listenersReady = true;
    return () => un.forEach((f) => f());
  } catch {
    return () => undefined;
  }
}

export async function reloadProviders() {
  ai.providers = await api.aiProviderList();
}

// ---------------------------------------------------------------------------
// 润色（FR-AI-02 / AC A5）
// ---------------------------------------------------------------------------

export function canPolish(): boolean {
  return ai.enabled && !!defaultProvider() && !!ed?.getSelection();
}

/** 处理中用户在选区按键 → 取消请求并立即解锁（不静默吞输入） */
export function cancelRunning(reason: "polish" | "continue") {
  if (reason === "polish" && ai.polish.state === "running" && ai.polish.to) {
    void api.aiJobCancel(ai.polish.jobId);
    ai.polish.state = "idle";
    ai.polish.jobId = ""; // 清除门控：迟到的 ai:done 不得把已取消的润色"复活"为 review
  }
  if (reason === "continue" && ai.cont.state !== "idle") {
    void api.aiJobCancel(ai.cont.jobId);
    if (!ai.cont.collapsed) cleanupDraft();
    ai.cont.state = "idle";
    ai.cont.collapsed = false;
    ai.cont.text = "";
  }
}

/** 切换文档前清理全部 AI 流程状态：A 文档的草稿/润色区间/会话绝不能落进 B 文档。
 * 顺序：先清 jobId（陈旧事件全被门控丢弃）再尝试清草稿——草稿区间可能已失效
 * （编辑器被清空/远端重载），removeDraft 抛错不得中断重置（否则门控被绕过） */
export function resetAiForDocSwitch() {
  // 1. 先断事件门控与状态
  const polishJob = ai.polish.jobId;
  const contJob = ai.cont.jobId;
  const chatJob = ai.chat.jobId;
  ai.polish.jobId = "";
  ai.cont.jobId = "";
  ai.chat.jobId = "";
  if (ai.polish.state === "running") void api.aiJobCancel(polishJob);
  if (ai.cont.state === "running") void api.aiJobCancel(contJob);
  if (ai.chat.streaming) void api.aiJobCancel(chatJob);
  // 2. 再清草稿（区间失效时静默放弃——内容本就是未接受的草稿）
  try {
    if (ai.cont.state === "draft" && !ai.cont.collapsed) cleanupDraft();
  } catch {
    // 草稿区间越界（编辑器内容已换）：无需处理
  }
  resetPolish();
  ai.cont.state = "idle";
  ai.cont.text = "";
  ai.cont.collapsed = false;
  ai.cont.error = "";
  ai.chat.streaming = false;
  ai.chat.streamText = "";
  ai.chat.error = "";
}

/** 库切换时清空会话绑定：旧库 convId 在新库 meta.db 是孤儿行（防串库） */
export function resetChatForLibrary() {
  ai.chat.convId = "";
  ai.chat.docUuid = "";
  ai.chat.messages = [];
  ai.chat.streaming = false;
  ai.chat.streamText = "";
  ai.chat.error = "";
  ai.chat.jobId = "";
}

/** 打开润色面板（Ctrl+Shift+P / 工具栏；须有选区）；
 * 未配置 Provider → 引导跳转设置（FR-UI-07 空状态） */
export function openPolishUI() {
  if (!ai.enabled) return;
  if (!defaultProvider()) {
    notifyProviderGuide("润色");
    return;
  }
  if (!ed?.getSelection()?.text.trim()) {
    s.toast = { msg: "请先选中要润色的文字" };
    setTimeout(() => (s.toast = null), 2500);
    return;
  }
  ai.polishOpen = true;
}

/** FR-UI-07：未配置 AI 时的引导通知（一键跳转 Provider 配置） */
function notifyProviderGuide(what: string) {
  void import("./notify").then((m) =>
    m.notify("ai", `尚未配置 AI Provider，无法使用${what}`, {
      actionLabel: "去配置",
      action: () => (ai.settingsOpen = true),
      dur: 8000,
    }),
  );
}

export async function startPolish() {
  if (!ed) return;
  const sel = ed.getSelection();
  const p = defaultProvider();
  if (!sel || sel.text.trim().length < 2 || !p) return;
  if (!privacyGate("润色")) return; // A7：零发送

  const style = POLISH_STYLES.find((x) => x.id === ai.polish.style) ?? POLISH_STYLES[0];
  const messages: ChatMsg[] = [
    { role: "system", content: "你是中文写作助手。只输出修改后的文字，不要解释、不要加引号。" },
    {
      role: "user",
      content: `${style.prompt}${ai.polish.extra ? `\n附加要求：${ai.polish.extra}` : ""}\n\n${sel.text}`,
    },
  ];
  ai.polish.state = "running";
  ai.polish.error = "";
  ai.polish.from = sel.from;
  ai.polish.to = sel.to;
  ai.polish.original = sel.text;
  ai.polish.providerName = p.name;
  ai.lastRequestChars = sel.text.length; // FR-AI-02 发送前用量（面板展示）
  try {
    const ack = await api.aiChatStart("polish", p.id, messages);
    ai.polish.jobId = ack.jobId;
  } catch (e) {
    ai.polish.state = "idle";
    ai.polish.error = (e as { message?: string }).message ?? String(e);
  }
}

function onPolishDone(polished: string) {
  if (ai.polish.state !== "running") return; // 已取消（用户已按键改动文档）：不得复活 review 态
  ai.polish.polished = polished;
  ai.polish.ops = diffOps(ai.polish.original, polished);
  ai.polish.blocks = opsToBlocks(ai.polish.ops);
  ai.polish.accepts = ai.polish.blocks.map(() => true);
  ai.polish.state = ai.polish.blocks.length ? "review" : "idle";
  if (!ai.polish.blocks.length) {
    s.toast = { msg: "润色完成：无修改建议" };
    setTimeout(() => (s.toast = null), 3000);
  }
}

/** 应用已接受的块：pre_ai 快照 → 单事务替换（一次 Ctrl+Z 整体还原，AC A5） */
export async function applyPolish(onlyAccepted: boolean) {
  if (!ed || !ai.polish.ops) return;
  const accepts = onlyAccepted ? ai.polish.accepts : ai.polish.accepts.map(() => true);
  const newMd = applyBlocks(ai.polish.ops, accepts);
  if (newMd === ai.polish.original) {
    resetPolish();
    return;
  }
  const cur = s.current;
  if (cur) {
    await api.snapshotCreate(cur.path, "pre_ai", "润色前自动备份（FR-AI-02）");
  }
  ed.replaceRangeWithMd(ai.polish.from, ai.polish.to, newMd);
  resetPolish();
}

export function resetPolish() {
  ai.polishOpen = false;
  ai.polish.state = "idle";
  ai.polish.jobId = "";
  ai.polish.ops = null;
  ai.polish.blocks = [];
  ai.polish.accepts = [];
  ai.polish.polished = "";
}

// ---------------------------------------------------------------------------
// 续写（FR-AI-03 / AC A6）
// ---------------------------------------------------------------------------

/** 上下文策略：光标前 N 字 + 光标后 N 字 + 章节开头；截断范围在预览中明示 */
export function continueContext(n = 800) {
  if (!ed) return null;
  const ctx = ed.contextAround(n);
  return ctx;
}

export async function startContinue() {
  if (!ed || !ai.enabled) return; // 全局关闭旁路修复：快捷键不得绕过开关
  const p = defaultProvider();
  if (!p) {
    notifyProviderGuide("续写");
    return;
  }
  if (!privacyGate("续写")) return;
  if (ai.cont.state !== "idle") {
    cancelRunning("continue"); // 已有草稿/在跑：先清理，防旧草稿成孤儿残留
  }
  // 挂起的 2s 保存计时器会把未接受草稿落盘，触发续写即取消
  void import("./store").then((m) => m.cancelScheduledSave());

  const ctx = ed.contextAround(800);
  const parts: string[] = [];
  if (ctx.chapterStart) parts.push(`【本章开头】\n${ctx.chapterStart}`);
  parts.push(`【光标前文】\n${ctx.before}`);
  if (ctx.after) parts.push(`【光标后文】\n${ctx.after}`);
  const messages: ChatMsg[] = [
    {
      role: "system",
      content:
        "你是中文小说/文章写作助手。请顺着光标处的文风与情节续写 1-2 个自然段，不要重复已有内容，不要解释。直接输出续写正文。",
    },
    { role: "user", content: parts.join("\n\n") },
  ];
  ai.cont.state = "running";
  ai.cont.error = "";
  ai.cont.text = "";
  ai.cont.collapsed = false;
  ai.cont.anchor = ed.getCursor();
  ai.lastRequestChars = parts.join("\n\n").length; // FR-AI-03 发送前用量
  try {
    const ack = await api.aiChatStart("continue", p.id, messages);
    ai.cont.jobId = ack.jobId;
  } catch (e) {
    ai.cont.state = "idle";
    ai.cont.error = (e as { message?: string }).message ?? String(e);
  }
}

function refreshDraft() {
  if (!ed) return;
  if (ai.cont.state === "running" && ai.cont.text) {
    // 首个增量到达：落草稿
    ed.suppressSave(true);
    const r = ed.insertDraft(ai.cont.text);
    if (r) {
      ai.cont.draftFrom = r.from;
      ai.cont.draftTo = r.to;
      ai.cont.state = "draft";
    }
    ed.suppressSave(false);
  } else if (ai.cont.state === "draft" && ai.cont.text) {
    ed.suppressSave(true);
    const r = ed.updateDraft(ai.cont.draftFrom, ai.cont.draftTo, ai.cont.text);
    if (r) {
      ai.cont.draftFrom = r.from;
      ai.cont.draftTo = r.to;
    }
    ed.suppressSave(false);
  }
}

function onContinueDone() {
  // 取消（Esc）与 done 竞态：已回 idle 则不得再拉回 draft（悬空区间）
  if (ai.cont.state === "running") ai.cont.state = "draft"; // 等待 Tab/Esc；流已结束
}

/** Tab 接受：移除草稿 mark，单事务（AC A6） */
export function acceptContinue() {
  if (!ed || ai.cont.state !== "draft") return;
  ed.suppressSave(true);
  ed.acceptDraft(ai.cont.draftFrom, ai.cont.draftTo);
  ed.suppressSave(false);
  ai.cont.state = "idle";
  ai.cont.text = "";
  ai.cont.collapsed = false;
  scheduleAiSave();
}

/** Esc 放弃 */
export function dismissContinue() {
  cancelRunning("continue");
}

/** 用户在草稿插入点之前输入 → 草稿收起为"待插入"，Tab 随时取回（FR-AI-03 固定交互） */
export function maybeCollapseDraft(cursor: number) {
  if (ai.cont.state !== "draft" || ai.cont.collapsed || !ed) return;
  if (cursor < ai.cont.draftFrom) {
    ed.suppressSave(true);
    ed.removeDraft(ai.cont.draftFrom, ai.cont.draftTo);
    ed.suppressSave(false);
    ai.cont.collapsed = true;
    void import("./notify").then((m) => m.notify("ai", "续写草稿已收起为待插入（Tab 取回）", { dur: 4000 }));
  }
}

/** Tab 取回落起的草稿 */
export function restoreCollapsedDraft() {
  if (!ai.cont.collapsed || !ed) return;
  ed.suppressSave(true);
  const r = ed.insertDraft(ai.cont.text);
  ed.suppressSave(false);
  if (r) {
    ai.cont.draftFrom = r.from;
    ai.cont.draftTo = r.to;
    ai.cont.collapsed = false;
    ai.cont.state = "draft";
  }
}

function cleanupDraft() {
  if (!ed || ai.cont.collapsed) return;
  if (ai.cont.state === "draft" && ai.cont.draftTo > ai.cont.draftFrom) {
    ed.suppressSave(true);
    ed.removeDraft(ai.cont.draftFrom, ai.cont.draftTo);
    ed.suppressSave(false);
  }
  ai.cont.text = "";
}

/** AI 编辑后走正常保存管线：onUpdate 被抑制 → saveState 停留 saved，
 * saveNow 会早退——显式置 dirty 强制落盘（否则接受后直接关窗即丢稿） */
function scheduleAiSave() {
  import("./store").then((m) => {
    m.s.saveState = "dirty";
    void m.saveNow();
  });
}

// ---------------------------------------------------------------------------
// 讨论（FR-AI-04 / AC A7 上下文拦截）
// ---------------------------------------------------------------------------

/** 加载/切换当前文档的会话（openChat 与文档切换 watch 共用；非 toggle 语义） */
export async function loadConversation() {
  if (!s.current) return;
  const docUuid = docUuidOf();
  if (ai.chat.docUuid !== docUuid || !ai.chat.convId) {
    const conv = await api.aiConversationEnsure(docUuid, s.current.title);
    ai.chat.convId = conv.id;
    ai.chat.docUuid = docUuid;
    const msgs = await api.aiMessageList(conv.id);
    ai.chat.messages = msgs.map((m) => ({ role: m.role, content: m.content }));
  }
}

export async function openChat() {
  ai.chatOpen = !ai.chatOpen;
  if (!ai.chatOpen) return;
  if (!defaultProvider()) {
    notifyProviderGuide("讨论");
    // 面板照常打开（可看历史），发送时会被拦截
  }
  await loadConversation();
}

/** frontmatter id 即文档 uuid（与同步身份锚点同源，改道/改名后会话不断）；
 * 外部放入的无 id 文档以 path 兜底——否则多个空 uuid 文档共享同一会话串档 */
export function docUuidOf(): string {
  return s.current?.uuid || s.current?.path || "";
}

function buildChatContext(): string | null {
  if (!ed) return null;
  switch (ai.chat.contextMode) {
    case "selection": {
      const sel = ed.getSelection();
      return sel && sel.text.trim() ? `【选中文本】\n${sel.text}` : null;
    }
    case "chapter": {
      const t = ed.chapterText();
      return t ? `【当前章节】\n${t}` : null;
    }
    case "doc": {
      const json = currentDocJson();
      return json ? `【当前文档】\n${docToMd(json)}` : null;
    }
    case "paste":
      return ai.chat.pasteText.trim() ? `【粘贴片段】\n${ai.chat.pasteText}` : null;
    default:
      return null;
  }
}

let currentJsonFn: () => PMNode | null = () => null;
export function bindAiDocJson(fn: () => PMNode | null) {
  currentJsonFn = fn;
}
function currentDocJson(): PMNode | null {
  return currentJsonFn();
}

/** 返回是否已受理发送（隐私拦截返回 false，调用方据此保留用户输入不清空） */
export async function sendChat(text: string): Promise<boolean> {
  const p = defaultProvider();
  if (!p || !text.trim() || ai.chat.streaming) return false; // 未进入发送管线：保留输入
  // A7：隐私文档任何档位一律拦截（历史轮次可能复述过原文，none/paste 档也不放行）
  if (!privacyGate("讨论")) return false;
  // 会话守卫：openDoc 后未经 loadConversation / 切换竞态时不得把内容写进旧会话
  if (!ai.chat.convId || ai.chat.docUuid !== docUuidOf()) await loadConversation();
  const ctx = buildChatContext();
  ai.chat.messages.push({ role: "user", content: text });
  await api.aiMessageAppend(ai.chat.convId, "user", text);
  ai.chat.streaming = true;
  ai.chat.streamText = "";
  ai.chat.error = "";
  const messages: ChatMsg[] = [
    { role: "system", content: "你是写作讨论助手，回答简洁、贴合创作语境。" },
    ...ai.chat.messages.slice(-16).map((m) => ({ role: m.role as ChatMsg["role"], content: m.content })),
  ];
  if (ctx) messages.push({ role: "user", content: `（讨论上下文，供参考）\n${ctx}` });
  ai.lastRequestChars = messages.reduce((n, m) => n + m.content.length, 0); // FR-AI-04 发送前用量
  try {
    const ack = await api.aiChatStart("chat", p.id, messages);
    ai.chat.jobId = ack.jobId;
  } catch (e) {
    ai.chat.streaming = false;
    ai.chat.error = (e as { message?: string }).message ?? String(e);
  }
  return true;
}

/** 重试前移除末条未获回复的用户消息（UI 与后续上下文不翻倍；DB 旧记录待 M5 历史清理入口） */
export function popLastUserMessage() {
  for (let i = ai.chat.messages.length - 1; i >= 0; i--) {
    if (ai.chat.messages[i].role === "user") {
      ai.chat.messages.splice(i, 1);
      return;
    }
  }
}

function onChatDone(content: string) {
  ai.chat.streaming = false;
  ai.chat.messages.push({ role: "assistant", content });
  ai.chat.streamText = "";
  void api.aiMessageAppend(ai.chat.convId, "assistant", content);
}

/** 插入正文（FR-AI-04：一键将回复插入正文） */
export function insertReplyToBody(content: string) {
  ed?.insertAtCursor(content);
}

/** 导出为 md（B7：讨论成果沉淀） */
export async function exportChatMd() {
  if (!ai.chat.messages.length) return;
  const { save } = await import("@tauri-apps/plugin-dialog");
  const dest = await save({
    defaultPath: `AI讨论-${new Date().toISOString().slice(0, 10).replace(/-/g, "")}.md`,
    filters: [{ name: "Markdown", extensions: ["md"] }],
  });
  if (!dest) return;
  const md = ai.chat.messages
    .map((m) => `## ${m.role === "user" ? "我" : "AI"}\n\n${m.content}\n`)
    .join("\n");
  await api.exportWrite(dest, Array.from(new TextEncoder().encode(md)));
  s.toast = { msg: "讨论已导出为 Markdown" };
  setTimeout(() => (s.toast = null), 3000);
}
