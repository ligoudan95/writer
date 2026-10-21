/**
 * 主题（FR-UI-01）：亮 / 暗 / 跟随系统三档。
 * data-theme 挂在 <html> 上，CSS 变量两套配色见 style.css；
 * 稿纸模式配色跟随主题（v1.7.2 定案：明暗主题各配色）。
 */
import { reactive } from "vue";

export type ThemeMode = "light" | "dark" | "system";

function loadMode(): ThemeMode {
  const v = localStorage.getItem("writer.theme");
  return v === "dark" || v === "light" || v === "system" ? v : "system";
}

export const theme = reactive({
  mode: loadMode() as ThemeMode,
  /** 系统当前明暗（响应式：matchMedia 监听维护——模板消费方可随系统切换刷新） */
  systemDarkNow: window.matchMedia("(prefers-color-scheme: dark)").matches,
});

function systemDark(): boolean {
  return theme.systemDarkNow;
}

export function effectiveTheme(): "light" | "dark" {
  return theme.mode === "system" ? (systemDark() ? "dark" : "light") : theme.mode;
}

function apply() {
  document.documentElement.dataset.theme = effectiveTheme();
}

export function setTheme(mode: ThemeMode) {
  theme.mode = mode;
  localStorage.setItem("writer.theme", mode);
  apply();
}

let media: MediaQueryList | null = null;
export function initTheme() {
  apply();
  media = window.matchMedia("(prefers-color-scheme: dark)");
  media.addEventListener("change", (e) => {
    theme.systemDarkNow = e.matches; // 响应式传播（状态栏图标等即时跟随）
    if (theme.mode === "system") apply();
  });
}
