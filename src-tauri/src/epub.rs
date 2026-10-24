//! EPUB 3 导出（V1.3）：zip 构建标准 EPUB（mimetype 首位 STORED + container +
//! OPF + nav + 章节 XHTML）。章节 HTML 由前端 exportHtml 产出（统一排版管线）。

use crate::library::AppError;

#[derive(serde::Deserialize, Debug)]
pub struct Chapter {
    pub title: String,
    pub html: String,
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

pub fn build(title: &str, chapters: &[Chapter], dest: &str) -> Result<(), AppError> {
    use std::io::Write;
    let file = std::fs::File::create(dest).map_err(|e| AppError::new("epub", e.to_string()))?;
    let mut zip = zip::ZipWriter::new(file);
    // mimetype 必须首个且 STORED（EPUB 规范）
    let stored = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored);
    let deflated = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    zip.start_file("mimetype", stored).map_err(zip_err)?;
    zip.write_all(b"application/epub+zip").map_err(|e| AppError::new("epub", e.to_string()))?;

    zip.start_file("META-INF/container.xml", deflated).map_err(zip_err)?;
    zip.write_all(
        br#"<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>"#,
    )
    .map_err(|e| AppError::new("epub", e.to_string()))?;

    // nav（目录）
    let nav_items: String = chapters
        .iter()
        .enumerate()
        .map(|(i, c)| format!("<li><a href=\"chap-{}.xhtml\">{}</a></li>", i + 1, esc(&c.title)))
        .collect();
    zip.start_file("OEBPS/nav.xhtml", deflated).map_err(zip_err)?;
    zip.write_all(
        format!(
            r#"<?xml version="1.0" encoding="utf-8"?>
<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops">
<head><title>{}</title></head>
<body><nav epub:type="toc" id="toc"><h1>目录</h1><ol>{}</ol></nav></body></html>"#,
            esc(title),
            nav_items
        )
        .as_bytes(),
    )
    .map_err(|e| AppError::new("epub", e.to_string()))?;

    // 章节文件
    for (i, c) in chapters.iter().enumerate() {
        zip.start_file(format!("OEBPS/chap-{}.xhtml", i + 1), deflated).map_err(zip_err)?;
        // 前端 html 为 body 片段；包一层合法 XHTML（样式内联已随片段）
        zip.write_all(
            format!(
                "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<html xmlns=\"http://www.w3.org/1999/xhtml\"><head><title>{}</title></head><body>{}</body></html>",
                esc(&c.title),
                c.html
            )
            .as_bytes(),
        )
        .map_err(|e| AppError::new("epub", e.to_string()))?;
    }

    // content.opf（manifest + spine）
    let manifest: String = chapters
        .iter()
        .enumerate()
        .map(|(i, _)| {
            format!(
                "<item id=\"c{}\" href=\"chap-{}.xhtml\" media-type=\"application/xhtml+xml\"/>",
                i + 1,
                i + 1
            )
        })
        .collect();
    let spine: String = chapters
        .iter()
        .enumerate()
        .map(|(i, _)| format!("<itemref idref=\"c{}\"/>", i + 1))
        .collect();
    zip.start_file("OEBPS/content.opf", deflated).map_err(zip_err)?;
    let uid = uuid::Uuid::new_v4();
    zip.write_all(
        format!(
            r#"<?xml version="1.0" encoding="utf-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="uid">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:identifier id="uid">urn:uuid:{}</dc:identifier>
    <dc:title>{}</dc:title>
    <dc:language>zh-CN</dc:language>
    <meta property="dcterms:modified">2026-10-24T00:00:00Z</meta>
  </metadata>
  <manifest>
    <item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>
    {}
  </manifest>
  <spine>{}</spine>
</package>"#,
            uid,
            esc(title),
            manifest,
            spine
        )
        .as_bytes(),
    )
    .map_err(|e| AppError::new("epub", e.to_string()))?;

    zip.finish().map_err(zip_err)?;
    Ok(())
}

fn zip_err(e: zip::result::ZipError) -> AppError {
    AppError::new("epub", e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epub_structure_roundtrip() {
        let dest = std::env::temp_dir()
            .join(format!("writer-epub-{}.epub", std::process::id()));
        let chapters = vec![
            Chapter { title: "第一章".into(), html: "<h1>第一章</h1><p>内容一。</p>".into() },
            Chapter { title: "第二章".into(), html: "<h1>第二章</h1><p>内容 & 二。</p>".into() },
        ];
        build("测试书", &chapters, dest.to_str().unwrap()).unwrap();
        let f = std::fs::File::open(&dest).unwrap();
        let mut z = zip::ZipArchive::new(f).unwrap();
        // mimetype 首位且未压缩
        let (n0, comp0) = {
            let first = z.by_index(0).unwrap();
            (first.name().to_string(), first.compression())
        };
        assert_eq!(n0, "mimetype");
        assert_eq!(comp0, zip::CompressionMethod::Stored);
        let mut nav = String::new();
        use std::io::Read;
        z.by_name("OEBPS/nav.xhtml").unwrap().read_to_string(&mut nav).unwrap();
        assert!(nav.contains("第一章"));
        drop(nav);
        let names: Vec<String> = (0..z.len())
            .map(|i| z.by_index(i).unwrap().name().to_string())
            .collect();
        assert!(names.contains(&"META-INF/container.xml".to_string()));
        assert!(names.contains(&"OEBPS/content.opf".to_string()));
        assert!(names.contains(&"OEBPS/nav.xhtml".to_string()));
        assert!(names.contains(&"OEBPS/chap-2.xhtml".to_string()));
        let _ = std::fs::remove_file(&dest);
    }
}
