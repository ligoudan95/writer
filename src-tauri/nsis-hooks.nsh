; NSIS 安装器钩子（FR-PKG-03 卸载数据红线）
; 卸载只删除应用数据，绝不触碰任何文库目录（默认位于 文档\Writer\，随库完整保留，
; 可被重装直接打开）：
;   %APPDATA%\Writer        —— 应用配置（config_dir）
;   %LOCALAPPDATA%\com.writer.desktop —— WebView2 数据（localStorage：会话/主题/排版记忆）与缓存
!macro NSIS_HOOK_POSTUNINSTALL
  RMDir /r "$APPDATA\Writer"
  RMDir /r "$LOCALAPPDATA\com.writer.desktop"
  RMDir /r "$LOCALAPPDATA\com.writer.app" ; 旧 identifier（1.0.0 首发前）残留清理
!macroend
