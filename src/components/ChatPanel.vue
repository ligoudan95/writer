<script setup lang="ts">
import { nextTick, onMounted, ref, watch } from "vue";
import { ai, sendChat, insertReplyToBody, exportChatMd, defaultProvider } from "../aiflow";
import { s } from "../store";

const input = ref("");
const listEl = ref<HTMLElement | null>(null);
const CTX = [
  { id: "none", name: "无上下文" },
  { id: "selection", name: "选中文本" },
  { id: "chapter", name: "当前章节" },
  { id: "doc", name: "当前文档" },
  { id: "paste", name: "手动粘贴" },
];

async function send() {
  const t = input.value.trim();
  if (!t) return;
  input.value = "";
  await sendChat(t);
  await scrollBottom();
}

async function scrollBottom() {
  await nextTick();
  listEl.value?.scrollTo({ top: listEl.value.scrollHeight });
}

onMounted(scrollBottom);
watch(() => ai.chat.messages.length, scrollBottom);
watch(
  () => s.current?.path,
  () => {
    // 会话与文档绑定（FR-AI-04）：切文档即切会话
    if (ai.chatOpen && s.current && ai.chat.docUuid !== (s.current.uuid ?? "")) {
      ai.chat.docUuid = "";
      void import("../aiflow").then((m) => m.openChat());
    }
  },
);

const showPaste = () => ai.chat.contextMode === "paste";

function retryChat() {
  const msgs = ai.chat.messages;
  const lastUser = [...msgs].reverse().find((m) => m.role === "user");
  if (lastUser) void sendChat(lastUser.content);
}
</script>

<template>
  <aside class="chatpanel">
    <div class="cp-head">
      <span class="cp-title">AI 讨论</span>
      <span style="flex: 1"></span>
      <button class="btn small ghost" title="导出为 Markdown（B7）" @click="exportChatMd()">导出</button>
      <button class="icon-btn" @click="ai.chatOpen = false">✕</button>
    </div>
    <div class="cp-ctx">
      <select v-model="ai.chat.contextMode" title="上下文来源">
        <option v-for="c in CTX" :key="c.id" :value="c.id">{{ c.name }}</option>
      </select>
      <textarea
        v-if="showPaste()"
        v-model="ai.chat.pasteText"
        rows="3"
        placeholder="粘贴要讨论的片段…"
      ></textarea>
      <span class="cp-note">
        {{ defaultProvider()?.name ? `发送至 ${defaultProvider()!.name}` : "未配置 Provider" }}
        <template v-if="s.current?.privacy">· ⚠ 隐私文档：文档上下文将被拦截</template>
      </span>
    </div>
    <div ref="listEl" class="cp-list">
      <div v-for="(m, i) in ai.chat.messages" :key="i" class="cp-msg" :class="m.role">
        <div class="cpm-body">{{ m.content }}</div>
        <div v-if="m.role === 'assistant'" class="cpm-ops">
          <button class="btn small ghost" @click="insertReplyToBody(m.content)">插入正文</button>
        </div>
      </div>
      <div v-if="ai.chat.streaming" class="cp-msg assistant">
        <div class="cpm-body streaming">{{ ai.chat.streamText || "…" }}</div>
      </div>
      <div v-if="!ai.chat.messages.length && !ai.chat.streaming" class="cp-empty">
        与 AI 讨论本章情节、结构、文风——会话随文档保留（仅存本设备）
      </div>
    </div>
    <div v-if="ai.chat.error" class="cp-error">
      {{ ai.chat.error }}
      <button class="btn small ghost" @click="retryChat">重试</button>
    </div>
    <div class="cp-input">
      <textarea
        v-model="input"
        rows="2"
        placeholder="输入讨论内容…（Ctrl+Enter 发送）"
        @keydown.enter.exact.prevent="send"
        @keydown.ctrl.enter.prevent="send"
      ></textarea>
      <button class="btn small primary" :disabled="ai.chat.streaming || !defaultProvider()" @click="send">
        {{ ai.chat.streaming ? "回复中…" : "发送" }}
      </button>
    </div>
  </aside>
</template>
