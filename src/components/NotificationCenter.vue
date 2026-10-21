<script setup lang="ts">
/** 通知中心（FR-UI-06）：右下角通知历史聚合沉淀，可点击跳转处理页 */
import { notifyCenter, closeCenter, clearCenter } from "../notify";

const KIND_LABEL: Record<string, string> = {
  sync: "同步",
  ai: "AI",
  save: "保存",
  system: "系统",
  conflict: "冲突",
};

function fmtTime(t: number) {
  const d = new Date(t);
  const today = new Date();
  const same = d.toDateString() === today.toDateString();
  const hm = `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
  return same ? hm : `${d.getMonth() + 1}-${d.getDate()} ${hm}`;
}
</script>

<template>
  <div v-if="notifyCenter.open" class="modal-mask transparent" @click.self="closeCenter()">
    <div class="notify-center">
      <div class="nc-head">
        <span>通知中心</span>
        <span style="flex: 1"></span>
        <button class="btn small ghost" @click="clearCenter()">清空</button>
        <button class="icon-btn" @click="closeCenter()">✕</button>
      </div>
      <div class="nc-list">
        <div v-for="n in notifyCenter.items" :key="n.id" class="nc-item" :class="n.kind">
          <span class="nc-kind">{{ KIND_LABEL[n.kind] ?? n.kind }}</span>
          <span class="nc-msg">{{ n.msg }}</span>
          <span class="nc-time">{{ fmtTime(n.time) }}</span>
          <button v-if="n.actionLabel" class="btn small" @click="n.action?.(); closeCenter()">
            {{ n.actionLabel }}
          </button>
        </div>
        <div v-if="!notifyCenter.items.length" class="nc-empty">暂无通知</div>
      </div>
    </div>
  </div>
</template>
