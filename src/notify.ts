/**
 * 通知中心与打扰治理（FR-UI-06）：
 * - 模态白名单之外的一切通知非模态（s.toast 横幅），并沉淀到通知中心历史；
 * - 输入保护：最近 3 秒内有键入时，非紧急通知自动延后到输入停顿再呈现；
 * - 撤销窗口保护：删除撤销类 toast（带 undo 按钮）展示期间不被通知横幅顶掉；
 * - 保存/同步常规状态仅状态栏，不进通知中心；
 * - 通知同时落库内日志（FR-SET-03 前后端同文件，best-effort）。
 */
import { reactive } from "vue";
import { s } from "./store";

export interface Notice {
  id: number;
  time: number;
  msg: string;
  kind: "sync" | "ai" | "save" | "system" | "conflict";
  /** 跳转动作（通知中心可点）：如打开同步面板/冲突处理 */
  actionLabel?: string;
  action?: () => void;
}

let nextId = 1;
let lastKeystroke = 0;
const pending: Array<{ msg: string; dur: number }> = [];
let bannerTimer: number | undefined;
let retryTimer: number | undefined;

export const notifyCenter = reactive({
  items: [] as Notice[],
  open: false,
  unread: 0,
});

/** 输入保护：编辑器每次 keydown 调用（3 秒内不打扰，FR-UI-06 AC） */
export function markTyping() {
  lastKeystroke = Date.now();
  scheduleFlush();
}

function typingRecently(): boolean {
  return Date.now() - lastKeystroke < 3000;
}

function logToDisk(kind: Notice["kind"], msg: string) {
  // 落日志（FR-SET-03：前端与 Rust 同文件）；失败静默（通知本身不能因日志失败而丢）
  void import("./api")
    .then((m) => m.api.logWrite(kind === "conflict" ? "warn" : "info", "", msg))
    .catch(() => undefined);
}

function showBanner(msg: string, dur: number) {
  // 撤销窗口保护：当前横幅带撤销按钮时，通知排队等待（FR-DOC-03 5 秒撤销窗口不被顶掉）
  if (s.toast?.undo) {
    pending.push({ msg, dur });
    scheduleFlush();
    return;
  }
  s.toast = { msg };
  clearTimeout(bannerTimer);
  bannerTimer = window.setTimeout(() => {
    if (s.toast?.msg === msg) s.toast = null;
    tryFlushPending();
  }, dur);
}

function tryFlushPending() {
  if (!pending.length || typingRecently()) {
    if (pending.length) scheduleFlush();
    return;
  }
  const next = pending.shift();
  if (next) showBanner(next.msg, next.dur);
}

/** 输入停止 3.3s 后冲刷队列（markTyping 每次重挂——持续输入不会耗尽兜底定时器） */
function scheduleFlush() {
  clearTimeout(retryTimer);
  retryTimer = window.setTimeout(() => tryFlushPending(), 3300);
}

/**
 * 非模态通知：横幅即时呈现（输入中则延后）+ 沉淀通知中心 + 落日志。
 * kind=conflict 视为非紧急（用户输入保护优先，停顿后必达）。
 */
export function notify(
  kind: Notice["kind"],
  msg: string,
  opts?: { actionLabel?: string; action?: () => void; dur?: number },
) {
  notifyCenter.items.unshift({
    id: nextId++,
    time: Date.now(),
    msg,
    kind,
    actionLabel: opts?.actionLabel,
    action: opts?.action,
  });
  if (notifyCenter.items.length > 100) notifyCenter.items.pop();
  if (!notifyCenter.open) notifyCenter.unread++;
  logToDisk(kind, msg);
  const dur = opts?.dur ?? 5000;
  if (typingRecently()) {
    pending.push({ msg, dur });
    scheduleFlush();
  } else {
    showBanner(msg, dur);
  }
}

export function openCenter() {
  notifyCenter.open = true;
  notifyCenter.unread = 0;
}

export function closeCenter() {
  notifyCenter.open = false;
}

export function clearCenter() {
  notifyCenter.items = [];
  notifyCenter.unread = 0;
}

/** 撤销类 toast 到期让位横幅槽位时调用：冲刷排队中的通知 */
export function onToastSlotFreed() {
  tryFlushPending();
}
