//! DOCX 导入（FR-IMP-01）：docx（zip + Office Open XML）→ Markdown 方言子集。
//! 支持正文段落、标题（Heading1-6）、加粗/斜体/下划线、有序/无序列表（numbering.xml
//! 解析 numFmt）、表格（含合并单元格——经 HTML 表格方言承载，与 codec 成对）、换行。
//! 不支持项（图片、脚注转文末注释等）尽力保留文字，结构降级不丢内容。

use std::collections::HashMap;
use std::io::{Cursor, Read};

use crate::library::AppError;

/// 从 docx 字节提取 Markdown 文本
pub fn docx_to_md(bytes: &[u8]) -> Result<String, AppError> {
    let cursor = Cursor::new(bytes);
    let mut zip = zip::ZipArchive::new(cursor)
        .map_err(|e| AppError::new("import", format!("不是有效的 docx 文件：{e}")))?;
    let document = read_entry(&mut zip, "word/document.xml")?;
    let numbering = read_entry(&mut zip, "word/numbering.xml").ok();
    let number_fmt = numbering.as_deref().map(parse_numbering).unwrap_or_default();

    let mut out = String::new();
    let mut ctx = ParseCtx::new(&number_fmt);
    parse_document(&document, &mut ctx, &mut out)?;
    Ok(out)
}

fn read_entry(zip: &mut zip::ZipArchive<Cursor<&[u8]>>, name: &str) -> Result<String, AppError> {
    let mut f = zip
        .by_name(name)
        .map_err(|_| AppError::new("import", "docx 缺少正文（word/document.xml）"))?;
    let mut s = String::new();
    f.read_to_string(&mut s)?;
    Ok(s)
}

// -- numbering：numId → bullet?；ordered 计数在解析侧按连续段递增 -------------------

fn parse_numbering(xml: &str) -> HashMap<String, bool> {
    use quick_xml::events::Event;
    let mut map: HashMap<String, bool> = HashMap::new();
    let mut abstract_fmt: HashMap<String, bool> = HashMap::new(); // abstractNumId → bullet
    let mut num_to_abstract: HashMap<String, String> = HashMap::new();
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut in_abstract: Option<String> = None;
    let mut cur_num: Option<String> = None;
    let mut expect_abstract_text = false;
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => match local_name(e.name().as_ref()) {
                "abstractNum" => in_abstract = attr_val(&e, "abstractNumId"),
                "num" => cur_num = attr_val(&e, "numId"),
                // <w:num><w:abstractNumId w:val="N"/>（属性形态为主，文本形态兜底）
                "abstractNumId" => {
                    if let Some(nid) = cur_num.clone() {
                        match attr_val(&e, "val") {
                            Some(v) => {
                                num_to_abstract.insert(nid, v);
                                cur_num = None;
                            }
                            None => expect_abstract_text = true,
                        }
                    }
                }
                "numFmt" => {
                    if in_abstract.is_some() {
                        let bullet = attr_val(&e, "val").map(|v| v == "bullet").unwrap_or(false);
                        if let Some(a) = &in_abstract {
                            abstract_fmt.entry(a.clone()).or_insert(bullet);
                        }
                    }
                }
                _ => {}
            },
            Ok(Event::Text(t)) => {
                if expect_abstract_text {
                    if let Some(nid) = cur_num.take() {
                        num_to_abstract.insert(nid, String::from_utf8_lossy(t.as_ref()).to_string());
                    }
                    expect_abstract_text = false;
                }
            }
            Ok(Event::End(e)) => {
                if local_name(e.name().as_ref()) == "abstractNum" {
                    in_abstract = None;
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    for (num, abs) in num_to_abstract {
        map.insert(num, abstract_fmt.get(&abs).copied().unwrap_or(false));
    }
    map
}

use quick_xml::events::BytesStart;
/// XML 实体解码（quick-xml 的 Text/属性内容是转义原文：&amp; 等，导入前必须还原）
fn unesc(s: &str) -> String {
    quick_xml::escape::unescape(s)
        .map(|c| c.into_owned())
        .unwrap_or_else(|_| s.to_string())
}

fn attr_val(e: &BytesStart, local: &str) -> Option<String> {
    for a in e.attributes().flatten() {
        let name = local_name(a.key.as_ref());
        if name == local {
            return Some(unesc(&String::from_utf8_lossy(a.value.as_ref())));
        }
    }
    None
}

fn local_name(ns_tag: &[u8]) -> &str {
    let s = std::str::from_utf8(ns_tag).unwrap_or("");
    s.split(':').next_back().unwrap_or(s)
}

// -- 文档解析 ----------------------------------------------------------------------

#[derive(Clone)]
struct Run {
    text: String,
    bold: bool,
    italic: bool,
    underline: bool,
}

struct ParseCtx<'a> {
    number_fmt: &'a HashMap<String, bool>,
    /// 列表计数按 (numId, ilvl) 分键（多级列表不串号）；连续性由 last_list 跟踪
    list_counter: HashMap<String, usize>,
    last_list: Option<String>,
}

impl<'a> ParseCtx<'a> {
    fn new(number_fmt: &'a HashMap<String, bool>) -> Self {
        Self { number_fmt, list_counter: HashMap::new(), last_list: None }
    }
}

fn esc_md(s: &str) -> String {
    // 与 codec escText 同集 + 行首结构字符
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        if matches!(c, '\\' | '`' | '*' | '_' | '[' | ']' | '~' | '<' | '>') {
            out.push('\\');
        }
        out.push(c);
    }
    if out.starts_with('#') || out.starts_with('-') || out.starts_with('+') {
        out.insert(0, '\\');
    }
    out
}

fn flush_para(ctx: &mut ParseCtx, runs: &[Run], style: &str, num: &Option<(String, u8)>, out: &mut String) {
    let text: String = runs.iter().map(|r| r.text.as_str()).collect::<Vec<_>>().join("");
    let inline = runs_to_inline(runs);
    if text.trim().is_empty() && runs.is_empty() {
        return;
    }
    // 标题（也打断列表连续性：Word 常整篇复用同一 numId，标题不清计数会跨章节串号）
    if let Some(level) = style.strip_prefix("Heading").and_then(|n| n.parse::<u8>().ok()) {
        if (1..=6).contains(&level) && !text.trim().is_empty() {
            out.push_str(&format!("\n\n{} {}\n", "#".repeat(level as usize), inline));
            ctx.last_list = None;
            ctx.list_counter.clear();
            return;
        }
    }
    // 列表
    if let Some((num_id, ilvl)) = num {
        let bullet = ctx.number_fmt.get(num_id).copied().unwrap_or(false);
        // 嵌套缩进 = 父 marker 内容列宽（bullet 2 / ordered 3，与 codec listToMd 同源规则）
        let indent = " ".repeat(*ilvl as usize * if bullet { 2 } else { 3 });
        if bullet {
            out.push_str(&format!("\n{indent}- {inline}"));
        } else {
            let key = format!("{num_id}:{ilvl}");
            let c = ctx.list_counter.entry(key).or_insert(0);
            *c += 1;
            out.push_str(&format!("\n{indent}{}. {inline}", c));
        }
        ctx.last_list = Some(num_id.clone());
        return;
    }
    // 普通段落：打断列表连续性（计数重置）
    if ctx.last_list.take().is_some() {
        ctx.list_counter.clear();
    }
    if text.trim().is_empty() {
        return;
    }
    out.push_str(&format!("\n\n{inline}"));
}

fn runs_to_inline(runs: &[Run]) -> String {
    let mut s = String::new();
    for r in runs {
        let mut t = esc_md(&r.text);
        if t.is_empty() {
            continue;
        }
        if r.underline {
            t = format!("<u>{t}</u>");
        }
        if r.italic {
            t = format!("*{t}*");
        }
        if r.bold {
            t = format!("**{t}**");
        }
        s.push_str(&t);
    }
    s
}

fn parse_document(xml: &str, ctx: &mut ParseCtx, out: &mut String) -> Result<(), AppError> {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_str(xml);
    reader.config_mut().trim_text(false);

    let mut runs: Vec<Run> = Vec::new();
    let mut cur_run: Option<Run> = None;
    let mut para_style = String::new();
    let mut para_num: Option<(String, u8)> = None;
    let mut in_rPr = false;
    let mut in_pPr = false;
    let mut in_numPr = false;
    let mut ilvl: u8 = 0;
    // 域指令（w:instrText，如 TOC/PAGE）与修订待删文本（w:delText）不进正文
    let mut text_skip_depth: usize = 0;

    // 表格状态：外层表管理缓冲；嵌套表（tbl_depth>0）的行/格不接管缓冲，
    // 其段落自然流入外层当前单元格（内容保留、结构降级）
    let mut table: Option<Vec<Vec<Para>>> = None;
    let mut row: Option<Vec<Para>> = None;
    type CellBuf = Vec<(Vec<Run>, String, Option<(String, u8)>)>;
    let mut cell: Option<CellBuf> = None;
    let mut cell_span: usize = 1;
    let mut cell_vmerge: VMerge = VMerge::None;
    let mut table_has_merge = false;
    let mut tbl_depth: usize = 0;

    let mut buf = Vec::new();
    macro_rules! on_off {
        ($e:expr) => {
            attr_val($e, "val")
                .map(|v| !matches!(v.as_str(), "false" | "0" | "off"))
                .unwrap_or(true)
        };
    }
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => match local_name(e.name().as_ref()) {
                "instrText" | "delText" => text_skip_depth += 1,
                "p" => {
                    runs.clear();
                    para_style.clear();
                    para_num = None;
                    ilvl = 0;
                }
                "pPr" => in_pPr = true,
                "rPr" => {
                    in_rPr = true;
                    if let Some(r) = cur_run.as_mut() {
                        r.bold = false;
                        r.italic = false;
                        r.underline = false;
                    }
                }
                "pStyle" => {
                    if in_pPr {
                        para_style = attr_val(&e, "val").unwrap_or_default();
                    }
                }
                "numPr" => in_numPr = true,
                "ilvl" => {
                    if in_numPr {
                        ilvl = attr_val(&e, "val").and_then(|v| v.parse::<u8>().ok()).unwrap_or(0);
                    }
                }
                "numId" => {
                    if in_numPr {
                        if let Some(v) = attr_val(&e, "val") {
                            para_num = Some((v, ilvl));
                        }
                    }
                }
                "r" => {
                    cur_run = Some(Run { text: String::new(), bold: false, italic: false, underline: false });
                }
                "b" => {
                    if in_rPr {
                        let on = on_off!(&e);
                        if let Some(r) = cur_run.as_mut() {
                            r.bold = on;
                        }
                    }
                }
                "i" => {
                    if in_rPr {
                        let on = on_off!(&e);
                        if let Some(r) = cur_run.as_mut() {
                            r.italic = on;
                        }
                    }
                }
                "u" => {
                    if in_rPr {
                        // w:val="none" 是显式关闭
                        let on = attr_val(&e, "val")
                            .map(|v| v != "none")
                            .unwrap_or(true);
                        if let Some(r) = cur_run.as_mut() {
                            r.underline = on;
                        }
                    }
                }
                "br" | "tab" => {
                    if let Some(r) = cur_run.as_mut() {
                        r.text.push_str(if local_name(e.name().as_ref()) == "br" { "\\\n" } else { "  " });
                    }
                }
                "tbl" => {
                    if table.is_some() {
                        tbl_depth += 1; // 嵌套表：段落并入外层单元格，不接管缓冲
                    } else {
                        table = Some(Vec::new());
                        table_has_merge = false;
                    }
                }
                "tr" => {
                    if tbl_depth == 0 {
                        row = Some(Vec::new());
                    }
                }
                "tc" => {
                    if tbl_depth == 0 {
                        cell = Some(Vec::new());
                        cell_span = 1;
                        cell_vmerge = VMerge::None;
                    }
                }
                "gridSpan" => {
                    if cell.is_some() {
                        cell_span = attr_val(&e, "val").and_then(|v| v.parse::<usize>().ok()).unwrap_or(1);
                        if cell_span > 1 {
                            table_has_merge = true;
                        }
                    }
                }
                "vMerge" => {
                    if cell.is_some() {
                        cell_vmerge = match attr_val(&e, "val").as_deref() {
                            Some("restart") => VMerge::Restart,
                            // continue / 空（自闭合 <w:vMerge/> = 延续占位格）
                            _ => VMerge::Continue,
                        };
                        table_has_merge = true;
                    }
                }
                _ => {}
            },
            Ok(Event::Text(t)) => {
                if text_skip_depth == 0 {
                    // quick-xml Text 是转义原文，必须 unescape（&amp; → &）
                    let txt = t.unescape().map(|c| c.into_owned()).unwrap_or_default();
                    if let Some(r) = cur_run.as_mut() {
                        r.text.push_str(&txt);
                    }
                }
            },
            Ok(Event::End(e)) => match local_name(e.name().as_ref()) {
                "instrText" | "delText" => text_skip_depth = text_skip_depth.saturating_sub(1),
                "pPr" => in_pPr = false,
                "numPr" => in_numPr = false,
                "rPr" => in_rPr = false,
                "r" => {
                    if let Some(r) = cur_run.take() {
                        runs.push(r);
                    }
                }
                "p" => {
                    // 段落完结：进表格单元格缓冲，或直接输出
                    let para = (runs.clone(), para_style.clone(), para_num.clone());
                    if let Some(cellbuf) = cell.as_mut() {
                        cellbuf.push(para);
                    } else {
                        let (runs, style, num) = para;
                        flush_para(ctx, &runs, &style, &num, out);
                    }
                }
                "tc" => {
                    if tbl_depth == 0 {
                        if let (Some(cell), Some(row)) = (cell.take(), row.as_mut()) {
                            row.push(Para { paras: cell, span: cell_span, vmerge: cell_vmerge });
                        }
                    }
                }
                "tr" => {
                    if tbl_depth == 0 {
                        if let (Some(row), Some(table)) = (row.take(), table.as_mut()) {
                            table.push(row);
                        }
                    }
                }
                "tbl" => {
                    if tbl_depth > 0 {
                        tbl_depth -= 1; // 嵌套表结束：外层继续
                    } else if let Some(table) = table.take() {
                        emit_table(table, table_has_merge, out);
                        out.push('\n');
                    }
                }
                _ => {}
            },
            Ok(Event::Empty(e)) => {
                // 自闭合标签（<w:b/>、<w:br/>、<w:ilvl w:val/> 等）：与 Start 同语义的叶子处理
                match local_name(e.name().as_ref()) {
                    "pStyle" => {
                        if in_pPr {
                            para_style = attr_val(&e, "val").unwrap_or_default();
                        }
                    }
                    "ilvl" => {
                        if in_numPr {
                            ilvl = attr_val(&e, "val").and_then(|v| v.parse::<u8>().ok()).unwrap_or(0);
                        }
                    }
                    "numId" => {
                        if in_numPr {
                            if let Some(v) = attr_val(&e, "val") {
                                para_num = Some((v, ilvl));
                            }
                        }
                    }
                    "b" => {
                        if in_rPr {
                            let on = attr_val(&e, "val")
                                .map(|v| !matches!(v.as_str(), "false" | "0" | "off"))
                                .unwrap_or(true);
                            if let Some(r) = cur_run.as_mut() {
                                r.bold = on;
                            }
                        }
                    }
                    "i" => {
                        if in_rPr {
                            let on = attr_val(&e, "val")
                                .map(|v| !matches!(v.as_str(), "false" | "0" | "off"))
                                .unwrap_or(true);
                            if let Some(r) = cur_run.as_mut() {
                                r.italic = on;
                            }
                        }
                    }
                    "u" => {
                        if in_rPr {
                            let on = attr_val(&e, "val").map(|v| v != "none").unwrap_or(true);
                            if let Some(r) = cur_run.as_mut() {
                                r.underline = on;
                            }
                        }
                    }
                    "br" | "tab" => {
                        if let Some(r) = cur_run.as_mut() {
                            r.text.push_str(if local_name(e.name().as_ref()) == "br" { "\\\n" } else { "  " });
                        }
                    }
                    "gridSpan" => {
                        if cell.is_some() {
                            cell_span = attr_val(&e, "val").and_then(|v| v.parse::<usize>().ok()).unwrap_or(1);
                            if cell_span > 1 {
                                table_has_merge = true;
                            }
                        }
                    }
                    "vMerge" => {
                        if cell.is_some() {
                            // 自闭合可带 val：restart=起点；无 val/continue=占位
                            cell_vmerge = match attr_val(&e, "val").as_deref() {
                                Some("restart") => VMerge::Restart,
                                _ => VMerge::Continue,
                            };
                            table_has_merge = true;
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(AppError::new("import", format!("docx 解析失败：{e}"))),
            _ => {}
        }
        buf.clear();
    }
    Ok(())
}

/// 纵向合并语义（OOXML w:vMerge）：restart=合并起点，continue=延续占位格（不产出实体）
#[derive(Clone, Copy, PartialEq)]
enum VMerge {
    None,
    Restart,
    Continue,
}

/// 单元格（一至多段）
struct Para {
    paras: Vec<(Vec<Run>, String, Option<(String, u8)>)>,
    span: usize,
    vmerge: VMerge,
}

fn cell_text(c: &Para) -> String {
    let mut parts = Vec::new();
    for (runs, _style, num) in &c.paras {
        let mut line = runs_to_inline(runs);
        if let Some((_num_id, ilvl)) = num {
            line = format!("{}- {line}", "  ".repeat(*ilvl as usize)); // 单元格内列表降级
        }
        if !line.trim().is_empty() {
            parts.push(line);
        }
    }
    parts.join("<br>")
}

/// 每行各单元格的网格列号（含 continue 占位格；colspan 前进游标）
fn grid_cols(rows: &[Vec<Para>]) -> Vec<Vec<usize>> {
    rows.iter()
        .map(|row| {
            let mut cols = Vec::with_capacity(row.len());
            let mut col = 0usize;
            for c in row {
                cols.push(col);
                col += c.span.max(1);
            }
            cols
        })
        .collect()
}

fn emit_table(rows: Vec<Vec<Para>>, has_merge: bool, out: &mut String) {
    // 计算纵向合并的 rowspan：restart 格向下数同列连续 continue 格
    let cols = grid_cols(&rows);
    let mut rowspans: Vec<Vec<usize>> = rows.iter().map(|r| vec![1; r.len()]).collect();
    for ri in 0..rows.len() {
        for ci in 0..rows[ri].len() {
            if rows[ri][ci].vmerge != VMerge::Restart {
                continue;
            }
            let col = cols[ri][ci];
            let mut n = 1usize;
            loop {
                let below = ri + n;
                if below >= rows.len() {
                    break;
                }
                let Some(pos) = cols[below].iter().position(|&x| x == col) else { break };
                if rows[below][pos].vmerge == VMerge::Continue {
                    n += 1;
                } else {
                    break;
                }
            }
            rowspans[ri][ci] = n;
        }
    }

    if has_merge {
        // 合并单元格 → HTML 表格方言（与 codec parseHtmlTable 成对）
        out.push_str("\n\n<table>\n");
        for (ri, row) in rows.iter().enumerate() {
            out.push_str("  <tr>\n");
            for (ci, c) in row.iter().enumerate() {
                if c.vmerge == VMerge::Continue {
                    continue; // 延续占位格：不产出实体
                }
                let tag = if ri == 0 { "th" } else { "td" };
                let mut attrs = String::new();
                if c.span > 1 {
                    attrs.push_str(&format!(" colspan=\"{}\"", c.span));
                }
                if c.vmerge == VMerge::Restart && rowspans[ri][ci] > 1 {
                    attrs.push_str(&format!(" rowspan=\"{}\"", rowspans[ri][ci]));
                }
                let txt = cell_text(c)
                    .replace('&', "&amp;")
                    .replace('<', "&lt;")
                    .replace('>', "&gt;")
                    .replace('\n', "<br>");
                out.push_str(&format!("    <{tag}{attrs}>{txt}</{tag}>\n"));
            }
            out.push_str("  </tr>\n");
        }
        out.push_str("</table>");
        return;
    }
    // 普通表格 → GFM 管道
    let mut lines: Vec<String> = Vec::new();
    for (ri, row) in rows.iter().enumerate() {
        let cells: Vec<String> = row
            .iter()
            .map(|c| cell_text(c).replace('|', "\\|").replace('\n', "<br>"))
            .collect();
        lines.push(format!("| {} |", cells.join(" | ")));
        if ri == 0 {
            lines.push(format!("|{}|", cells.iter().map(|_| " --- ".to_string()).collect::<Vec<_>>().join("|")));
        }
    }
    out.push_str("\n\n");
    out.push_str(&lines.join("\n"));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// 构造最小 docx（zip 仅含 word/document.xml + numbering.xml——我们是读取方，够用）
    fn make_docx(document: &str, numbering: Option<&str>) -> Vec<u8> {
        let mut buf = Cursor::new(Vec::new());
        {
            let mut w = zip::ZipWriter::new(&mut buf);
            let opts: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default();
            w.start_file("word/document.xml", opts).unwrap();
            w.write_all(document.as_bytes()).unwrap();
            if let Some(n) = numbering {
                w.start_file("word/numbering.xml", opts).unwrap();
                w.write_all(n.as_bytes()).unwrap();
            }
            w.finish().unwrap();
        }
        buf.into_inner()
    }

    const BODY: &str = r#"<?xml version="1.0"?><w:document xmlns:w="ns"><w:body>
<w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>文档标题</w:t></w:r></w:p>
<w:p><w:r><w:rPr><w:b/></w:rPr><w:t>加粗</w:t></w:r><w:r><w:t>与普通 &amp; 转义</w:t></w:r><w:r><w:rPr><w:i/><w:u/></w:rPr><w:t>斜下</w:t></w:r></w:p>
<w:p><w:r><w:instrText>TOC \o "1-3"</w:instrText></w:r><w:r><w:t>正文段。</w:t></w:r></w:p>
<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>第一项</w:t></w:r></w:p>
<w:p><w:pPr><w:pStyle w:val="Heading2"/></w:pPr><w:r><w:t>章节标题</w:t></w:r></w:p>
<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>重新计数项</w:t></w:r></w:p>
<w:tbl><w:tr><w:tc><w:p><w:r><w:t>甲</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>乙</w:t></w:r></w:p></w:tc></w:tr>
<w:tr><w:tc><w:p><w:r><w:t>1</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>2</w:t></w:r></w:p></w:tc></w:tr></w:tbl>
<w:tbl><w:tr><w:tc><w:tcPr><w:gridSpan w:val="2"/></w:tcPr><w:p><w:r><w:t>合并头</w:t></w:r></w:p></w:tc></w:tr></w:tbl>
<w:tbl>
<w:tr><w:tc><w:tcPr><w:vMerge w:val="restart"/></w:tcPr><w:p><w:r><w:t>跨三行</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>右1</w:t></w:r></w:p></w:tc></w:tr>
<w:tr><w:tc><w:tcPr><w:vMerge/></w:tcPr><w:p><w:r><w:t></w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>右2</w:t></w:r></w:p></w:tc></w:tr>
<w:tr><w:tc><w:tcPr><w:vMerge w:val="continue"/></w:tcPr><w:p><w:r><w:t></w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>右3</w:t></w:r></w:p></w:tc></w:tr>
</w:tbl>
</w:body></w:document>"#;

    const NUMBERING: &str = r#"<?xml version="1.0"?><w:numbering xmlns:w="ns">
<w:num numId="1"><w:abstractNumId w:val="10"/></w:num>
<w:abstractNum w:abstractNumId="10"><w:lvl w:ilvl="0"><w:numFmt w:val="decimal"/></w:lvl></w:abstractNum>
</w:numbering>"#;

    #[test]
    fn docx_to_markdown() {
        let bytes = make_docx(BODY, Some(NUMBERING));
        let md = docx_to_md(&bytes).unwrap();
        assert!(md.contains("# 文档标题"), "{md}");
        assert!(md.contains("**加粗**"), "{md}");
        assert!(md.contains("*<u>斜下</u>*"), "{md}");
        // XML 实体解码：&amp; → &
        assert!(md.contains("与普通 & 转义"), "{md}");
        // 域指令不进正文
        assert!(!md.contains("TOC"), "{md}");
        assert!(md.contains("1. 第一项"), "{md}");
        // 标题打断有序列表计数（重新从 1 开始）
        assert!(md.contains("## 章节标题"), "{md}");
        assert!(md.contains("1. 重新计数项"), "{md}");
        assert!(md.contains("| 甲 | 乙 |"), "{md}");
        assert!(md.contains(" --- "), "{md}");
        assert!(md.contains("<table>"), "合并表格走 HTML 方言：{md}");
        assert!(md.contains("合并头"), "{md}");
        // vMerge：restart 计 rowspan=3，continue 占位格不产出
        assert!(md.contains("rowspan=\"3\""), "{md}");
        assert!(md.contains("跨三行"), "{md}");
        assert!(md.contains("右3"), "{md}");
        assert_eq!(md.matches("右1").count(), 1, "continue 格不产生幻影单元格：{md}");
    }

    #[test]
    fn docx_rejects_non_docx() {
        assert!(docx_to_md(b"not a zip").is_err());
    }
}
