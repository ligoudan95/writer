<script setup lang="ts">
import { onMounted, ref } from "vue";
import { api, type AiProvider, type AiRequestLog } from "../api";
import { ai, reloadProviders, toggleGlobal } from "../aiflow";
import { s } from "../store";

const editing = ref<null | AiProvider>(null);
const apiKeyInput = ref("");
const testMsg = ref("");
const testing = ref(false);
const models = ref<string[]>([]);
const logRows = ref<AiRequestLog[]>([]);
const showLog = ref(false);
const firstNotice = ref(localStorage.getItem("writer.ai.notice") !== "1");

function blank(): AiProvider {
  return {
    id: "",
    name: "",
    baseUrl: "https://",
    model: "",
    enabled: true,
    stream: true,
    isDefault: false,
  };
}

onMounted(async () => {
  await reloadProviders();
  logRows.value = await api.aiRequestLog(100);
});

function edit(p?: AiProvider) {
  editing.value = p ? { ...p } : blank();
  apiKeyInput.value = "";
  testMsg.value = "";
  models.value = [];
}

async function test() {
  if (!editing.value) return;
  testing.value = true;
  testMsg.value = "";
  try {
    // providerId 传入：key 留空时 Rust 侧回退使用已存 Key（DPAPI）
    const r = await api.aiConnectivityTest(editing.value.id, editing.value.baseUrl, apiKeyInput.value);
    testMsg.value = r.ok ? "✓ 连接成功" : `✗ ${r.message}`;
  } catch (e) {
    testMsg.value = `✗ ${(e as { message?: string }).message ?? String(e)}`;
  } finally {
    testing.value = false;
  }
}

async function fetchModels() {
  if (!editing.value) return;
  // 保存但不关编辑区（datalist 仍挂载）；用 save() 返回的真实 id（新建 Provider 旧 id 为空）
  const saved = await save(false);
  if (!saved) return;
  try {
    models.value = await api.aiModelsFetch(saved.id);
  } catch (e) {
    testMsg.value = `✗ 拉取模型失败：${(e as { message?: string }).message ?? e}`;
  }
}

async function save(closePanel = true): Promise<AiProvider | null> {
  if (!editing.value) return null;
  const saved = await api.aiProviderSave(editing.value, apiKeyInput.value || undefined);
  // 回写 saved（含后端分配的新 id）：否则新建 Provider 下反复保存/拉模型
  // 每次都以空 id 再入库 → 克隆出多个同名 Provider
  editing.value = saved;
  if (saved.isDefault || (!ai.providers.some((p) => p.isDefault) && saved.enabled)) {
    await api.aiProviderSetDefault(saved.id);
  }
  await reloadProviders();
  if (closePanel) editing.value = null;
  return saved;
}

async function remove(id: string) {
  await api.aiProviderDelete(id);
  await reloadProviders();
}

async function markDefault(id: string) {
  await api.aiProviderSetDefault(id);
  await reloadProviders();
}

async function clearLog() {
  await api.aiRequestLogClear();
  logRows.value = [];
}

function ackNotice() {
  localStorage.setItem("writer.ai.notice", "1");
  firstNotice.value = false;
}

async function togglePrivacy() {
  const cur = s.current;
  if (!cur) return;
  const next = !cur.privacy;
  const r = await api.docSetPrivacy(cur.path, next);
  s.current = { ...cur, privacy: next };
  s.lastReadHash = r.hash;
}

const KIND_LABEL: Record<string, string> = { polish: "润色", continue: "续写", chat: "讨论" };
</script>

<template>
  <div class="ai-settings">
    <div class="ai-head">
      <span class="ai-title">Provider 列表</span>
      <span style="flex: 1"></span>
      <button class="btn small ghost" @click="showLog = !showLog">请求日志</button>
    </div>

    <!-- 首次配置数据流向说明（FR-AI-06 信任声明，一次性） -->
    <div v-if="firstNotice" class="ai-notice">
      <p><b>数据流向说明：</b>AI 请求经本软件直连你所配置的 Provider（OpenAI 兼容接口）。API Key 经
        Windows DPAPI 加密存储于本机，不会写入文库、不会上传到任何第三方。请求正文 = 你在面板中看到并确认的上下文；
        对话历史仅保存在本设备。软件自身零遥测。</p>
      <button class="btn small" @click="ackNotice">我已了解</button>
    </div>

    <label class="ai-row">
      <input
        type="checkbox"
        :checked="ai.enabled"
        @change="toggleGlobal()"
      />
      <span>启用 AI 功能（全局开关，关闭后隐藏全部入口）</span>
    </label>

    <div class="ai-list">
      <div v-for="p in ai.providers" :key="p.id" class="ai-prov" :class="{ off: !p.enabled }">
        <span class="ap-name">{{ p.name }}{{ p.isDefault ? " ★默认" : "" }}</span>
        <span class="ap-url" :title="p.baseUrl">{{ p.baseUrl }} · {{ p.model }}</span>
        <span style="flex: 1"></span>
        <button class="btn small ghost" @click="markDefault(p.id)">设默认</button>
        <button class="btn small ghost" @click="edit(p)">编辑</button>
        <button class="btn small ghost danger" @click="remove(p.id)">删除</button>
      </div>
      <button class="btn small" @click="edit()">＋ 添加 Provider</button>
    </div>

    <!-- 编辑区 -->
    <div v-if="editing" class="ai-edit">
      <div class="ai-row">
        <span class="lab">名称</span>
        <input v-model="editing.name" placeholder="如 我的 GLM" />
      </div>
      <div class="ai-row">
        <span class="lab">Base URL</span>
        <input v-model="editing.baseUrl" placeholder="https://api.example.com（OpenAI 兼容根地址）" />
      </div>
      <div class="ai-row">
        <span class="lab">API Key</span>
        <input v-model="apiKeyInput" type="password" placeholder="留空 = 保持已存 Key（DPAPI 加密）" />
      </div>
      <div class="ai-row">
        <span class="lab">默认模型</span>
        <input v-model="editing.model" placeholder="如 glm-4.7" list="ai-models" />
        <datalist id="ai-models">
          <option v-for="m in models" :key="m" :value="m" />
        </datalist>
        <button class="btn small ghost" @click="fetchModels">拉取模型列表</button>
      </div>
      <div class="ai-row">
        <label><input v-model="editing.stream" type="checkbox" /> 流式输出</label>
        <label style="margin-left: 14px"><input v-model="editing.enabled" type="checkbox" /> 启用</label>
      </div>
      <div class="ai-row actions">
        <button class="btn small" :disabled="testing || !editing.baseUrl" @click="test">
          {{ testing ? "测试中…" : "测试连通" }}
        </button>
        <button class="btn small primary" @click="save()">保存</button>
        <button class="btn small ghost" @click="editing = null">取消</button>
        <span v-if="testMsg" :class="{ ok: testMsg.startsWith('✓') }" class="test-msg">{{ testMsg }}</span>
      </div>
    </div>

    <!-- 请求日志（FR-AI-06：时间/Provider/字数，不含内容） -->
    <div v-if="showLog" class="ai-log">
      <div class="sp-head">
        <span>AI 请求日志（本地记录，不含内容）</span>
        <span style="flex: 1"></span>
        <button class="btn small ghost danger" @click="clearLog">清空</button>
      </div>
      <div class="sp-list">
        <div v-for="(r, i) in logRows" :key="i" class="sp-row">
          <span class="sp-time">{{ new Date(r.time * 1000).toLocaleString("zh-CN", { hour12: false }) }}</span>
          <span class="sp-file">{{ r.provider }}</span>
          <span>{{ KIND_LABEL[r.kind] ?? r.kind }}</span>
          <span class="sp-bytes">{{ r.chars }} 字</span>
        </div>
        <div v-if="!logRows.length" class="sp-empty">暂无记录</div>
      </div>
    </div>

    <p class="ai-note">隐私：标记为隐私的文档内容永不作为上下文发送（发送前拦截）；所有请求经 Rust NetGateway 直连 Provider。</p>
    <div v-if="s.current" class="ai-privacy-toggle">
      本文档隐私标记：
      <button class="btn small" @click="togglePrivacy">
        {{ s.current.privacy ? "🔒 已开启（点击关闭）" : "🔓 未开启（点击开启）" }}
      </button>
    </div>
  </div>
</template>
