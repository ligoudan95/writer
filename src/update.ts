/**
 * 自动更新（FR-SET-02，V1.1）：启动时检查（默认开、可关）、非模态通知、
 * 写作中永不弹窗打扰（notify 输入保护）；更新不触碰用户库数据；
 * B10：安装前确认保存并重启（flush → downloadAndInstall → relaunch）。
 */
import { s, flush } from "./store";

export interface UpdateInfo {
  version: string;
  notes: string;
}

let checking = false;

async function getUpdater() {
  const { check } = await import("@tauri-apps/plugin-updater");
  return check();
}

/** 检查更新；manual=true 时总是提示结果，否则仅在发现新版本时通知 */
export async function checkUpdate(manual: boolean): Promise<void> {
  if (checking || !s.ready) return;
  checking = true;
  try {
    const update = await getUpdater();
    if (!update) {
      if (manual) {
        s.toast = { msg: "已是最新版本" };
        setTimeout(() => {
          if (s.toast?.msg === "已是最新版本") s.toast = null;
        }, 3000);
      }
      return;
    }
    const notes = (update.body ?? "").slice(0, 80);
    const { notify } = await import("./notify");
    notify("system", `发现新版本 v${update.version}${notes ? `：${notes}` : ""}`, {
      actionLabel: "查看并安装",
      action: () => void installUpdate(update.version),
      dur: 0,
    });
  } catch {
    // 未配置更新源 / 网络不可达：手动检查时如实提示，自动检查静默
    if (manual) {
      s.toast = { msg: "检查更新失败（未配置更新源或网络不可用）" };
      setTimeout(() => {
        if (s.toast?.msg.startsWith("检查更新失败")) s.toast = null;
      }, 4000);
    }
  } finally {
    checking = false;
  }
}

/** B10：安装前确认（保存全部修改并重启应用）→ flush → 下载安装 → relaunch */
export async function installUpdate(version: string): Promise<void> {
  const { confirm } = await import("@tauri-apps/plugin-dialog");
  const ok = await confirm(
    `将安装 v${version} 并重启应用。\n安装前会自动保存全部未保存的修改（文库数据不受更新影响）。`,
    { title: "安装更新" },
  );
  if (!ok) return;
  s.toast = { msg: "正在下载并安装更新…" };
  try {
    await flush().catch(() => undefined);
    const update = await getUpdater();
    if (!update) {
      s.toast = { msg: "更新不可用，请稍后重试" };
      setTimeout(() => (s.toast = null), 3000);
      return;
    }
    await update.downloadAndInstall();
    const { relaunch } = await import("@tauri-apps/plugin-process");
    await relaunch();
  } catch (e) {
    s.toast = { msg: `更新失败：${(e as { message?: string }).message ?? e}（升级失败不影响当前版本）` };
    setTimeout(() => {
      if (s.toast?.msg.startsWith("更新失败")) s.toast = null;
    }, 6000);
  }
}

/** 启动检查（FR-SET-02：默认仅启动时；延迟 5s 且经通知中心非模态呈现） */
export function scheduleStartupCheck() {
  if (localStorage.getItem("writer.update.auto") === "0") return;
  setTimeout(() => void checkUpdate(false), 5000);
}

export function autoCheckEnabled(): boolean {
  return localStorage.getItem("writer.update.auto") !== "0";
}

export function setAutoCheck(on: boolean) {
  localStorage.setItem("writer.update.auto", on ? "1" : "0");
}
