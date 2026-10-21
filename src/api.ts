import { invoke } from "@tauri-apps/api/core";

export interface TreeNode {
  id: string;
  name: string;
  kind: "folder" | "doc";
  privacy?: boolean;
  children?: TreeNode[];
}

export interface DocPayload {
  path: string;
  title: string;
  body: string;
  hash: string;
  privacy?: boolean;
  uuid?: string;
}

export interface WriteResult {
  path: string;
  hash: string;
  savedAt: number;
}

export interface StartupInfo {
  root: string;
  fresh: boolean;
  pending: DocPayload[];
  thirdpartyWarnings?: string[];
  auditWarnings?: string[];
  tookOverStaleLock?: boolean;
}
export interface TrashEntry {
  name: string;
  origName: string;
  origRel?: string;
  isDir: boolean;
  deletedAt: number;
  size: number;
  over30d: boolean;
}
export interface BackupInfo {
  name: string;
  size: number;
  ts: number;
}

export interface SnapshotInfo {
  id: string;
  ts: number;
  source: string;
  note: string;
  words: number;
}

export interface ImportedFile {
  name: string;
  content: string;
}

// -- 云同步（FR-SYN，M3） ------------------------------------------------------

export interface SyncConfig {
  enabled: boolean;
  url: string;
  username: string;
  remoteRoot: string;
  hasPassword: boolean;
  bound: boolean;
  autoSync: boolean;
  thirdpartyWarnings: string[];
}
export interface DavTestResult {
  ok: boolean;
  message: string;
}
export interface SyncConflict {
  uuid: string;
  path: string;
  copyPath: string;
}
export interface SyncReport {
  uploaded: number;
  downloaded: number;
  moved: number;
  deletedRemote: number;
  trashedLocal: number;
  conflicts: SyncConflict[];
  bytesUp: number;
  bytesDown: number;
  warnings: string[];
  /** 库标识（切库后旧批次事件过滤用） */
  libRoot?: string;
}
export interface SyncStatus {
  running: boolean;
  phase: string;
  last: SyncReport | null;
  conflicts: SyncConflict[];
}
export interface SyncLogRow {
  time: number;
  file: string;
  direction: string;
  bytes: number;
  action: string;
  result: string;
}

// -- AI（FR-AI，M4） ------------------------------------------------------------

export interface AiProvider {
  id: string;
  name: string;
  baseUrl: string;
  model: string;
  enabled: boolean;
  stream: boolean;
  isDefault: boolean;
}
export interface ChatMsg {
  role: "system" | "user" | "assistant";
  content: string;
}
export interface AiConversation {
  id: string;
  docUuid: string;
  title: string;
  createdAt: number;
  updatedAt: number;
}
export interface AiMessage {
  id: number;
  convId: string;
  role: string;
  content: string;
  createdAt: number;
}
export interface AiRequestLog {
  time: number;
  provider: string;
  chars: number;
  kind: string;
}

// -- 网络出口（M5.1：AI 与 WebDAV 共用；默认直连绕过系统代理） ------------------

export interface NetConfig {
  mode: "direct" | "system" | "custom";
  url: string;
}

const call = <T>(cmd: string, args?: Record<string, unknown>): Promise<T> =>
  invoke<T>(cmd, args);

/** 类型化 command 封装（5.3 契约的前端门面；NFR-16：前端不持有泛型 fs 能力） */
export const api = {
  startup: () => call<StartupInfo>("library_startup"),
  libraryRecentList: () => call<string[]>("library_recent_list"),
  libraryOpen: (path: string) => call<StartupInfo>("library_open", { path }),
  libraryCreate: (path: string) => call<StartupInfo>("library_create", { path }),
  list: () => call<TreeNode[]>("doc_list"),
  readDoc: (path: string) => call<DocPayload>("doc_read", { path }),
  writeDoc: (path: string, body: string, expectedHash: string, force = false) =>
    call<WriteResult>("doc_write", { path, body, expectedHash, force }),
  createDoc: (folder: string, title: string) =>
    call<TreeNode>("doc_create", { folder, title }),
  renameDoc: (path: string, newTitle: string) =>
    call<WriteResult>("doc_rename", { path, newTitle }),
  deleteNode: (path: string) => call<string>("doc_delete", { path }),
  moveDoc: (path: string, destFolder: string) => call<string>("doc_move", { path, destFolder }),
  moveFolder: (path: string, destParent: string) => call<string>("folder_move", { path, destParent }),
  reorderDocs: (folder: string, orderedPaths: string[]) =>
    call<string[]>("doc_reorder", { folder, orderedPaths }),
  restoreFromTrash: (trashName: string, origRel: string) =>
    call<null>("trash_restore", { trashName, origRel }),
  trashList: () => call<TrashEntry[]>("trash_list"),
  trashDelete: (trashName: string) => call<null>("trash_delete", { trashName }),
  trashEmpty: () => call<number>("trash_empty"),
  backupNow: (includeAttachments: boolean, copyTo?: string) =>
    call<BackupInfo>("backup_now", { includeAttachments, copyTo }),
  backupList: () => call<BackupInfo[]>("backup_list"),
  diagnosticsExport: (dest: string) => call<null>("diagnostics_export", { dest }),
  logWrite: (level: string, requestId: string, message: string) =>
    call<null>("log_write", { level, requestId, message }),
  logsDir: () => call<string>("logs_dir"),
  createFolder: (parent: string, name: string) =>
    call<TreeNode>("folder_create", { parent, name }),
  renameFolder: (path: string, newName: string) =>
    call<string>("folder_rename", { path, newName }),
  deleteFolder: (path: string) => call<string>("folder_delete", { path }),
  snapshotCreate: (path: string, source: string, note: string) =>
    call<SnapshotInfo>("snapshot_create", { path, source, note }),
  snapshotList: (path: string) => call<SnapshotInfo[]>("snapshot_list", { path }),
  snapshotRead: (path: string, ts: number) => call<string>("snapshot_read", { path, ts }),
  snapshotRestore: (path: string, ts: number) =>
    call<WriteResult>("snapshot_restore", { path, ts }),
  exportWrite: (path: string, data: number[]) => call<null>("export_write", { path, data }),
  /** PDF 导出（FR-EXP-05）：渲染舱分页 + WebView2 PrintToPdf，返回成品路径 */
  pdfExport: (title: string, body: string, dest: string) =>
    call<string>("pdf_export", { title, body, dest }),
  importRead: (path: string) => call<ImportedFile>("import_read", { path }),
  importDoc: (folder: string, filename: string, content: string) =>
    call<TreeNode>("import_doc", { folder, filename, content }),
  // 同步（5.3 契约 sync.* 域）
  syncConfigGet: () => call<SyncConfig>("sync_config_get"),
  syncConfigSet: (cfg: {
    enabled: boolean;
    url: string;
    username: string;
    password?: string;
    remoteRoot: string;
    autoSync: boolean;
  }) => call<SyncConfig>("sync_config_set", cfg),
  syncTest: (url: string, username: string, password?: string, remoteRoot?: string) =>
    call<DavTestResult>("sync_test", { url, username, password, remoteRoot }),
  syncNow: (direction?: "upload" | "download") =>
    call<{ started: boolean; reason: string }>("sync_now", { direction }),
  syncStatus: () => call<SyncStatus>("sync_status"),
  syncConflictResolve: (copyPath: string, keepRemote: boolean) =>
    call<null>("sync_conflict_resolve", { copyPath, keepRemote }),
  syncHistory: (limit?: number) => call<SyncLogRow[]>("sync_history", { limit }),
  syncDocStates: () => call<{ path: string; state: string }[]>("sync_doc_states"),
  // AI（5.3 契约 ai.* 域）
  aiProviderList: () => call<AiProvider[]>("ai_provider_list"),
  aiProviderSave: (provider: AiProvider, apiKey?: string) =>
    call<AiProvider>("ai_provider_save", { provider, apiKey }),
  aiProviderDelete: (id: string) => call<null>("ai_provider_delete", { id }),
  aiProviderSetDefault: (id: string | null) =>
    call<null>("ai_provider_set_default", { id }),
  aiConnectivityTest: (providerId: string, baseUrl: string, apiKey: string) =>
    call<DavTestResult>("ai_connectivity_test", { providerId, baseUrl, apiKey }),
  aiModelsFetch: (providerId: string) => call<string[]>("ai_models_fetch", { providerId }),
  aiChatStart: (kind: "polish" | "continue" | "chat", providerId: string, messages: ChatMsg[]) =>
    call<{ jobId: string; chars: number }>("ai_chat_start", { kind, providerId, messages }),
  aiJobCancel: (jobId: string) => call<boolean>("ai_job_cancel", { jobId }),
  aiConversationEnsure: (docUuid: string, title: string) =>
    call<AiConversation>("ai_conversation_ensure", { docUuid, title }),
  aiMessageList: (convId: string) => call<AiMessage[]>("ai_message_list", { convId }),
  aiMessageAppend: (convId: string, role: string, content: string) =>
    call<null>("ai_message_append", { convId, role, content }),
  aiMessageDeleteLast: (convId: string, role: string) =>
    call<null>("ai_message_delete_last", { convId, role }),
  aiRequestLog: (limit?: number) => call<AiRequestLog[]>("ai_request_log", { limit }),
  aiRequestLogClear: () => call<null>("ai_request_log_clear"),
  docSetPrivacy: (path: string, privacy: boolean) =>
    call<WriteResult>("doc_set_privacy", { path, privacy }),
  // 网络出口（5.x 契约 net.* 域）
  netConfigGet: () => call<NetConfig>("net_config_get"),
  netConfigSet: (mode: NetConfig["mode"], url: string) =>
    call<NetConfig>("net_config_set", { mode, url }),
};
