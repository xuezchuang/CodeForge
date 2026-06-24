use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use base64::{engine::general_purpose, Engine as _};
use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;
use serde_json::{json, Value};
use zip::ZipArchive;

use crate::path_utils::normalize_display_path;

const MAX_ENTRY_BYTES: u64 = 16 * 1024 * 1024;
const DEFAULT_MAX_ITEMS: usize = 1200;
const MAX_ITEMS_LIMIT: usize = 5000;
const MAX_TEXT_CHARS: usize = 200_000;
const DEFAULT_MODEL_IMAGE_MAX_BYTES: u64 = 4 * 1024 * 1024;

const REL_IMAGE: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/image";
const REL_NOTES_SLIDE: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/notesSlide";
const REL_SLIDE: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide";

#[derive(Default)]
struct LimitState {
    max_items: usize,
    emitted_items: usize,
    text_chars: usize,
    truncated: bool,
}

impl LimitState {
    fn new(max_items: usize) -> Self {
        Self {
            max_items,
            ..Self::default()
        }
    }

    fn can_emit(&mut self, text: &str) -> bool {
        if self.emitted_items >= self.max_items {
            self.truncated = true;
            return false;
        }
        if self.text_chars.saturating_add(text.len()) > MAX_TEXT_CHARS {
            self.truncated = true;
            return false;
        }
        self.emitted_items += 1;
        self.text_chars += text.len();
        true
    }
}

#[derive(Clone, Debug, Default)]
struct Paragraph {
    text: String,
    style_id: Option<String>,
}

#[derive(Clone, Debug, Default)]
struct Table {
    rows: Vec<Vec<String>>,
}

#[derive(Default)]
struct WordDocumentContent {
    paragraphs: Vec<Paragraph>,
    tables: Vec<Table>,
    truncated: bool,
}

#[derive(Clone, Debug, Default)]
struct Relationship {
    id: String,
    rel_type: String,
    target: String,
}

pub fn read_docx(workspace_root: &str, arguments: &Value) -> Result<Value, String> {
    let (workspace, file) = resolve_office_file(workspace_root, arguments, ".docx")?;
    let max_items = optional_usize(arguments, "max_items", DEFAULT_MAX_ITEMS)?;
    let max_items = max_items.min(MAX_ITEMS_LIMIT);

    let mut archive = open_zip(&file)?;
    if !archive_contains(&mut archive, "word/document.xml") {
        return Err("invalid_docx: missing word/document.xml".to_string());
    }

    let styles = read_zip_text_optional(&mut archive, "word/styles.xml")?
        .map(|xml| parse_word_styles(&xml))
        .unwrap_or_default();
    let document_xml = read_zip_text(&mut archive, "word/document.xml")?;
    let mut limits = LimitState::new(max_items);
    let document = parse_word_document(&document_xml, &mut limits)?;
    let mut warnings = Vec::<String>::new();
    if document.truncated || limits.truncated {
        warnings.push("truncated: document text exceeded extraction limits".to_string());
    }

    let comments = read_zip_text_optional(&mut archive, "word/comments.xml")?
        .map(|xml| parse_word_comments(&xml, max_items))
        .transpose()?
        .unwrap_or_default();
    let footnotes = read_zip_text_optional(&mut archive, "word/footnotes.xml")?
        .map(|xml| parse_note_part(&xml, "footnote", max_items))
        .transpose()?
        .unwrap_or_default();
    let endnotes = read_zip_text_optional(&mut archive, "word/endnotes.xml")?
        .map(|xml| parse_note_part(&xml, "endnote", max_items))
        .transpose()?
        .unwrap_or_default();
    let headers = parse_word_named_parts(&mut archive, "word/header", max_items)?;
    let footers = parse_word_named_parts(&mut archive, "word/footer", max_items)?;
    let relationships = read_zip_text_optional(&mut archive, "word/_rels/document.xml.rels")?
        .map(|xml| parse_relationships(&xml))
        .transpose()?
        .unwrap_or_default();
    let images = relationships
        .iter()
        .filter(|relationship| relationship.rel_type == REL_IMAGE)
        .map(|relationship| {
            json!({
                "relationshipId": relationship.id,
                "target": relationship.target,
            })
        })
        .collect::<Vec<_>>();

    let headings = document
        .paragraphs
        .iter()
        .enumerate()
        .filter_map(|(index, paragraph)| {
            let style = paragraph.style_id.as_deref()?;
            let heading = classify_heading(style, &styles)?;
            Some(json!({
                "index": index + 1,
                "level": heading,
                "text": paragraph.text,
                "styleId": style,
                "styleName": styles.get(style).cloned().unwrap_or_else(|| style.to_string()),
            }))
        })
        .collect::<Vec<_>>();

    let paragraphs = document
        .paragraphs
        .iter()
        .enumerate()
        .map(|(index, paragraph)| {
            json!({
                "index": index + 1,
                "text": paragraph.text,
                "styleId": paragraph.style_id,
                "styleName": paragraph
                    .style_id
                    .as_ref()
                    .and_then(|style| styles.get(style))
                    .cloned(),
            })
        })
        .collect::<Vec<_>>();

    let tables = document
        .tables
        .iter()
        .enumerate()
        .map(|(index, table)| {
            json!({
                "index": index + 1,
                "rows": table.rows,
                "rowCount": table.rows.len(),
                "columnCount": table.rows.iter().map(Vec::len).max().unwrap_or(0),
            })
        })
        .collect::<Vec<_>>();

    let text = document
        .paragraphs
        .iter()
        .map(|paragraph| paragraph.text.as_str())
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n");

    Ok(json!({
        "file": relative_path(&workspace, &file),
        "type": "docx",
        "summary": {
            "paragraphCount": document.paragraphs.len(),
            "headingCount": headings.len(),
            "tableCount": document.tables.len(),
            "commentCount": comments.len(),
            "headerCount": headers.len(),
            "footerCount": footers.len(),
            "footnoteCount": footnotes.len(),
            "endnoteCount": endnotes.len(),
            "imageCount": images.len(),
            "truncated": document.truncated || limits.truncated,
        },
        "text": text,
        "headings": headings,
        "paragraphs": paragraphs,
        "tables": tables,
        "comments": comments,
        "headers": headers,
        "footers": footers,
        "footnotes": footnotes,
        "endnotes": endnotes,
        "images": images,
        "warnings": warnings,
    }))
}

pub fn read_pptx(workspace_root: &str, arguments: &Value) -> Result<Value, String> {
    let (workspace, file) = resolve_office_file(workspace_root, arguments, ".pptx")?;
    let max_items = optional_usize(arguments, "max_items", DEFAULT_MAX_ITEMS)?;
    let max_items = max_items.min(MAX_ITEMS_LIMIT);

    let mut archive = open_zip(&file)?;
    if !archive_contains(&mut archive, "ppt/presentation.xml") {
        return Err("invalid_pptx: missing ppt/presentation.xml".to_string());
    }

    let presentation_xml = read_zip_text(&mut archive, "ppt/presentation.xml")?;
    let presentation_rels =
        read_zip_text_optional(&mut archive, "ppt/_rels/presentation.xml.rels")?
            .map(|xml| parse_relationships(&xml))
            .transpose()?
            .unwrap_or_default();
    let slide_order = parse_presentation_slide_order(&presentation_xml, &presentation_rels)?;
    let slide_parts = if slide_order.is_empty() {
        sorted_zip_names(&mut archive, "ppt/slides/slide", ".xml")
    } else {
        slide_order
    };

    let mut slides = Vec::new();
    let mut warnings = Vec::<String>::new();
    let mut total_text_count = 0usize;
    let mut total_note_count = 0usize;
    let mut total_image_count = 0usize;
    let mut truncated = false;

    for (index, slide_part) in slide_parts.iter().enumerate() {
        if index >= max_items {
            truncated = true;
            break;
        }
        let slide_xml = match read_zip_text_optional(&mut archive, slide_part)? {
            Some(xml) => xml,
            None => {
                warnings.push(format!("missing_slide_part: {slide_part}"));
                continue;
            }
        };
        let slide_texts = parse_text_paragraphs(&slide_xml, max_items)?;
        let slide_rels_part = rels_part_name(slide_part);
        let slide_rels = read_zip_text_optional(&mut archive, &slide_rels_part)?
            .map(|xml| parse_relationships(&xml))
            .transpose()?
            .unwrap_or_default();

        let notes_part = slide_rels
            .iter()
            .find(|relationship| relationship.rel_type == REL_NOTES_SLIDE)
            .map(|relationship| resolve_package_target(slide_part, &relationship.target));
        let notes = match notes_part.as_deref() {
            Some(part) => read_zip_text_optional(&mut archive, part)?
                .map(|xml| parse_text_paragraphs(&xml, max_items))
                .transpose()?
                .unwrap_or_default(),
            None => Vec::new(),
        };
        let mut images = Vec::new();
        for relationship in slide_rels
            .iter()
            .filter(|relationship| relationship.rel_type == REL_IMAGE)
        {
            let target = resolve_package_target(slide_part, &relationship.target);
            let (mime_type, byte_size) = zip_entry_image_metadata(&mut archive, &target)?;
            images.push(json!({
                "relationshipId": relationship.id,
                "target": target,
                "mimeType": mime_type,
                "byteSize": byte_size,
            }));
        }

        total_text_count += slide_texts.len();
        total_note_count += notes.len();
        total_image_count += images.len();

        let title = slide_texts
            .iter()
            .find(|text| !text.trim().is_empty())
            .cloned();

        slides.push(json!({
            "index": index + 1,
            "part": slide_part,
            "title": title,
            "texts": slide_texts
                .iter()
                .enumerate()
                .map(|(text_index, text)| json!({
                    "index": text_index + 1,
                    "text": text,
                }))
                .collect::<Vec<_>>(),
            "notes": notes
                .iter()
                .enumerate()
                .map(|(note_index, note)| json!({
                    "index": note_index + 1,
                    "text": note,
                }))
                .collect::<Vec<_>>(),
            "images": images,
        }));
    }

    if truncated {
        warnings.push("truncated: slide count exceeded max_items".to_string());
    }

    Ok(json!({
        "file": relative_path(&workspace, &file),
        "type": "pptx",
        "summary": {
            "slideCount": slides.len(),
            "textItemCount": total_text_count,
            "noteItemCount": total_note_count,
            "imageCount": total_image_count,
            "truncated": truncated,
        },
        "slides": slides,
        "warnings": warnings,
    }))
}

pub fn pptx_model_image_attachments(
    workspace_root: &str,
    presentation_output: &Value,
) -> Result<Value, String> {
    let workspace = canonical_workspace_root(workspace_root)?;
    let raw_path = presentation_output
        .get("file")
        .and_then(Value::as_str)
        .ok_or_else(|| "invalid_pptx_output: missing file".to_string())?;
    let file = resolve_existing_path(&workspace, raw_path)?;
    let mut archive = open_zip(&file)?;
    let mut attachments = Vec::new();
    let mut skipped = Vec::new();

    for slide in presentation_output
        .get("slides")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let slide_index = slide.get("index").and_then(Value::as_u64).unwrap_or(0);
        for image in slide
            .get("images")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(target) = image.get("target").and_then(Value::as_str) else {
                continue;
            };
            let Some(mime_type) = supported_image_mime_type(target) else {
                skipped.push(format!(
                    "unsupported_image_type: slide {slide_index}: {target}"
                ));
                continue;
            };
            let bytes = read_zip_binary_optional(&mut archive, target)?;
            let Some(bytes) = bytes else {
                skipped.push(format!("missing_image: slide {slide_index}: {target}"));
                continue;
            };
            if bytes.len() as u64 > DEFAULT_MODEL_IMAGE_MAX_BYTES {
                skipped.push(format!(
                    "image_too_large: slide {slide_index}: {target}: {} bytes",
                    bytes.len()
                ));
                continue;
            }
            let encoded = general_purpose::STANDARD.encode(bytes);
            let file_name = target
                .rsplit_once('/')
                .map(|(_, name)| name)
                .unwrap_or(target);
            attachments.push(json!({
                "kind": "image",
                "name": format!("slide-{slide_index}-{file_name}"),
                "mimeType": mime_type,
                "dataUrl": format!("data:{mime_type};base64,{encoded}"),
                "slideIndex": slide_index,
                "target": target,
            }));
        }
    }

    Ok(json!({
        "attachments": attachments,
        "skipped": skipped,
        "maxImageBytes": DEFAULT_MODEL_IMAGE_MAX_BYTES,
    }))
}

fn parse_word_document(xml: &str, limits: &mut LimitState) -> Result<WordDocumentContent, String> {
    let mut reader = xml_reader(xml);
    let mut buf = Vec::new();
    let mut content = WordDocumentContent::default();
    let mut in_text = false;
    let mut in_paragraph = false;
    let mut paragraph = Paragraph::default();
    let mut table_depth = 0usize;
    let mut current_table: Option<Table> = None;
    let mut current_row: Option<Vec<String>> = None;
    let mut current_cell: Option<String> = None;

    loop {
        match reader
            .read_event_into(&mut buf)
            .map_err(|error| format!("xml_parse_failed: {error}"))?
        {
            Event::Start(event) => {
                let name = local_name(event.name().as_ref());
                match name.as_str() {
                    "tbl" => {
                        table_depth += 1;
                        if table_depth == 1 {
                            current_table = Some(Table::default());
                        }
                    }
                    "tr" if table_depth == 1 => current_row = Some(Vec::new()),
                    "tc" if table_depth == 1 => current_cell = Some(String::new()),
                    "p" => {
                        in_paragraph = true;
                        paragraph = Paragraph::default();
                    }
                    "pStyle" if in_paragraph => {
                        paragraph.style_id = attr_value(&reader, &event, "val")?;
                    }
                    "t" => in_text = true,
                    _ => {}
                }
            }
            Event::Empty(event) => {
                let name = local_name(event.name().as_ref());
                match name.as_str() {
                    "pStyle" if in_paragraph => {
                        paragraph.style_id = attr_value(&reader, &event, "val")?;
                    }
                    "tab" if in_paragraph => {
                        append_word_text(&mut paragraph, &mut current_cell, "\t")
                    }
                    "br" | "cr" if in_paragraph => {
                        append_word_text(&mut paragraph, &mut current_cell, "\n")
                    }
                    _ => {}
                }
            }
            Event::Text(event) if in_text => {
                let text = event
                    .xml10_content()
                    .map_err(|error| format!("xml_text_decode_failed: {error}"))?;
                append_word_text(&mut paragraph, &mut current_cell, &text);
            }
            Event::CData(event) if in_text => {
                let text = event
                    .decode()
                    .map_err(|error| format!("xml_cdata_decode_failed: {error}"))?;
                append_word_text(&mut paragraph, &mut current_cell, &text);
            }
            Event::End(event) => {
                let name = local_name(event.name().as_ref());
                match name.as_str() {
                    "t" => in_text = false,
                    "p" if in_paragraph => {
                        in_paragraph = false;
                        let text = normalize_text(&paragraph.text);
                        paragraph.text = text;
                        if table_depth == 0 && !paragraph.text.is_empty() {
                            if limits.can_emit(&paragraph.text) {
                                content.paragraphs.push(paragraph.clone());
                            } else {
                                content.truncated = true;
                            }
                        }
                    }
                    "tc" if table_depth == 1 => {
                        if let (Some(row), Some(cell)) = (current_row.as_mut(), current_cell.take())
                        {
                            row.push(normalize_text(&cell));
                        }
                    }
                    "tr" if table_depth == 1 => {
                        if let (Some(table), Some(row)) =
                            (current_table.as_mut(), current_row.take())
                        {
                            table.rows.push(row);
                        }
                    }
                    "tbl" if table_depth > 0 => {
                        if table_depth == 1 {
                            if let Some(table) = current_table.take() {
                                let table_text = table
                                    .rows
                                    .iter()
                                    .flat_map(|row| row.iter())
                                    .map(String::as_str)
                                    .collect::<Vec<_>>()
                                    .join("\n");
                                if limits.can_emit(&table_text) {
                                    content.tables.push(table);
                                } else {
                                    content.truncated = true;
                                }
                            }
                        }
                        table_depth -= 1;
                    }
                    _ => {}
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }

    Ok(content)
}

fn parse_word_styles(xml: &str) -> HashMap<String, String> {
    let mut reader = xml_reader(xml);
    let mut buf = Vec::new();
    let mut styles = HashMap::new();
    let mut current_style_id: Option<String> = None;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(event)) => {
                let name = local_name(event.name().as_ref());
                if name == "style" {
                    current_style_id = attr_value(&reader, &event, "styleId").ok().flatten();
                } else if name == "name" {
                    if let (Some(style_id), Ok(Some(style_name))) = (
                        current_style_id.as_ref(),
                        attr_value(&reader, &event, "val"),
                    ) {
                        styles.insert(style_id.clone(), style_name);
                    }
                }
            }
            Ok(Event::Empty(event)) => {
                let name = local_name(event.name().as_ref());
                if name == "name" {
                    if let (Some(style_id), Ok(Some(style_name))) = (
                        current_style_id.as_ref(),
                        attr_value(&reader, &event, "val"),
                    ) {
                        styles.insert(style_id.clone(), style_name);
                    }
                }
            }
            Ok(Event::End(event)) if local_name(event.name().as_ref()) == "style" => {
                current_style_id = None;
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    styles
}

fn parse_word_comments(xml: &str, max_items: usize) -> Result<Vec<Value>, String> {
    let parts = parse_text_parts_by_element(xml, "comment", max_items)?;
    Ok(parts
        .into_iter()
        .map(|part| {
            json!({
                "id": part.attributes.get("id"),
                "author": part.attributes.get("author"),
                "date": part.attributes.get("date"),
                "text": part.text,
            })
        })
        .collect())
}

fn parse_note_part(xml: &str, element_name: &str, max_items: usize) -> Result<Vec<Value>, String> {
    let parts = parse_text_parts_by_element(xml, element_name, max_items)?;
    Ok(parts
        .into_iter()
        .filter(|part| {
            !matches!(
                part.attributes.get("type").map(String::as_str),
                Some("separator" | "continuationSeparator")
            )
        })
        .map(|part| {
            json!({
                "id": part.attributes.get("id"),
                "text": part.text,
            })
        })
        .collect())
}

fn parse_word_named_parts(
    archive: &mut ZipArchive<File>,
    prefix: &str,
    max_items: usize,
) -> Result<Vec<Value>, String> {
    let names = sorted_zip_names(archive, prefix, ".xml");
    let mut parts = Vec::new();
    for name in names {
        let xml = read_zip_text(archive, &name)?;
        let texts = parse_text_paragraphs(&xml, max_items)?;
        parts.push(json!({
            "part": name,
            "texts": texts
                .iter()
                .enumerate()
                .map(|(index, text)| json!({
                    "index": index + 1,
                    "text": text,
                }))
                .collect::<Vec<_>>(),
        }));
    }
    Ok(parts)
}

#[derive(Default)]
struct TextPart {
    attributes: HashMap<String, String>,
    text: String,
}

fn parse_text_parts_by_element(
    xml: &str,
    element_name: &str,
    max_items: usize,
) -> Result<Vec<TextPart>, String> {
    let mut reader = xml_reader(xml);
    let mut buf = Vec::new();
    let mut parts = Vec::<TextPart>::new();
    let mut active_depth = 0usize;
    let mut active = TextPart::default();
    let mut in_text = false;

    loop {
        match reader
            .read_event_into(&mut buf)
            .map_err(|error| format!("xml_parse_failed: {error}"))?
        {
            Event::Start(event) => {
                let name = local_name(event.name().as_ref());
                if name == element_name && active_depth == 0 {
                    active_depth = 1;
                    active = TextPart {
                        attributes: collect_attrs(&reader, &event)?,
                        text: String::new(),
                    };
                } else if active_depth > 0 {
                    active_depth += 1;
                    if name == "t" {
                        in_text = true;
                    }
                }
            }
            Event::Empty(event) if active_depth > 0 => {
                let name = local_name(event.name().as_ref());
                if matches!(name.as_str(), "tab") {
                    active.text.push('\t');
                } else if matches!(name.as_str(), "br" | "cr") {
                    active.text.push('\n');
                }
            }
            Event::Text(event) if active_depth > 0 && in_text => {
                let text = event
                    .xml10_content()
                    .map_err(|error| format!("xml_text_decode_failed: {error}"))?;
                active.text.push_str(&text);
            }
            Event::End(event) if active_depth > 0 => {
                let name = local_name(event.name().as_ref());
                if name == "t" {
                    in_text = false;
                }
                active_depth -= 1;
                if active_depth == 0 {
                    active.text = normalize_text(&active.text);
                    if !active.text.is_empty() {
                        parts.push(active);
                    }
                    active = TextPart::default();
                    if parts.len() >= max_items {
                        break;
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }

    Ok(parts)
}

fn parse_text_paragraphs(xml: &str, max_items: usize) -> Result<Vec<String>, String> {
    let mut reader = xml_reader(xml);
    let mut buf = Vec::new();
    let mut paragraphs = Vec::<String>::new();
    let mut in_paragraph = false;
    let mut in_text = false;
    let mut current = String::new();

    loop {
        match reader
            .read_event_into(&mut buf)
            .map_err(|error| format!("xml_parse_failed: {error}"))?
        {
            Event::Start(event) => {
                let name = local_name(event.name().as_ref());
                if name == "p" {
                    in_paragraph = true;
                    current.clear();
                } else if in_paragraph && name == "t" {
                    in_text = true;
                }
            }
            Event::Empty(event) if in_paragraph => {
                let name = local_name(event.name().as_ref());
                if matches!(name.as_str(), "tab") {
                    current.push('\t');
                } else if matches!(name.as_str(), "br" | "cr") {
                    current.push('\n');
                }
            }
            Event::Text(event) if in_paragraph && in_text => {
                let text = event
                    .xml10_content()
                    .map_err(|error| format!("xml_text_decode_failed: {error}"))?;
                current.push_str(&text);
            }
            Event::CData(event) if in_paragraph && in_text => {
                let text = event
                    .decode()
                    .map_err(|error| format!("xml_cdata_decode_failed: {error}"))?;
                current.push_str(&text);
            }
            Event::End(event) => {
                let name = local_name(event.name().as_ref());
                if name == "t" {
                    in_text = false;
                } else if name == "p" && in_paragraph {
                    in_paragraph = false;
                    let text = normalize_text(&current);
                    if !text.is_empty() {
                        paragraphs.push(text);
                    }
                    if paragraphs.len() >= max_items {
                        break;
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }

    Ok(paragraphs)
}

fn parse_presentation_slide_order(
    presentation_xml: &str,
    relationships: &[Relationship],
) -> Result<Vec<String>, String> {
    let rels_by_id = relationships
        .iter()
        .filter(|relationship| relationship.rel_type == REL_SLIDE)
        .map(|relationship| (relationship.id.as_str(), relationship.target.as_str()))
        .collect::<HashMap<_, _>>();
    let mut reader = xml_reader(presentation_xml);
    let mut buf = Vec::new();
    let mut slides = Vec::new();

    loop {
        match reader
            .read_event_into(&mut buf)
            .map_err(|error| format!("xml_parse_failed: {error}"))?
        {
            Event::Start(event) | Event::Empty(event) => {
                if local_name(event.name().as_ref()) == "sldId" {
                    if let Some(rel_id) = attr_value(&reader, &event, "id")? {
                        if let Some(target) = rels_by_id.get(rel_id.as_str()) {
                            slides.push(resolve_package_target("ppt/presentation.xml", target));
                        }
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }

    Ok(slides)
}

fn parse_relationships(xml: &str) -> Result<Vec<Relationship>, String> {
    let mut reader = xml_reader(xml);
    let mut buf = Vec::new();
    let mut relationships = Vec::new();

    loop {
        match reader
            .read_event_into(&mut buf)
            .map_err(|error| format!("xml_parse_failed: {error}"))?
        {
            Event::Start(event) | Event::Empty(event) => {
                if local_name(event.name().as_ref()) == "Relationship" {
                    relationships.push(Relationship {
                        id: attr_value(&reader, &event, "Id")?.unwrap_or_default(),
                        rel_type: attr_value(&reader, &event, "Type")?.unwrap_or_default(),
                        target: attr_value(&reader, &event, "Target")?.unwrap_or_default(),
                    });
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }

    Ok(relationships)
}

fn resolve_office_file(
    workspace_root: &str,
    arguments: &Value,
    expected_extension: &str,
) -> Result<(PathBuf, PathBuf), String> {
    let workspace = canonical_workspace_root(workspace_root)?;
    let raw_path = required_string(arguments, "path")?;
    let file = resolve_existing_path(&workspace, &raw_path)?;
    if !file.is_file() {
        return Err(format!(
            "not_file: {}",
            relative_or_display(&workspace, &file)
        ));
    }
    let extension = file
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| format!(".{}", extension.to_ascii_lowercase()))
        .unwrap_or_default();
    if extension != expected_extension {
        return Err(format!(
            "unsupported_file_type: expected {expected_extension}, got {}",
            extension
        ));
    }
    Ok((workspace, file))
}

fn open_zip(file: &Path) -> Result<ZipArchive<File>, String> {
    let input =
        File::open(file).map_err(|error| format!("file_not_found: {}: {error}", file.display()))?;
    ZipArchive::new(input).map_err(|error| format!("invalid_zip: {}: {error}", file.display()))
}

fn read_zip_text(archive: &mut ZipArchive<File>, name: &str) -> Result<String, String> {
    read_zip_text_optional(archive, name)?.ok_or_else(|| format!("zip_entry_missing: {name}"))
}

fn read_zip_text_optional(
    archive: &mut ZipArchive<File>,
    name: &str,
) -> Result<Option<String>, String> {
    let mut file = match archive.by_name(name) {
        Ok(file) => file,
        Err(zip::result::ZipError::FileNotFound) => return Ok(None),
        Err(error) => return Err(format!("zip_entry_open_failed: {name}: {error}")),
    };
    if file.size() > MAX_ENTRY_BYTES {
        return Err(format!(
            "zip_entry_too_large: {name}: {} bytes",
            file.size()
        ));
    }
    let mut text = String::new();
    file.read_to_string(&mut text)
        .map_err(|error| format!("zip_entry_read_failed: {name}: {error}"))?;
    Ok(Some(text))
}

fn read_zip_binary_optional(
    archive: &mut ZipArchive<File>,
    name: &str,
) -> Result<Option<Vec<u8>>, String> {
    let mut file = match archive.by_name(name) {
        Ok(file) => file,
        Err(zip::result::ZipError::FileNotFound) => return Ok(None),
        Err(error) => return Err(format!("zip_entry_open_failed: {name}: {error}")),
    };
    if file.size() > MAX_ENTRY_BYTES {
        return Err(format!(
            "zip_entry_too_large: {name}: {} bytes",
            file.size()
        ));
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|error| format!("zip_entry_read_failed: {name}: {error}"))?;
    Ok(Some(bytes))
}

fn zip_entry_image_metadata(
    archive: &mut ZipArchive<File>,
    target: &str,
) -> Result<(Option<&'static str>, Option<u64>), String> {
    let mime_type = supported_image_mime_type(target);
    let byte_size = match archive.by_name(target) {
        Ok(file) => Some(file.size()),
        Err(zip::result::ZipError::FileNotFound) => None,
        Err(error) => return Err(format!("zip_entry_open_failed: {target}: {error}")),
    };
    Ok((mime_type, byte_size))
}

fn archive_contains(archive: &mut ZipArchive<File>, name: &str) -> bool {
    archive.by_name(name).is_ok()
}

fn sorted_zip_names(archive: &mut ZipArchive<File>, prefix: &str, suffix: &str) -> Vec<String> {
    let mut names = Vec::new();
    for index in 0..archive.len() {
        if let Ok(file) = archive.by_index(index) {
            let name = file.name().to_string();
            if name.starts_with(prefix) && name.ends_with(suffix) {
                names.push(name);
            }
        }
    }
    names.sort_by_key(|name| natural_name_key(name));
    names
}

fn rels_part_name(part_name: &str) -> String {
    let Some((dir, file)) = part_name.rsplit_once('/') else {
        return format!("_rels/{part_name}.rels");
    };
    format!("{dir}/_rels/{file}.rels")
}

fn resolve_package_target(base_part: &str, target: &str) -> String {
    if target.starts_with('/') {
        return target.trim_start_matches('/').to_string();
    }
    let mut parts = base_part
        .rsplit_once('/')
        .map(|(dir, _)| dir.split('/').collect::<Vec<_>>())
        .unwrap_or_default();
    for segment in target.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    parts.join("/")
}

fn classify_heading(style_id: &str, styles: &HashMap<String, String>) -> Option<u8> {
    let candidates = [
        style_id.to_ascii_lowercase(),
        styles
            .get(style_id)
            .map(|style| style.to_ascii_lowercase())
            .unwrap_or_default(),
    ];
    for candidate in candidates {
        let compact = candidate.replace(' ', "");
        if let Some(rest) = compact.strip_prefix("heading") {
            if let Ok(level) = rest.parse::<u8>() {
                return Some(level);
            }
        }
    }
    None
}

fn supported_image_mime_type(path: &str) -> Option<&'static str> {
    match path
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase())
        .as_deref()
    {
        Some("png") => Some("image/png"),
        Some("jpg") | Some("jpeg") => Some("image/jpeg"),
        Some("gif") => Some("image/gif"),
        Some("webp") => Some("image/webp"),
        _ => None,
    }
}

fn xml_reader(xml: &str) -> Reader<&[u8]> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    reader
}

fn append_word_text(paragraph: &mut Paragraph, cell: &mut Option<String>, text: &str) {
    paragraph.text.push_str(text);
    if let Some(cell) = cell.as_mut() {
        cell.push_str(text);
    }
}

fn attr_value(
    reader: &Reader<&[u8]>,
    event: &BytesStart<'_>,
    key: &str,
) -> Result<Option<String>, String> {
    for attr in event.attributes().with_checks(false) {
        let attr = attr.map_err(|error| format!("xml_attr_parse_failed: {error}"))?;
        if local_name(attr.key.as_ref()) == key {
            let value = attr
                .decode_and_unescape_value(reader.decoder())
                .map_err(|error| format!("xml_attr_decode_failed: {error}"))?;
            return Ok(Some(value.into_owned()));
        }
    }
    Ok(None)
}

fn collect_attrs(
    reader: &Reader<&[u8]>,
    event: &BytesStart<'_>,
) -> Result<HashMap<String, String>, String> {
    let mut attrs = HashMap::new();
    for attr in event.attributes().with_checks(false) {
        let attr = attr.map_err(|error| format!("xml_attr_parse_failed: {error}"))?;
        let key = local_name(attr.key.as_ref());
        let value = attr
            .decode_and_unescape_value(reader.decoder())
            .map_err(|error| format!("xml_attr_decode_failed: {error}"))?;
        attrs.insert(key, value.into_owned());
    }
    Ok(attrs)
}

fn local_name(name: &[u8]) -> String {
    let name = std::str::from_utf8(name).unwrap_or_default();
    name.rsplit_once(':')
        .map(|(_, local)| local)
        .unwrap_or(name)
        .to_string()
}

fn normalize_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn natural_name_key(name: &str) -> (String, u32) {
    let lower = name.to_ascii_lowercase();
    let (dir, file_name) = lower
        .rsplit_once('/')
        .map(|(dir, file)| (format!("{dir}/"), file))
        .unwrap_or_else(|| (String::new(), lower.as_str()));
    let stem = file_name
        .rsplit_once('.')
        .map(|(stem, _)| stem)
        .unwrap_or(file_name);
    let digit_start = stem
        .char_indices()
        .rev()
        .find(|(_, ch)| !ch.is_ascii_digit())
        .map(|(index, ch)| index + ch.len_utf8())
        .unwrap_or(0);
    let (prefix, suffix) = stem.split_at(digit_start);
    let digits = suffix
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect::<String>();
    let number = digits.parse::<u32>().unwrap_or(0);
    (format!("{dir}{prefix}"), number)
}

fn canonical_workspace_root(workspace_root: &str) -> Result<PathBuf, String> {
    let workspace = PathBuf::from(workspace_root);
    workspace.canonicalize().map_err(|error| {
        format!(
            "workspace_not_found: {}: {error}",
            normalize_display_path(workspace_root)
        )
    })
}

fn resolve_existing_path(workspace: &Path, raw_path: &str) -> Result<PathBuf, String> {
    let input = PathBuf::from(raw_path);
    if input.is_absolute() {
        return Err(format!(
            "path_outside_workspace: {}",
            normalize_display_path(raw_path)
        ));
    }
    let candidate = workspace.join(&input);
    let canonical = candidate.canonicalize().map_err(|error| {
        format!(
            "file_not_found: {}: {error}",
            normalize_display_path(raw_path)
        )
    })?;
    if canonical.starts_with(workspace) {
        Ok(canonical)
    } else {
        Err(format!(
            "path_outside_workspace: {}",
            normalize_display_path(raw_path)
        ))
    }
}

fn relative_path(workspace: &Path, path: &Path) -> String {
    path.strip_prefix(workspace)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn relative_or_display(workspace: &Path, path: &Path) -> String {
    if path.starts_with(workspace) {
        relative_path(workspace, path)
    } else {
        normalize_display_path(&path.display().to_string())
    }
}

fn required_string(arguments: &Value, key: &str) -> Result<String, String> {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("invalid_arguments: missing {key}"))
}

fn optional_usize(arguments: &Value, key: &str, default: usize) -> Result<usize, String> {
    match arguments.get(key) {
        Some(value) => {
            let parsed = value
                .as_u64()
                .and_then(|value| usize::try_from(value).ok())
                .ok_or_else(|| format!("invalid_arguments: {key} must be a positive integer"))?;
            if parsed == 0 {
                return Err(format!(
                    "invalid_arguments: {key} must be a positive integer"
                ));
            }
            Ok(parsed)
        }
        None => Ok(default),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use zip::write::SimpleFileOptions;

    #[test]
    fn read_docx_extracts_paragraphs_tables_comments_and_headers() {
        let temp = TempDir::new().unwrap();
        let file = temp.path().join("sample.docx");
        write_zip(
            &file,
            &[
                (
                    "word/document.xml",
                    r#"<w:document xmlns:w="w"><w:body><w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Title</w:t></w:r></w:p><w:p><w:r><w:t>Hello</w:t></w:r><w:r><w:t> world</w:t></w:r></w:p><w:tbl><w:tr><w:tc><w:p><w:r><w:t>A1</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>B1</w:t></w:r></w:p></w:tc></w:tr></w:tbl></w:body></w:document>"#,
                ),
                (
                    "word/styles.xml",
                    r#"<w:styles xmlns:w="w"><w:style w:styleId="Heading1"><w:name w:val="heading 1"/></w:style></w:styles>"#,
                ),
                (
                    "word/comments.xml",
                    r#"<w:comments xmlns:w="w"><w:comment w:id="1" w:author="A"><w:p><w:r><w:t>Check this</w:t></w:r></w:p></w:comment></w:comments>"#,
                ),
                (
                    "word/header1.xml",
                    r#"<w:hdr xmlns:w="w"><w:p><w:r><w:t>Header text</w:t></w:r></w:p></w:hdr>"#,
                ),
            ],
        );

        let output = read_docx(
            temp.path().to_str().unwrap(),
            &json!({ "path": "sample.docx" }),
        )
        .unwrap();

        assert_eq!(output["summary"]["paragraphCount"], json!(2));
        assert_eq!(output["summary"]["tableCount"], json!(1));
        assert_eq!(output["headings"][0]["text"], json!("Title"));
        assert_eq!(output["tables"][0]["rows"][0][1], json!("B1"));
        assert_eq!(output["comments"][0]["text"], json!("Check this"));
        assert_eq!(
            output["headers"][0]["texts"][0]["text"],
            json!("Header text")
        );
    }

    #[test]
    fn read_pptx_extracts_slides_notes_and_images() {
        let temp = TempDir::new().unwrap();
        let file = temp.path().join("deck.pptx");
        write_zip(
            &file,
            &[
                (
                    "ppt/presentation.xml",
                    r#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId r:id="rId2"/></p:sldIdLst></p:presentation>"#,
                ),
                (
                    "ppt/_rels/presentation.xml.rels",
                    r#"<Relationships><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/></Relationships>"#,
                ),
                (
                    "ppt/slides/slide1.xml",
                    r#"<p:sld xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree><p:sp><p:txBody><a:p><a:r><a:t>Slide title</a:t></a:r></a:p><a:p><a:r><a:t>Body text</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:sld>"#,
                ),
                (
                    "ppt/slides/_rels/slide1.xml.rels",
                    r#"<Relationships><Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/notesSlide" Target="../notesSlides/notesSlide1.xml"/><Relationship Id="rId4" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="../media/image1.png"/></Relationships>"#,
                ),
                (
                    "ppt/notesSlides/notesSlide1.xml",
                    r#"<p:notes xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree><p:sp><p:txBody><a:p><a:r><a:t>Speaker note</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:notes>"#,
                ),
                ("ppt/media/image1.png", "fake-png"),
            ],
        );

        let output = read_pptx(
            temp.path().to_str().unwrap(),
            &json!({ "path": "deck.pptx" }),
        )
        .unwrap();

        assert_eq!(output["summary"]["slideCount"], json!(1));
        assert_eq!(output["slides"][0]["title"], json!("Slide title"));
        assert_eq!(output["slides"][0]["texts"][1]["text"], json!("Body text"));
        assert_eq!(
            output["slides"][0]["notes"][0]["text"],
            json!("Speaker note")
        );
        assert_eq!(
            output["slides"][0]["images"][0]["target"],
            json!("ppt/media/image1.png")
        );
        assert_eq!(
            output["slides"][0]["images"][0]["mimeType"],
            json!("image/png")
        );

        let attachments =
            pptx_model_image_attachments(temp.path().to_str().unwrap(), &output).unwrap();
        assert_eq!(
            attachments["attachments"][0]["mimeType"],
            json!("image/png")
        );
        assert!(attachments["attachments"][0]["dataUrl"]
            .as_str()
            .unwrap()
            .starts_with("data:image/png;base64,"));
    }

    fn write_zip(path: &Path, entries: &[(&str, &str)]) {
        let file = File::create(path).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        let options = SimpleFileOptions::default();
        for (name, text) in entries {
            writer.start_file(name, options).unwrap();
            std::io::Write::write_all(&mut writer, text.as_bytes()).unwrap();
        }
        writer.finish().unwrap();
    }
}
