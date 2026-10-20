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

/// 拆分 frontmatter 与正文；无 frontmatter 时返回 (None, 原文)
pub fn split(content: &str) -> (Option<Frontmatter>, String) {
    let trimmed = content.trim_start_matches('\u{feff}');
    let rest = match trimmed.strip_prefix("---") {
        Some(r) => r,
        None => return (None, content.to_string()),
    };
    let rest = rest
        .strip_prefix("\r\n")
        .or_else(|| rest.strip_prefix('\n'))
        .unwrap_or(rest);

    let mut fm = Frontmatter::default();
    let mut body_from: Option<usize> = None; // 行索引（含）
    for (i, line) in rest.lines().enumerate() {
        let l = line.trim_end();
        if l == "---" || l == "..." {
            body_from = Some(i);
            break;
        }
        if let Some((k, v)) = l.split_once(':') {
            fm.set(k.trim(), v.trim());
        }
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
        None => (None, content.to_string()), // 无闭合标记，视为普通正文
    }
}
