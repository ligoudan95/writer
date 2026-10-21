use serde::{Deserialize, Serialize};

/// 极简有序 frontmatter：`---` 包围的 `key: value` 行；重写时保留未知键（FR-IMP-01 兼容规则）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Frontmatter {
    pub entries: Vec<(String, String)>,
}

impl Frontmatter {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    pub fn set(&mut self, key: &str, value: &str) {
        if let Some(slot) = self.entries.iter_mut().find(|(k, _)| k == key) {
            slot.1 = value.to_string();
        } else {
            self.entries.push((key.to_string(), value.to_string()));
        }
    }

    pub fn render(&self) -> String {
        let mut s = String::from("---\n");
        for (k, v) in &self.entries {
            s.push_str(k);
            s.push_str(": ");
            s.push_str(v);
            s.push('\n');
        }
        s.push_str("---\n");
        s
    }
}

/// 拆分 frontmatter 与正文；无 frontmatter 时返回 (None, 去 BOM 原文)
///（BOM 必须剥在 split 内：否则加 fm 后 BOM 落进正文首行，重复保存也去不掉）
pub fn split(content: &str) -> (Option<Frontmatter>, String) {
    let trimmed = content.trim_start_matches('\u{feff}');
    let rest = match trimmed.strip_prefix("---") {
        Some(r) => r,
        None => return (None, trimmed.to_string()),
    };
    let rest = rest
        .strip_prefix("\r\n")
        .or_else(|| rest.strip_prefix('\n'))
        .unwrap_or(rest);

    let mut fm = Frontmatter::default();
    let mut fm_valid = true;
    let mut body_from: Option<usize> = None; // 行索引（含）
    for (i, line) in rest.lines().enumerate() {
        let l = line.trim_end();
        if l == "---" || l == "..." {
            body_from = Some(i);
            break;
        }
        if l.trim().is_empty() {
            continue; // 空行容忍
        }
        // YAML 块列表续行（缩进/- 开头，如 tags:\n  - a）：不入 fm（本实现只存标量），
        // 但不否定 fm 本身——否则外来合法文档丢 uuid、掉出同步体系。
        // 完整 YAML 兼容（保留列表值）列为 M5 事项
        if l.starts_with([' ', '\t']) || l.trim_start().starts_with("- ") {
            continue;
        }
        match l.split_once(':') {
            Some((k, v)) if !k.trim().is_empty() => fm.set(k.trim(), v.trim()),
            // 非 key:value 行（如正文首行是分隔线后的普通文字）：不是 frontmatter，
            // 整块退回正文——否则该行会在下次保存时被静默丢弃（真实数据丢失）
            _ => {
                fm_valid = false;
                break;
            }
        }
    }
    if !fm_valid {
        return (None, trimmed.to_string());
    }
    match body_from {
        Some(idx) => {
            let body = rest.lines().skip(idx + 1).collect::<Vec<_>>().join("\n");
            let body = body
                .strip_prefix('\n')
                .map(|s| s.to_string())
                .unwrap_or(body);
            (Some(fm), body)
        }
        None => (None, trimmed.to_string()), // 无闭合标记，视为普通正文
    }
}
