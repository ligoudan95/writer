//! 库体检与修复（FR-DOC-07，V1.1）：风险 R8 各场景的统一处理入口。
//! 检出：frontmatter id 缺失/重复、非 UTF-8 编码文件、搜索索引与 md 不一致、
//! 僵死库锁、孤儿附件、损坏的墓碑清单。修复前自动备份，全部动作写日志。
//! AC（A14）：人为制造"两文件同 uuid + 缺 id 文件"后体检，两项均被检出且
//! 一键修复（重分配 uuid），修复后同步正常。

use std::collections::BTreeMap;
use std::path::Path;

use crate::frontmatter;
use crate::library::{atomic_write, AppError};

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct HealthIssue {
    /// missing_id | dup_id | bad_encoding | index_stale | stale_lock | orphan_attachment | tombstone_corrupt
    pub kind: String,
    pub path: String,
    pub detail: String,
    pub fixable: bool,
}

fn now_log() -> String {
    "[health]".to_string()
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn walk_files(dir: &Path, prefix: &str, out: &mut Vec<String>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let rel = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
        let p = e.path();
        if p.is_dir() {
            walk_files(&p, &rel, out);
        } else {
            out.push(rel);
        }
    }
}

/// 体检主入口：只读扫描，不做任何修复
pub fn check(root: &Path) -> Vec<HealthIssue> {
    let mut issues = Vec::new();
    let mut mds = Vec::new();
    walk_files(&root.join("docs"), "docs", &mut mds);

    // 1) id 缺失 / 重复 / 编码异常（单遍扫描）
    let mut uuid_map: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for rel in &mds {
        if !rel.to_lowercase().ends_with(".md") || rel.ends_with(".writer-sync.json") || rel.ends_with(".writer-stats.json") {
            continue;
        }
        let bytes = match std::fs::read(root.join(rel)) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let content = match String::from_utf8(bytes) {
            Ok(c) => c,
            Err(_) => {
                issues.push(HealthIssue {
                    kind: "bad_encoding".into(),
                    path: rel.clone(),
                    detail: "文件不是 UTF-8 编码（可能为 GBK 等）：编辑器按替换符显示，请在编辑器中以当前内容另存修复".into(),
                    fixable: false,
                });
                continue;
            }
        };
        let (fm, _) = frontmatter::split(&content);
        let id_opt = fm.as_ref().and_then(|f| f.get("id")).map(|v| v.trim().to_string());
        match id_opt.as_deref() {
            None | Some("") => {
                issues.push(HealthIssue {
                    kind: "missing_id".into(),
                    path: rel.clone(),
                    detail: "缺少 frontmatter id：不参与同步身份体系（外部引入文件）".into(),
                    fixable: true,
                });
            }
            Some(id) => {
                uuid_map.entry(id.to_string()).or_default().push(rel.clone());
            }
        }
    }
    for (uuid, paths) in &uuid_map {
        if paths.len() > 1 {
            for p in paths {
                issues.push(HealthIssue {
                    kind: "dup_id".into(),
                    path: p.clone(),
                    detail: format!("uuid {uuid} 同时出现在 {} 个文件（复制/同步异常产物），需重分配", paths.len()),
                    fixable: true,
                });
            }
        }
    }

    // 2) 搜索索引一致性：索引行数与文件数不一致（查询路径自愈，这里给显式入口）
    if let Ok(conn) = crate::search::open(root) {
        let indexed: i64 = conn
            .query_row("SELECT COUNT(*) FROM fts_stat", [], |r| r.get(0))
            .unwrap_or(-1);
        let md_count = mds.iter().filter(|p| p.ends_with(".md")).count() as i64;
        if indexed >= 0 && indexed != md_count {
            issues.push(HealthIssue {
                kind: "index_stale".into(),
                path: "（全文搜索索引）".into(),
                detail: format!("索引 {} 条 vs 磁盘 {} 篇（查询时也会自愈增量，可全量重建）", indexed, md_count),
                fixable: true,
            });
        }
    }

    // 3) 僵死锁文件：.writer/.lock 存在但无活动持有（启动已接管过的会提示，此处兜底可见）
    let lock = root.join(".writer").join(".lock");
    if lock.exists() {
        let size = std::fs::metadata(&lock).map(|m| m.len()).unwrap_or(0);
        // 锁文件 16 字节为正常句柄标记；异常残留以体检可见性为目的（单实例场景下
        // 应用自身持有锁——体检在应用内运行即非僵死；仅提示状态）
        if size == 0 {
            issues.push(HealthIssue {
                kind: "stale_lock".into(),
                path: ".writer/.lock".into(),
                detail: "库锁文件为空（上次异常退出的残留，启动时已自动接管）".into(),
                fixable: false,
            });
        }
    }

    // 4) 孤儿附件：attachments/ 下未被任何 md 引用的文件（按文件名引用扫描）
    let att_dir = root.join("attachments");
    if att_dir.is_dir() {
        let mut all_text = String::new();
        for rel in &mds {
            if rel.ends_with(".md") {
                if let Ok(c) = std::fs::read_to_string(root.join(rel)) {
                    all_text.push_str(&c);
                }
            }
        }
        let mut atts = Vec::new();
        walk_files(&att_dir, "attachments", &mut atts);
        for a in atts {
            let name = a.rsplit('/').next().unwrap_or(&a).to_string();
            if !all_text.contains(&name) {
                issues.push(HealthIssue {
                    kind: "orphan_attachment".into(),
                    path: a.clone(),
                    detail: "附件未被任何文档引用（可能是删除文档后的遗留），可移入回收站".into(),
                    fixable: true,
                });
            }
        }
    }

    // 5) 墓碑清单损坏
    let manifest = root.join(crate::sync::MANIFEST_SYNC);
    if manifest.exists() {
        if let Ok(b) = std::fs::read(&manifest) {
            if b.len() > 2 && serde_json::from_slice::<serde_json::Value>(&b).is_err() {
                issues.push(HealthIssue {
                    kind: "tombstone_corrupt".into(),
                    path: crate::sync::MANIFEST_SYNC.into(),
                    detail: "墓碑清单不是合法 JSON（高风险：旧删除可能复活）——备份后重置为空清单".into(),
                    fixable: true,
                });
            }
        }
    }
    issues
}

#[derive(serde::Serialize)]
pub struct FixReport {
    pub fixed: Vec<String>,
    pub skipped: Vec<String>,
    pub backup: String,
}

/// 一键修复：修复前自动备份（FR-DOC-07）；逐项动作写应用日志
pub fn fix_all(root: &Path, issues: &[HealthIssue]) -> Result<FixReport, AppError> {
    // 修复前备份（失败则中止修复——备份是红线）
    let backup = crate::backup::backup_now(root, false, None)
        .map_err(|e| AppError::new("health", format!("修复前备份失败，已中止：{}", e.message)))?
        .name;
    crate::applog::info("", &format!("{} 体检修复开始（备份 {backup}，{} 项）", now_log(), issues.len()));

    let mut report = FixReport { fixed: Vec::new(), skipped: Vec::new(), backup };
    // dup_id：同组保留首个（路径字典序），其余重分配
    let mut seen_uuid: BTreeMap<String, String> = BTreeMap::new();
    let mut sorted: Vec<&HealthIssue> = issues.iter().collect();
    sorted.sort_by(|a, b| a.path.cmp(&b.path));
    for issue in sorted {
        let r = match issue.kind.as_str() {
            "missing_id" | "dup_id" => {
                if issue.kind == "dup_id" {
                    // 从 detail 提取 uuid（同组去重保留首个）
                    let uuid = issue.detail.split(' ').nth(1).unwrap_or("").to_string();
                    if let Some(first) = seen_uuid.get(&uuid) {
                        if first == &issue.path {
                            report.fixed.push(format!("{}：保留原 uuid", issue.path));
                            continue; // 组内首个保留
                        }
                    } else {
                        seen_uuid.insert(uuid, issue.path.clone());
                        report.fixed.push(format!("{}：保留原 uuid", issue.path));
                        continue;
                    }
                }
                reassign_uuid(root, &issue.path)
                    .map(|_| format!("{}：已重分配 uuid", issue.path))
            }
            "index_stale" => crate::search::open(root)
                .and_then(|c| crate::search::rebuild(&c, root))
                .map(|n| format!("索引已全量重建（{n} 篇）")),
            "orphan_attachment" => {
                let src = root.join(&issue.path);
                let name = issue.path.rsplit('/').next().unwrap_or(&issue.path);
                let trash_dir = root.join(".writer").join("trash");
                std::fs::create_dir_all(&trash_dir)
                    .map_err(|e| AppError::new("health", e.to_string()))?;
                let dest = trash_dir.join(format!("{}-{name}", now_secs()));
                std::fs::rename(&src, &dest)
                    .map(|_| format!("{}：已移入回收站", issue.path))
                    .map_err(|e| AppError::new("health", e.to_string()))
            }
            "tombstone_corrupt" => {
                atomic_write(&root.join(crate::sync::MANIFEST_SYNC), b"{\"tombstones\":[]}")
                    .map(|_| "墓碑清单已重置为空".to_string())
                    .map_err(|e| AppError::new("health", e.to_string()))
            }
            _ => {
                report.skipped.push(format!("{}：不可自动修复（{}）", issue.path, issue.kind));
                Ok(String::new())
            }
        };
        match r {
            Ok(msg) if !msg.is_empty() => {
                report.fixed.push(msg.clone());
                crate::applog::info("", &format!("{} 修复：{msg}", now_log()));
            }
            Ok(_) => {}
            Err(e) => {
                report.skipped.push(format!("{}：修复失败（{}）", issue.path, e.message));
                crate::applog::error("", &format!("{} 修复失败 {}：{}", now_log(), issue.path, e.message));
            }
        }
    }
    crate::applog::info("", &format!("{} 体检修复完成：{} 项修复 / {} 项跳过", now_log(), report.fixed.len(), report.skipped.len()));
    Ok(report)
}

/// 重分配 uuid：重写 frontmatter id（保持其余键与正文不变）
fn reassign_uuid(root: &Path, rel: &str) -> Result<(), AppError> {
    let p = root.join(rel);
    let bytes = std::fs::read(&p).map_err(|e| AppError::new("health", e.to_string()))?;
    let content = String::from_utf8_lossy(&bytes).to_string();
    let (fm_opt, body) = frontmatter::split(&content);
    let mut fm = fm_opt.unwrap_or_default();
    fm.set("id", &uuid::Uuid::new_v4().to_string());
    let full = format!("{}\n{}", fm.render(), body);
    atomic_write(&p, full.as_bytes()).map_err(|e| AppError::new("health", e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_lib(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("writer-health-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("docs")).unwrap();
        std::fs::create_dir_all(d.join(".writer")).unwrap();
        d
    }

    fn w(root: &Path, rel: &str, front: &str, body: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, format!("{front}\n{body}")).unwrap();
    }

    /// AC（A14）：两文件同 uuid + 缺 id 文件 → 全检出 + 一键修复 + id 重新唯一
    #[test]
    fn detects_and_fixes_id_issues() {
        let root = tmp_lib("ac");
        w(&root, "docs/a.md", "---\nid: same-uuid\ntitle: A\n---", "正文A");
        w(&root, "docs/b.md", "---\nid: same-uuid\ntitle: B\n---", "正文B");
        w(&root, "docs/c.md", "没有 frontmatter 的外部文件", "正文C");

        let issues = check(&root);
        let missing: Vec<_> = issues.iter().filter(|i| i.kind == "missing_id").collect();
        let dups: Vec<_> = issues.iter().filter(|i| i.kind == "dup_id").collect();
        assert_eq!(missing.len(), 1, "缺 id 必须检出");
        assert_eq!(dups.len(), 2, "同 uuid 两文件必须双双检出");

        let report = fix_all(&root, &issues).unwrap();
        assert!(!report.backup.is_empty(), "修复前必须产生备份");

        // 修复后复检：无 id 问题；三个文件 uuid 互不相同
        let after = check(&root);
        assert!(after.iter().all(|i| i.kind != "missing_id" && i.kind != "dup_id"), "修复后仍有问题：{after:?}");
        let mut ids = Vec::new();
        for f in ["a.md", "b.md", "c.md"] {
            let c = std::fs::read_to_string(root.join("docs").join(f)).unwrap();
            let (fm, _) = frontmatter::split(&c);
            ids.push(fm.unwrap().get("id").unwrap().to_string());
        }
        assert_eq!(ids.len(), 3);
        assert_ne!(ids[0], ids[1]);
        assert_ne!(ids[0], ids[2]);
        assert_ne!(ids[1], ids[2]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn detects_bad_encoding_and_orphan_attachment() {
        let root = tmp_lib("enc");
        // GBK 字节（"中文" GBK = D6 D0 CE C4）→ 非 UTF-8
        std::fs::write(root.join("docs").join("gbk.md"), [0x2d, 0x2d, 0x2d, 0x0a, 0xd6, 0xd0, 0xce, 0xc4]).unwrap();
        w(&root, "docs/ok.md", "---\nid: u1\n---", "引用图 image-1.png");
        std::fs::create_dir_all(root.join("attachments")).unwrap();
        std::fs::write(root.join("attachments").join("image-1.png"), b"png").unwrap();
        std::fs::write(root.join("attachments").join("orphan.png"), b"png").unwrap();

        let issues = check(&root);
        assert!(issues.iter().any(|i| i.kind == "bad_encoding" && i.path == "docs/gbk.md"));
        // 非 UTF-8 文件只报编码问题（修复编码后才进入 id 体系检查）
        assert!(!issues.iter().any(|i| i.path == "docs/gbk.md" && i.kind != "bad_encoding"));
        let orphan = issues.iter().find(|i| i.kind == "orphan_attachment").unwrap();
        assert_eq!(orphan.path, "attachments/orphan.png");

        // 修复：孤儿附件入"回收站"目录；编码问题跳过（不可自动修复）
        let report = fix_all(&root, &issues).unwrap();
        assert!(!root.join("attachments/orphan.png").exists(), "孤儿附件应被移走");
        assert!(root.join("attachments/image-1.png").exists(), "被引用附件不得误删");
        assert!(report.skipped.iter().any(|s| s.contains("gbk.md")), "编码问题应跳过并提示");
        let _ = std::fs::remove_dir_all(&root);
    }
}
