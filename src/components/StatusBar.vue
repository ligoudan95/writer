<script setup lang="ts">
import { ref } from "vue";
import { s } from "../store";

type ThemeMode = "light" | "dark" | "system";
const mode = ref<ThemeMode>((localStorage.getItem("writer.theme") as ThemeMode) || "system");

function applyTheme() {
  const dark =
    mode.value === "dark" ||
    (mode.value === "system" && window.matchMedia("(prefers-color-scheme: dark)").matches);
  document.documentElement.dataset.theme = dark ? "dark" : "light";
}
applyTheme();
window.matchMedia("(prefers-color-scheme: dark)").addEventListener("change", applyTheme);

function cycle() {
  mode.value = mode.value === "system" ? "light" : mode.value === "light" ? "dark" : "system";
  localStorage.setItem("writer.theme", mode.value);
  applyTheme();
}
</script>

<template>
  <footer class="statusbar">
    <span class="sb-item">{{ s.counts.words }} 字</span>
    <span class="sb-item">{{ s.counts.chars }} 字符</span>
    <span class="sb-item">{{ s.counts.paragraphs }} 段</span>
    <span class="sb-grow" />
    <span v-if="s.root" class="sb-item dim" :title="s.root">{{ s.root }}</span>
    <button class="icon-btn" :title="`主题：${mode}（点击切换）`" @click="cycle()">
      {{ mode === "dark" ? "🌙" : mode === "light" ? "☀" : "◐" }}
    </button>
  </footer>
</template>
