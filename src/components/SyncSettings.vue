<script setup lang="ts">
import { onMounted, ref } from "vue";
import { api, type SyncConfig } from "../api";
import { s, triggerSync } from "../store";

const TEMPLATES = [
  { name: "坚果云", url: "https://dav.jianguoyun.com/dav/", root: "writer" },
  { name: "自定义", url: "", root: "" },
];

const enabled = ref(false);
const url = ref("");
const username = ref("");
const password = ref("");
const remoteRoot = ref("");
const autoSync = ref(true);
const hasPassword = ref(false);
const bound = ref(false);
const testing = ref(false);
const testMsg = ref("");
const saving = ref(false);

onMounted(async () => {
  const cfg = await api.syncConfigGet();
  applyCfg(cfg);
});

function applyCfg(cfg: SyncConfig) {
  enabled.value = cfg.enabled;
  url.value = cfg.url;
  username.value = cfg.username;
  remoteRoot.value = cfg.remoteRoot;
  autoSync.value = cfg.autoSync;
  hasPassword.value = cfg.hasPassword;
  bound.value = cfg.bound;
}

function useTemplate(t: (typeof TEMPLATES)[number]) {
  if (t.url) {
    url.value = t.url;
    if (t.root && !remoteRoot.value) remoteRoot.value = t.root;
  }
}

async function test() {
  testing.value = true;
  testMsg.value = "";
  try {
    const r = await api.syncTest(
      url.value,
      username.value,
      password.value || undefined,
      remoteRoot.value,
    );
    testMsg.value = r.ok ? "✓ 连接成功" : `✗ ${r.message}`;
  } catch (e) {
    const err = e as { message?: string };
    testMsg.value = `✗ ${err.message ?? String(e)}`;
  } finally {
    testing.value = false;
  }
}

async function save() {
  saving.value = true;
  try {
    const cfg = await api.syncConfigSet({
      enabled: enabled.value,
      url: url.value,
      username: username.value,
      password: password.value || undefined,
      remoteRoot: remoteRoot.value,
      autoSync: autoSync.value,
    });
    applyCfg(cfg);
    s.sync.autoSync = cfg.autoSync; // 立即生效（scheduleAutoSync 消费）
    password.value = "";
    if (cfg.enabled) void triggerSync();
  } finally {
    saving.value = false;
  }
}

function close() {
  s.sync.settingsOpen = false;
}
</script>

<template>
  <div class="sync-settings">
    <div class="sync-head">
      <span class="sync-title">云同步（WebDAV）</span>
      <span style="flex: 1"></span>
      <button class="btn small" :disabled="!enabled" @click="triggerSync()">立即同步</button>
      <button class="icon-btn" title="关闭" @click="close">✕</button>
    </div>

    <!-- 双引擎检测（FR-SYN-01） -->
    <div v-if="s.sync.thirdpartyWarnings.length" class="sync-warn">
      检测到文库位于第三方同步盘（{{ s.sync.thirdpartyWarnings.join("、") }}）管理范围内：
      两套同步引擎管理同一批文件会互相冲突，建议将文库移出该目录或停用其一。
    </div>

    <div class="sync-form">
      <label class="row">
        <input v-model="enabled" type="checkbox" />
        <span>启用同步</span>
      </label>
      <div class="row providers">
        <button
          v-for="t in TEMPLATES"
          :key="t.name"
          class="btn small ghost"
          type="button"
          @click="useTemplate(t)"
        >
          {{ t.name }}
        </button>
      </div>
      <label class="row">
        <span class="lab">服务器</span>
        <input v-model="url" placeholder="https://dav.jianguoyun.com/dav/" :disabled="!enabled" />
      </label>
      <label class="row">
        <span class="lab">账号</span>
        <input v-model="username" placeholder="邮箱 / 用户名" :disabled="!enabled" />
      </label>
      <label class="row">
        <span class="lab">{{ hasPassword ? "应用密码（留空保持不变）" : "应用密码" }}</span>
        <input
          v-model="password"
          type="password"
          placeholder="坚果云请使用应用密码，而非登录密码"
          :disabled="!enabled"
        />
      </label>
      <label class="row">
        <span class="lab">远程目录</span>
        <input v-model="remoteRoot" placeholder="如 writer（库将同步到该目录下）" :disabled="!enabled" />
      </label>
      <label class="row">
        <input v-model="autoSync" type="checkbox" :disabled="!enabled" />
        <span>保存后自动同步（10 秒去抖）</span>
      </label>

      <div class="row actions">
        <button class="btn small" :disabled="!enabled || testing || !url || !username" @click="test">
          {{ testing ? "测试中…" : "测试连接" }}
        </button>
        <button class="btn small primary" :disabled="saving" @click="save">保存</button>
        <span v-if="testMsg" class="test-msg" :class="{ ok: testMsg.startsWith('✓') }">{{ testMsg }}</span>
        <span v-if="bound" class="bound-tag" title="首次绑定方向已确认">已绑定</span>
      </div>
    </div>

    <p class="sync-note">
      凭据经 Windows DPAPI 加密存储于本机，绝不写入库文件、不上传（FR-SYN-01）。
      同步范围：docs/ 与 attachments/；本地数据库、快照、回收站不上云（FR-SYN-02）。
    </p>
  </div>
</template>
