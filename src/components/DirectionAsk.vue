<script setup lang="ts">
import { chooseDirection, s } from "../store";

/** 首次绑定方向选择（FR-SYN-07）：本地与远端均有内容时必须显式选择；
 * 模态弹窗白名单成员（3.3-5）。选择前引擎零写操作。 */
</script>

<template>
  <div v-if="s.sync.directionAsk" class="dir-ask-mask">
    <div class="dir-ask">
      <h3>首次同步：请选择方向</h3>
      <p class="da-desc">
        本地有 <b>{{ s.sync.directionAsk.localDocs }}</b> 篇文档，远端目录有
        <b>{{ s.sync.directionAsk.remoteDocs }}</b> 篇文档。两端均有内容，请选择合并方向——
        在你选择之前，不会写入任何一端。
      </p>
      <div class="da-actions">
        <button class="btn" @click="chooseDirection('upload')">
          上传本地覆盖远端
          <small>远端多余文件将被删除</small>
        </button>
        <button class="btn" @click="chooseDirection('download')">
          下载远端覆盖本地
          <small>本地多余文档移入回收站（可恢复）</small>
        </button>
      </div>
      <p class="da-hint">
        两端均有同名/同 id 文档时，内容不同者会按冲突流程保留两个版本，不会静默覆盖。
      </p>
    </div>
  </div>
</template>
