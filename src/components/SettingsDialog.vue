<script setup lang="ts">
/**
 * 设置中心（FR-SET-01）：AI 接口 / 云同步 / 网络 / 外观 / 编辑器排版 / 备份与诊断 / 关于。
 * 接口配置（坚果云 WebDAV、AI Provider、代理出口）内嵌为标签页，不再散落浮层。
 * 应用级偏好（主题/排版/网络出口）跨库共享；同步与 AI Provider 随文库存储。
 */
import { onMounted, ref } from "vue";
import { save } from "@tauri-apps/plugin-dialog";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import { s, setupPeriodicSync, refreshTree } from "../store";
import { api, type NetConfig } from "../api";
import { theme, setTheme, type ThemeMode } from "../theme";
import { layout, FONT_CANDIDATES, DEFAULT_LAYOUT } from "../layout";
import { ai, toggleGlobal } from "../aiflow";
import { checkUpdate, autoCheckEnabled, setAutoCheck } from "../update";
import AiSettings from "./AiSettings.vue";
import SyncSettings from "./SyncSettings.vue";

type Tab = "appearance" | "editor" | "ai" | "sync" | "network" | "backup" | "storage" | "about";
const props = defineProps<{ initialTab?: Tab }>();
const tab = ref<Tab>(props.initialTab ?? "appearance");

// 定时同步间隔（FR-SYN-03 可配置定时；分钟，0=关）
const syncInterval = ref(Number(localStorage.getItem("writer.sync.interval") ?? 5));
function setIntervalMin(v: number) {
  syncInterval.value = v;
  localStorage.setItem("writer.sync.interval", String(v));
  setupPeriodicSync();
}

// -- 网络出口（M5.1） -----------------------------------------------------------
const netMode = ref<NetConfig["mode"]>("direct");
const netUrl = ref("");
const netSaving = ref(false);
const netMsg = ref("");

onMounted(async () => {
  try {
    const cfg = await api.netConfigGet();
    netMode.value = cfg.mode;
    netUrl.value = cfg.url;
  } catch {
    /* 读取失败保持默认显示 */
  }
});

async function saveNet() {
  netSaving.value = true;
  netMsg.value = "";
  try {
    const cfg = await api.netConfigSet(netMode.value, netUrl.value);
    netMode.value = cfg.mode;
    netUrl.value = cfg.url;
    netMsg.value = "✓ 已保存：AI 请求立即走新出口，WebDAV 同步下次批次生效";
  } catch (e) {
    netMsg.value = `✗ 保存失败：${(e as { message?: string }).message ?? e}`;
  } finally {
    netSaving.value = false;
  }
}

// -- 库体检（FR-DOC-07） ----------------------------------------------------------
interface HealthIssue {
  kind: string;
  path: string;
  detail: string;
  fixable: boolean;
}
const healthIssues = ref<HealthIssue[] | null>(null);
const healthBusy = ref(false);
const healthFixReport = ref<string[] | null>(null);

async function runHealthCheck() {
  healthBusy.value = true;
  healthFixReport.value = null;
  try {
    healthIssues.value = await api.healthCheck();
  } finally {
    healthBusy.value = false;
  }
}

async function runHealthFix() {
  if (!healthIssues.value) return;
  healthBusy.value = true;
  try {
    const fixable = healthIssues.value.filter((i) => i.fixable);
    const r = await api.healthFix(fixable);
    healthFixReport.value = [
      `已自动备份：${r.backup}`,
      ...r.fixed.map((f) => `✓ ${f}`),
      ...r.skipped.map((sk) => `△ ${sk}`),
    ];
    healthIssues.value = await api.healthCheck();
  } finally {
    healthBusy.value = false;
  }
}

// -- 存储面板 + 恢复向导（FR-PKG-05 / FR-SAV-05） --------------------------------
const storage = ref<import("../api").StorageInfo | null>(null);
const snapDays = ref(90);
const keepBackups = ref(7);
const restoring = ref<null | { name: string; docs: string[]; picked: Set<string> }>(null);
const restoreBusy = ref(false);
const restoreDone = ref<string[] | null>(null);

async function loadStorage() {
  storage.value = await api.storageInfo();
  snapDays.value = storage.value.snapshotKeepDays;
  keepBackups.value = storage.value.backupKeep;
}

async function doCleanup() {
  const [freed, acts] = await api.storageCleanup();
  s.toast = { msg: `已清理，释放 ${fmtSize(freed)}${acts.length ? "（" + acts.join("；") + "）" : ""}` };
  setTimeout(() => (s.toast = null), 5000);
  await loadStorage();
  await refreshBackups();
}

async function savePolicy() {
  await api.storageSetPolicy(snapDays.value, keepBackups.value);
  s.toast = { msg: "保留策略已保存" };
  setTimeout(() => (s.toast = null), 2500);
}

async function openRestore(name: string) {
  restoreDone.value = null;
  const entry = await api.backupListEntries(name);
  restoring.value = { name, docs: entry.docs, picked: new Set(entry.docs) };
}

async function doRestore(all: boolean) {
  if (!restoring.value) return;
  restoreBusy.value = true;
  try {
    const sel = restoring.value.docs.filter((d) => restoring.value!.picked.has(d));
    const r = await api.backupRestore(restoring.value.name, all, all ? [] : sel);
    restoreDone.value = [`恢复完成（${r.length} 项，恢复前已自动备份当前状态）`];
    restoring.value = null;
    await refreshTree();
  } finally {
    restoreBusy.value = false;
  }
}

const HEALTH_KIND: Record<string, string> = {
  missing_id: "缺文档 id",
  dup_id: "uuid 重复",
  bad_encoding: "编码异常",
  index_stale: "索引过期",
  stale_lock: "锁残留",
  orphan_attachment: "孤儿附件",
  tombstone_corrupt: "墓碑清单损坏",
};

// -- 备份与诊断 ----------------------------------------------------------------
const backups = ref<{ name: string; size: number; ts: number }[]>([]);
const includeAttachments = ref(false);
const backingUp = ref(false);

async function refreshBackups() {
  backups.value = await api.backupList();
}

onMounted(refreshBackups);

async function doBackup(dest?: string) {
  backingUp.value = true;
  try {
    const info = await api.backupNow(includeAttachments.value, dest);
    s.toast = { msg: `备份完成：${info.name}` };
    setTimeout(() => (s.toast = null), 3500);
    await refreshBackups();
  } catch (e) {
    s.toast = { msg: `备份失败：${(e as { message?: string }).message ?? e}` };
    setTimeout(() => (s.toast = null), 6000);
  } finally {
    backingUp.value = false;
  }
}

async function backupTo() {
  const dest = await save({
    title: "备份到…",
    defaultPath: `Writer-backup-${new Date().toISOString().slice(0, 10)}.zip`,
    filters: [{ name: "ZIP", extensions: ["zip"] }],
  });
  if (dest) void doBackup(dest);
}

async function openLogs() {
  try {
    const dir = await api.logsDir();
    await revealItemInDir(dir);
  } catch {
    s.toast = { msg: "打开日志目录失败" };
    setTimeout(() => (s.toast = null), 3000);
  }
}

async function exportDiagnostics() {
  const dest = await save({
    title: "导出诊断包…",
    defaultPath: `Writer-diagnostics-${new Date().toISOString().slice(0, 10)}.zip`,
    filters: [{ name: "ZIP", extensions: ["zip"] }],
  });
  if (!dest) return;
  try {
    await api.diagnosticsExport(dest);
    s.toast = { msg: "诊断包已导出（脱敏，不含正文与凭据）" };
    setTimeout(() => (s.toast = null), 4000);
  } catch (e) {
    s.toast = { msg: `导出失败：${(e as { message?: string }).message ?? e}` };
    setTimeout(() => (s.toast = null), 4000);
  }
}

const fmtSize = (n: number) => (n > 1048576 ? `${(n / 1048576).toFixed(1)} MB` : `${Math.ceil(n / 1024)} KB`);
const fmtTime = (t: number) => {
  const d = new Date(t * 1000);
  return `${d.getMonth() + 1}-${d.getDate()} ${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
};

const emit = defineEmits<{ (e: "close"): void }>();

// -- 更新（FR-SET-02） -----------------------------------------------------------
const updateBusy = ref(false);
const updateAuto = ref(autoCheckEnabled());
async function doCheckUpdate() {
  updateBusy.value = true;
  try {
    await checkUpdate(true);
  } finally {
    updateBusy.value = false;
  }
}
function toggleUpdateAuto() {
  updateAuto.value = !updateAuto.value;
  setAutoCheck(updateAuto.value);
}
</script>

<template>
  <div class="modal-mask" @click.self="emit('close')">
    <div class="settings-dialog">
      <div class="sd-head">
        <span class="sd-title">设置</span>
        <span style="flex: 1"></span>
        <button class="icon-btn" @click="emit('close')">✕</button>
      </div>
      <div class="sd-body">
        <nav class="sd-nav">
          <button :class="{ on: tab === 'ai' }" @click="tab = 'ai'">AI 接口</button>
          <button :class="{ on: tab === 'sync' }" @click="tab = 'sync'">云同步</button>
          <button :class="{ on: tab === 'network' }" @click="tab = 'network'">网络</button>
          <button :class="{ on: tab === 'appearance' }" @click="tab = 'appearance'">外观</button>
          <button :class="{ on: tab === 'editor' }" @click="tab = 'editor'">编辑器</button>
          <button :class="{ on: tab === 'backup' }" @click="tab = 'backup'">备份与诊断</button>
          <button :class="{ on: tab === 'storage' }" @click="tab = 'storage'; loadStorage()">存储与恢复</button>
          <button :class="{ on: tab === 'about' }" @click="tab = 'about'">关于</button>
        </nav>

        <!-- AI 接口（Provider 管理，内嵌面板） -->
        <div v-if="tab === 'ai'" class="sd-pane">
          <AiSettings />
        </div>

        <!-- 云同步（坚果云 / WebDAV，内嵌面板） -->
        <div v-if="tab === 'sync'" class="sd-pane">
          <SyncSettings />
        </div>

        <!-- 网络出口 -->
        <div v-if="tab === 'network'" class="sd-pane">
          <div class="sd-row">
            <span class="sd-label">网络出口</span>
            <div class="seg">
              <button :class="{ on: netMode === 'direct' }" @click="netMode = 'direct'">直连</button>
              <button :class="{ on: netMode === 'system' }" @click="netMode = 'system'">跟随系统代理</button>
              <button :class="{ on: netMode === 'custom' }" @click="netMode = 'custom'">自定义代理</button>
            </div>
          </div>
          <div v-if="netMode === 'custom'" class="sd-row">
            <span class="sd-label">代理地址</span>
            <input
              v-model="netUrl"
              class="sd-input"
              placeholder="http://127.0.0.1:7890"
            />
          </div>
          <div class="sd-row">
            <button class="btn small primary" :disabled="netSaving" @click="saveNet">
              {{ netSaving ? "保存中…" : "保存" }}
            </button>
            <span v-if="netMsg" :class="{ ok: netMsg.startsWith('✓') }" class="test-msg">{{ netMsg }}</span>
          </div>
          <div class="sd-note">
            直连（默认）：不使用 Windows 系统代理。本机代理软件未运行时，系统代理会指向失效端口，
            导致 AI 与云同步全部请求连接失败——直连可避免。访问国内接口（智谱、坚果云等）推荐直连。<br />
            跟随系统代理：经 Windows 设置的代理出站（需代理软件正在运行）。<br />
            自定义代理：所有 AI / WebDAV 请求经指定代理出站。<br />
            本设置对 AI 请求立即生效；WebDAV 同步在下一个批次生效。
          </div>
        </div>

        <!-- 外观 -->
        <div v-if="tab === 'appearance'" class="sd-pane">
          <div class="sd-row">
            <span class="sd-label">主题</span>
            <div class="seg">
              <button
                v-for="m in (['light', 'dark', 'system'] as ThemeMode[])"
                :key="m"
                :class="{ on: theme.mode === m }"
                @click="setTheme(m)"
              >
                {{ m === "light" ? "亮色" : m === "dark" ? "暗色" : "跟随系统" }}
              </button>
            </div>
          </div>
          <div class="sd-row">
            <span class="sd-label">AI 功能（全局）</span>
            <label class="switch">
              <input type="checkbox" :checked="ai.enabled" @change="toggleGlobal()" />
              <span>{{ ai.enabled ? "已启用" : "已关闭（隐藏全部入口）" }}</span>
            </label>
          </div>
          <div class="sd-row">
            <span class="sd-label">定时同步间隔（长开时拉取对端更新）</span>
            <div class="seg">
              <button
                v-for="m in [0, 5, 15, 30]"
                :key="m"
                :class="{ on: syncInterval === m }"
                @click="setIntervalMin(m)"
              >
                {{ m === 0 ? "关" : `${m} 分钟` }}
              </button>
            </div>
          </div>
          <div class="sd-row">
            <span class="sd-label">隐私</span>
            <span class="sd-sub">文档隐私标记在文档标题栏 🔒 与树中设置；标记后任何内容不外发</span>
          </div>
        </div>

        <!-- 编辑器排版（FR-UI-03） -->
        <div v-if="tab === 'editor'" class="sd-pane">
          <div class="sd-row">
            <span class="sd-label">字体</span>
            <select v-model="layout.fontFamily" class="sd-select">
              <option v-for="f in FONT_CANDIDATES" :key="f.id" :value="f.id">{{ f.name }}</option>
            </select>
          </div>
          <div class="sd-row">
            <span class="sd-label">字号（{{ layout.fontSize }}px）</span>
            <input v-model.number="layout.fontSize" type="range" min="13" max="26" step="1" />
          </div>
          <div class="sd-row">
            <span class="sd-label">行距（{{ layout.lineHeight.toFixed(1) }} 倍）</span>
            <input v-model.number="layout.lineHeight" type="range" min="1.3" max="2.6" step="0.1" />
          </div>
          <div class="sd-row">
            <span class="sd-label">段间距（{{ layout.paraGap }}px）</span>
            <input v-model.number="layout.paraGap" type="range" min="0" max="30" step="1" />
          </div>
          <div class="sd-row">
            <span class="sd-label">页宽（{{ layout.pageWidth > 0 ? layout.pageWidth + "px 居中" : "全宽" }}）</span>
            <input v-model.number="layout.pageWidth" type="range" min="0" max="1200" step="20" />
          </div>
          <div class="sd-row">
            <span class="sd-label">首行缩进（排版型：显示与 PDF/DOCX）</span>
            <label class="switch">
              <input v-model="layout.indent" type="checkbox" />
              <span>{{ layout.indent ? "开" : "关" }}</span>
            </label>
          </div>
          <div class="sd-note">
            排版参数即时生效且作为导出默认值（导出模板固定浅色纸面，与主题解耦）。
          </div>
          <div class="sd-row">
            <button class="btn small ghost" @click="Object.assign(layout, DEFAULT_LAYOUT)">恢复默认排版</button>
          </div>
        </div>

        <!-- 备份与诊断（FR-SAV-04 / FR-SET-03） -->
        <div v-if="tab === 'backup'" class="sd-pane">
          <div class="sd-row" style="gap: 8px">
            <button class="btn small primary" :disabled="backingUp" @click="doBackup()">
              {{ backingUp ? "备份中…" : "立即备份（库内滚动，保留 7 份）" }}
            </button>
            <button class="btn small" :disabled="backingUp" @click="backupTo">备份到指定位置…</button>
          </div>
          <div class="sd-row">
            <label class="switch">
              <input v-model="includeAttachments" type="checkbox" />
              <span>备份包含 attachments/（默认排除，减小体积）</span>
            </label>
          </div>
          <div class="sd-sub">每日启动自动备份一次；范围：docs、meta.db、库配置、快照。</div>
          <div v-if="backups.length" class="sd-list">
            <div v-for="b in backups" :key="b.name" class="sd-item">
              <span>{{ b.name }}</span>
              <span class="muted">{{ fmtSize(b.size) }} · {{ fmtTime(b.ts) }}</span>
            </div>
          </div>
          <div v-else class="sd-sub">暂无备份。</div>
          <div class="sd-row" style="gap: 8px; margin-top: 12px">
            <button class="btn small" @click="openLogs">打开日志目录</button>
            <button class="btn small" @click="exportDiagnostics">导出诊断包（脱敏）…</button>
            <button class="btn small" :disabled="healthBusy" @click="runHealthCheck">
              {{ healthBusy ? "体检中…" : "库体检…" }}
            </button>
          </div>
          <!-- 库体检结果（FR-DOC-07） -->
          <div v-if="healthIssues" class="sd-list" style="max-height: 220px">
            <div v-for="(i, idx) in healthIssues" :key="idx" class="sd-item">
              <span :title="i.detail">
                {{ HEALTH_KIND[i.kind] ?? i.kind }} · {{ i.path }}
                {{ i.fixable ? "" : "（不可自动修复）" }}
              </span>
              <span class="muted">{{ i.fixable ? "可修复" : "人工处理" }}</span>
            </div>
            <div v-if="!healthIssues.length" class="sd-item"><span>✓ 一切正常</span><span class="muted">0 项问题</span></div>
          </div>
          <div v-if="healthIssues?.some((i) => i.fixable)" class="sd-row" style="gap: 8px">
            <button class="btn small primary" :disabled="healthBusy" @click="runHealthFix">
              一键修复（先自动备份）
            </button>
          </div>
          <div v-if="healthFixReport" class="sd-list" style="max-height: 160px">
            <div v-for="(line, idx) in healthFixReport" :key="idx" class="sd-item">
              <span>{{ line }}</span>
            </div>
          </div>
          <div class="sd-sub">日志滚动保留 7 天；诊断包不含正文、API Key 与密码。</div>
        </div>

        <!-- 存储与恢复（FR-PKG-05 / FR-SAV-05） -->
        <div v-if="tab === 'storage'" class="sd-pane">
          <template v-if="storage">
            <div class="stat-cards">
              <div class="stat-card"><b>{{ fmtSize(storage.docsBytes) }}</b><span>正文</span></div>
              <div class="stat-card"><b>{{ fmtSize(storage.snapshotsBytes) }}</b><span>快照 ×{{ storage.snapshotsCount }}</span></div>
              <div class="stat-card"><b>{{ fmtSize(storage.backupsBytes) }}</b><span>备份 ×{{ storage.backupsCount }}</span></div>
              <div class="stat-card"><b>{{ fmtSize(storage.trashBytes) }}</b><span>回收站 ×{{ storage.trashCount }}</span></div>
              <div class="stat-card"><b>{{ fmtSize(storage.attachmentsBytes) }}</b><span>附件 ×{{ storage.attachmentsCount }}</span></div>
            </div>
            <div class="sd-row" style="gap: 8px">
              <button class="btn small" @click="doCleanup">一键清理（旧快照 + 旧备份 + 回收站）</button>
              <span class="sd-sub">附件为内容资产不自动清理</span>
            </div>
            <div class="sd-row" style="gap: 8px">
              <span class="sd-label" style="min-width: auto">快照保留</span>
              <input v-model.number="snapDays" class="sd-input" type="number" min="7" max="3650" style="width:90px" /> 天
              <span class="sd-label" style="min-width: auto">备份保留</span>
              <input v-model.number="keepBackups" class="sd-input" type="number" min="1" max="30" style="width:70px" /> 份
              <button class="btn small" @click="savePolicy">保存策略</button>
            </div>
          </template>

          <div class="sd-sub" style="margin-top: 6px"><b>备份恢复向导</b>：恢复前会自动把当前状态再备份一份</div>
          <div v-if="backups.length" class="sd-list" style="max-height: 180px">
            <div v-for="b in backups" :key="b.name" class="sd-item">
              <span>{{ b.name }}</span>
              <span class="muted">{{ fmtSize(b.size) }} · {{ fmtTime(b.ts) }} ·
                <button class="btn small ghost" @click="openRestore(b.name)">恢复…</button>
              </span>
            </div>
          </div>
          <div v-else class="sd-sub">暂无备份（每日启动自动产生）。</div>

          <!-- 恢复确认（FR-SAV-05：预览 + 整库/勾选） -->
          <div v-if="restoring" class="ai-edit" style="margin-top: 8px">
            <div class="sd-sub">从 {{ restoring.name }} 恢复（{{ restoring.docs.length }} 篇）——勾选单篇或整库：</div>
            <div class="sd-list" style="max-height: 180px">
              <label v-for="d in restoring.docs" :key="d" class="sd-item" style="cursor: pointer">
                <input
                  type="checkbox"
                  :checked="restoring.picked.has(d)"
                  @change="restoring.picked.has(d) ? restoring.picked.delete(d) : restoring.picked.add(d); restoring.picked = new Set(restoring.picked)"
                />
                <span style="margin-left: 6px">{{ d }}</span>
              </label>
            </div>
            <div class="sd-row" style="gap: 8px; margin-top: 8px">
              <button class="btn small primary" :disabled="restoreBusy" @click="doRestore(true)">整库恢复</button>
              <button class="btn small" :disabled="restoreBusy || !restoring.picked.size" @click="doRestore(false)">
                恢复勾选的 {{ restoring.picked.size }} 篇
              </button>
              <button class="btn small ghost" @click="restoring = null">取消</button>
            </div>
          </div>
          <div v-if="restoreDone" class="sd-sub" style="color: #2e7d32">{{ restoreDone[0] }}</div>
        </div>

        <!-- 关于（FR-PKG-07 组件清单 + 信任声明；FR-SET-02 更新入口） -->
        <div v-if="tab === 'about'" class="sd-pane">
          <div class="sd-about">
            <div class="sd-name">Writer</div>
            <div class="muted">本地优先的个人写作软件 · V1.0</div>
          </div>
          <div class="sd-row" style="gap: 8px">
            <button class="btn small" :disabled="updateBusy" @click="doCheckUpdate">
              {{ updateBusy ? "检查中…" : "检查更新" }}
            </button>
            <label class="switch">
              <input type="checkbox" :checked="updateAuto" @change="toggleUpdateAuto()" />
              <span>启动时自动检查更新</span>
            </label>
          </div>
          <div class="sd-sub">
            更新提示一律非模态（通知中心），写作中永不打扰；安装前自动保存全部修改，
            升级失败自动回滚，绝不触碰文库数据。
          </div>
          <div class="sd-sub">
            本地优先：全部文稿以明文 Markdown 存储在你的文库目录，随时可用任何编辑器打开迁出。
            AI 与 WebDAV 凭据经 Windows DPAPI 加密仅存本机；除你配置的 AI 服务与网盘外，本软件不连接任何服务器。
          </div>
          <div class="sd-sub"><b>开源组件</b>：Tauri 2、Vue 3、TipTap、Paged.js、docx、marked、blake3、SQLite、reqwest 等的完整许可文本见安装目录 LICENSES/。</div>
        </div>
      </div>
    </div>
  </div>
</template>
