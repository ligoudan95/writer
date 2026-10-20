<script setup lang="ts">
import { computed } from "vue";
import { ai, POLISH_STYLES, applyPolish, resetPolish, startPolish, cancelRunning, defaultProvider } from "../aiflow";
import { s } from "../store";

const provider = () => defaultProvider();
const acceptedCount = computed(() => ai.polish.accepts.filter(Boolean).length);
const ctxChars = computed(() => ai.polish.original.length);
</script>

<template>
  <!-- 触发面板：选区就绪、未运行 -->
  <div v-if="ai.polish.state === 'idle' && ai.polish.error === ''" class="polish-panel">
    <div class="pp-head">
      <span class="pp-title">AI 润色</span>
      <span style="flex: 1"></span>
      <button class="icon-btn" title="关闭" @click="resetPolish()">✕</button>
    </div>
    <div class="pp-body">
      <div class="pp-row">
        <label class="lab">风格</label>
        <select v-model="ai.polish.style">
          <option v-for="st in POLISH_STYLES" :key="st.id" :value="st.id">{{ st.name }}</option>
        </select>
      </div>
      <div class="pp-row">
        <label class="lab">附加要求</label>
        <input v-model="ai.polish.extra" placeholder="自由补充（可选）" />
      </div>
      <div class="pp-row ctx">
        将发送：选中文本 {{ ctxChars }} 字 → <b>{{ provider()?.name ?? "未配置" }}</b>
        <template v-if="provider()"> · {{ provider()?.model }}</template>
        <span v-if="s.current?.privacy" class="pp-block">⚠ 隐私文档：将被拦截</span>
      </div>
      <div class="pp-row actions">
        <button class="btn small primary" :disabled="!provider()" @click="startPolish()">开始润色</button>
      </div>
    </div>
  </div>

  <!-- 处理中：非模态状态条，任意按键取消（FR-AI-02 固定交互） -->
  <div v-else-if="ai.polish.state === 'running'" class="polish-panel running">
    <span class="pp-spinner">⟳</span> 润色处理中（选区已临时锁定）… 在编辑器中按任意键取消
  </div>

  <!-- 审阅：diff 视图，逐块接受/拒绝（AC A5） -->
  <div v-else-if="ai.polish.state === 'review'" class="polish-panel review">
    <div class="pp-head">
      <span class="pp-title">润色建议 · {{ ai.polish.blocks.length }} 个修改块</span>
      <span style="flex: 1"></span>
      <button class="btn small" @click="applyPolish(true)">应用已接受（{{ acceptedCount }}/{{ ai.polish.blocks.length }}）</button>
      <button class="btn small" @click="applyPolish(false)">全部应用</button>
      <button class="btn small ghost" @click="resetPolish()">放弃</button>
    </div>
    <div class="pp-blocks">
      <label v-for="(b, i) in ai.polish.blocks" :key="i" class="pp-block" :class="{ rejected: !ai.polish.accepts[i] }">
        <div class="ppb-head">
          <input v-model="ai.polish.accepts[i]" type="checkbox" />
          <span class="ppb-ctx">…{{ b.ctxBefore }}</span>
        </div>
        <div class="ppb-diff">
          <div v-if="b.before" class="del">− {{ b.before }}</div>
          <div v-if="b.after" class="ins">＋ {{ b.after }}</div>
        </div>
        <div class="ppb-ctx after">{{ b.ctxAfter }}…</div>
      </label>
    </div>
    <p class="pp-hint">应用前自动保存 pre-AI 快照；应用后一次 Ctrl+Z 可整体还原。</p>
  </div>

  <!-- 错误条 -->
  <div v-if="ai.polish.error" class="polish-panel error" @click="ai.polish.error = ''">
    润色失败：{{ ai.polish.error }}（点击关闭）
    <button class="btn small ghost" @click.stop="startPolish()">重试</button>
    <button class="btn small ghost" @click.stop="cancelRunning('polish')">取消请求</button>
  </div>
</template>
