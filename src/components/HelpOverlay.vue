<script setup lang="ts">
/** 帮助（FR-UI-07）：F1 本地内置——快捷键速查 + 上手指引（断网可用） */
const emit = defineEmits<{ (e: "close"): void }>();

const KEYS: [string, string][] = [
  ["新建文档", "Ctrl+N"],
  ["保存", "Ctrl+S"],
  ["手动同步", "Ctrl+Shift+S"],
  ["标题 H1–H6", "Ctrl+1 ~ Ctrl+6"],
  ["加粗 / 斜体 / 下划线 / 删除线", "Ctrl+B / I / U / Shift+X"],
  ["撤销 / 重做", "Ctrl+Z / Ctrl+Y"],
  ["导出", "Ctrl+E"],
  ["AI 润色（选中文本）", "Ctrl+Shift+P"],
  ["AI 续写（草稿中 Tab 接受 / Esc 放弃）", "Ctrl+Shift+Enter"],
  ["AI 讨论", "Ctrl+Shift+A"],
  ["全文搜索", "Ctrl+Shift+F"],
  ["粘贴为纯文本", "Ctrl+Shift+V"],
  ["专注模式", "F11"],
  ["缩进 / 取消缩进", "Tab / Shift+Tab"],
  ["打开链接", "Ctrl+点击"],
  ["帮助", "F1"],
];
</script>

<template>
  <div class="modal-mask" @click.self="emit('close')">
    <div class="help-overlay">
      <div class="nc-head">
        <span>帮助与快捷键</span>
        <span style="flex: 1"></span>
        <button class="icon-btn" @click="emit('close')">✕</button>
      </div>
      <div class="help-body">
        <h4>三分钟上手</h4>
        <ul>
          <li>左侧文库新建文档/文件夹；正文自动保存（状态栏可见），断电不丢稿</li>
          <li>工具栏完成全部格式；表格有浮动工具条；稿纸模式适合中文创作</li>
          <li>导出：Ctrl+E（TXT / MD / Word / PDF）；云同步与 AI 在设置中配置</li>
          <li>文稿是明文 Markdown：随时用任何编辑器打开，不绑架数据</li>
        </ul>
        <h4>快捷键</h4>
        <table class="help-keys">
          <tr v-for="[k, v] in KEYS" :key="k">
            <td class="muted">{{ k }}</td>
            <td><code>{{ v }}</code></td>
          </tr>
        </table>
      </div>
    </div>
  </div>
</template>
