<script setup lang="ts">
/** 写作统计面板（FR-STAT-01/02，V1.1）：今日进度、连续天数、近一年热力图、目标设置 */
import { computed, onMounted, ref } from "vue";
import { api, type StatsSummary } from "../api";

const sum = ref<StatsSummary | null>(null);
const goalInput = ref("");
const emit = defineEmits<{ (e: "close"): void }>();

async function load() {
  sum.value = await api.statsSummary();
  goalInput.value = String(sum.value.goal);
}

onMounted(load);

async function saveGoal() {
  const g = Math.max(100, Math.min(50000, Number(goalInput.value) || 1000));
  goalInput.value = String(g);
  await api.statsSetGoal(g);
  await load();
}

/** 热力图网格：近 371 天（53 周），按周列排布；值 → 4 级色阶 */
interface Cell {
  date: string;
  words: number;
  level: 0 | 1 | 2 | 3 | 4;
}
const grid = computed<Cell[]>(() => {
  if (!sum.value) return [];
  const map = new Map(sum.value.days);
  const today = new Date();
  const cells: Cell[] = [];
  // 从 52 周前的周一开始
  const end = new Date(today);
  const start = new Date(today);
  start.setDate(start.getDate() - 370);
  start.setDate(start.getDate() - ((start.getDay() + 6) % 7)); // 对齐周一
  for (let d = new Date(start); d <= end; d.setDate(d.getDate() + 1)) {
    const key = `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
    const w = map.get(key) ?? 0;
    const goal = sum.value?.goal ?? 1000;
    const level: Cell["level"] = w <= 0 ? 0 : w < goal * 0.25 ? 1 : w < goal * 0.5 ? 2 : w < goal ? 3 : 4;
    cells.push({ date: key, words: w, level });
  }
  return cells;
});

const fmtNum = (n: number) => n.toLocaleString("zh-CN");
</script>

<template>
  <div class="modal-mask" @click.self="emit('close')">
    <div class="stats-panel">
      <div class="sd-head">
        <span class="sd-title">写作统计</span>
        <span style="flex: 1"></span>
        <button class="icon-btn" @click="emit('close')">✕</button>
      </div>
      <div v-if="sum" class="stats-body">
        <!-- 今日进度（FR-STAT-01） -->
        <div class="stat-today">
          <div class="stat-progress">
            <div class="stat-progress-bar" :style="{ width: `${Math.min(100, (sum.today / sum.goal) * 100)}%` }" />
          </div>
          <div class="stat-progress-text">
            今日 <b>{{ fmtNum(sum.today) }}</b> / {{ fmtNum(sum.goal) }} 字
            <span v-if="sum.today >= sum.goal" class="stat-done">✓ 已达成</span>
          </div>
        </div>

        <div class="stat-cards">
          <div class="stat-card"><b>{{ sum.streak }}</b><span>连续天数</span></div>
          <div class="stat-card"><b>{{ fmtNum(sum.weekTotal) }}</b><span>本周字数</span></div>
          <div class="stat-card"><b>{{ fmtNum(sum.monthTotal) }}</b><span>本月字数</span></div>
          <div class="stat-card"><b>{{ sum.activeDays }}</b><span>活跃天数</span></div>
        </div>

        <!-- 写作日历热力图（近一年，FR-STAT-02） -->
        <div class="stat-heat-wrap">
          <div class="stat-heat">
            <div
              v-for="c in grid"
              :key="c.date"
              class="heat-cell"
              :class="`lv${c.level}`"
              :title="`${c.date} · ${c.words} 字`"
            />
          </div>
          <div class="heat-legend">
            少
            <span class="heat-cell lv0" /><span class="heat-cell lv1" /><span class="heat-cell lv2" />
            <span class="heat-cell lv3" /><span class="heat-cell lv4" />
            多
          </div>
        </div>

        <div class="stat-goal-row">
          <span>每日目标（字）</span>
          <input v-model="goalInput" class="sd-input" type="number" min="100" max="50000" step="100" />
          <button class="btn small" @click="saveGoal">保存</button>
        </div>
        <div class="sd-sub">
          统计口径：净增字数（回删不计负）；多设备写作日历按设备并集聚合，跨设备不断连。
        </div>
      </div>
      <div v-else class="sd-sub" style="padding: 30px; text-align: center">加载中…</div>
    </div>
  </div>
</template>
