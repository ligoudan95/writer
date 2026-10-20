import { invoke } from "@tauri-apps/api/core";

export interface TreeNode {
  id: string;
  name: string;
  kind: "folder" | "doc";
  children?: TreeNode[];
}

export interface DocPayload {
  path: string;
  title: string;
  body: string;
  hash: string;
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

const call = <T>(cmd: string, args?: Record<string, unknown>): Promise<T> =>
  invoke<T>(cmd, args);

/** 类型化 command 封装（5.3 契约的前端门面；NFR-16：前端不持有泛型 fs 能力） */
export const api = {
  startup: () => call<StartupInfo>("library_startup"),
  list: () => call<TreeNode[]>("doc_list"),
  readDoc: (path: string) => call<DocPayload>("doc_read", { path }),
  writeDoc: (path: string, body: string, expectedHash: string, force = false) =>
    call<WriteResult>("doc_write", { path, body, expectedHash, force }),
  createDoc: (folder: string, title: string) =>
    call<TreeNode>("doc_create", { folder, title }),
  renameDoc: (path: string, newTitle: string) =>
    call<WriteResult>("doc_rename", { path, newTitle }),
  deleteNode: (path: string) => call<string>("doc_delete", { path }),
  restoreFromTrash: (trashName: string, origRel: string) =>
    call<null>("trash_restore", { trashName, origRel }),
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
};
