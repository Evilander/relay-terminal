use serde::Serialize;
use std::{
    collections::{HashMap, HashSet},
    fs::File,
    io::{Read, Write},
    path::Path,
};

const LIMIT: u64 = 8 * 1024 * 1024;
const MAX_INVENTORY_BYTES: usize = 16 * 1024 * 1024;
const MAX_ENTRIES: usize = 50_000;
const MAX_SECTIONS: usize = MAX_ENTRIES + 1;
const MAX_ROWS: usize = MAX_ENTRIES * 3;
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BookmarkEntry {
    pub section: String,
    pub folder: String,
    pub name: String,
    pub raw_value: String,
    pub image: String,
    pub ignored_by_source: bool,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BookmarkInventory {
    pub entries: Vec<BookmarkEntry>,
    pub warnings: Vec<String>,
    pub source: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EntryPreview<'a> {
    section: &'a str,
    folder: &'a str,
    name: &'a str,
    raw_value: &'a str,
    image: &'a str,
    ignored_by_source: bool,
}

#[derive(Serialize)]
struct InventoryEnvelope<'a> {
    entries: &'a [BookmarkEntry],
    warnings: &'a [String],
    source: &'a str,
}

#[derive(Default)]
struct InventoryBudget {
    bytes: usize,
}

impl Write for InventoryBudget {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > MAX_INVENTORY_BYTES.saturating_sub(self.bytes) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Bookmark inventory exceeds 16 MiB",
            ));
        }
        self.bytes += bytes.len();
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl InventoryBudget {
    fn new(source: &str) -> Result<Self, String> {
        let mut budget = Self::default();
        budget.reserve(&InventoryEnvelope {
            entries: &[],
            warnings: &[],
            source,
        })?;
        Ok(budget)
    }

    fn reserve<T: Serialize>(&mut self, value: &T) -> Result<(), String> {
        // Counting serialized bytes includes escaping without cloning shared metadata.
        // An extra comma per value conservatively covers array separators.
        serde_json::to_writer(&mut *self, value)
            .map_err(|_| "Bookmark inventory exceeds 16 MiB".to_owned())?;
        self.write_all(b",")
            .map_err(|_| "Bookmark inventory exceeds 16 MiB".to_owned())
    }

    fn warning(&mut self, warnings: &mut Vec<String>, warning: String) -> Result<(), String> {
        self.reserve(&warning)?;
        warnings.push(warning);
        Ok(())
    }
}

pub fn inspect_bookmarks(path: &Path) -> Result<BookmarkInventory, String> {
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|e| format!("Cannot read bookmark file: {e}"))?
        .take(LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > LIMIT {
        return Err("Bookmark file exceeds 8 MiB".into());
    }
    parse_bookmarks(&bytes, path.to_string_lossy().into_owned())
}

pub fn parse_bookmarks(bytes: &[u8], source: String) -> Result<BookmarkInventory, String> {
    if bytes.len() as u64 > LIMIT {
        return Err("Bookmark file exceeds 8 MiB".into());
    }
    let mut budget = InventoryBudget::new(&source)?;
    let mut warnings = vec![];
    let content = if bytes.starts_with(&[0xff, 0xfe]) {
        let (words, trailing) = bytes[2..].as_chunks::<2>();
        if !trailing.is_empty() {
            return Err("Truncated UTF-16LE bookmark file".into());
        }
        let words: Vec<u16> = words.iter().copied().map(u16::from_le_bytes).collect();
        String::from_utf16(&words).map_err(|_| "Invalid UTF-16LE bookmark file")?
    } else {
        let data = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
        match std::str::from_utf8(data) {
            Ok(text) => text.to_owned(),
            Err(_) => {
                budget.warning(&mut warnings, "Legacy encoding is unknown; invalid UTF-8 bytes are shown as replacement characters. Original file is unchanged.".into())?;
                String::from_utf8_lossy(data).into_owned()
            }
        }
    };
    let mut sections: Vec<(String, Vec<(String, String)>)> = vec![];
    let mut row_count = 0;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            if sections.len() >= MAX_SECTIONS {
                return Err("Bookmark parsed section count exceeds 50,001".into());
            }
            sections.push((trimmed[1..trimmed.len() - 1].to_owned(), vec![]));
        } else if !trimmed.is_empty() && !trimmed.starts_with(';') && !trimmed.starts_with('#') {
            if let Some((key, value)) = line.split_once('=') {
                if let Some((_, rows)) = sections.last_mut() {
                    if row_count >= MAX_ROWS {
                        return Err("Bookmark parsed row count exceeds 150,000".into());
                    }
                    row_count += 1;
                    rows.push((key.trim().to_owned(), value.to_owned()));
                }
            }
        }
    }
    let present: HashSet<usize> = sections
        .iter()
        .filter_map(|(name, _)| {
            let n = name.strip_prefix("Bookmarks_")?.parse::<usize>().ok()?;
            (name == &format!("Bookmarks_{n}")).then_some(n)
        })
        .collect();
    let has_base = sections.iter().any(|(name, _)| name == "Bookmarks");
    let mut first_gap = if has_base { 1 } else { 0 };
    while first_gap > 0 && present.contains(&first_gap) {
        first_gap += 1;
    }
    if !has_base && !present.is_empty() {
        budget.warning(&mut warnings, "[Bookmarks] is missing; numbered sections are retained but marked ignored by the source.".into())?;
    }
    if first_gap > 0 && present.iter().any(|n| *n > first_gap) {
        budget.warning(&mut warnings, format!("Bookmarks_{first_gap} is missing; subsequent numbered sections are retained but marked ignored by the source."))?;
    }
    let mut entries = vec![];
    let mut repeats = HashMap::new();
    for (section, rows) in sections {
        let numbered = section
            .strip_prefix("Bookmarks_")
            .and_then(|v| v.parse::<usize>().ok());
        if section != "Bookmarks" && numbered.is_none() {
            continue;
        }
        *repeats.entry(section.clone()).or_insert(0usize) += 1;
        let folders: Vec<_> = rows.iter().filter(|(k, _)| k == "SubRep").collect();
        let images: Vec<_> = rows.iter().filter(|(k, _)| k == "ImgNum").collect();
        if folders.len() > 1 || images.len() > 1 {
            budget.warning(&mut warnings, format!(
                "Duplicate metadata in [{section}]; the last occurrence is shown as folder/image, and all raw metadata rows are retained. Source duplicate resolution is unverified."
            ))?;
        }
        let folder = folders.last().map(|(_, v)| v.clone()).unwrap_or_default();
        let image = images.last().map(|(_, v)| v.clone()).unwrap_or_default();
        let folder_count = folders.len();
        let image_count = images.len();
        for (name, raw_value) in rows {
            if (name == "SubRep" && folder_count == 1) || (name == "ImgNum" && image_count == 1) {
                continue;
            }
            if entries.len() >= MAX_ENTRIES {
                return Err("Bookmark entry count exceeds 50,000".into());
            }
            let ignored_by_source = numbered
                .is_some_and(|n| n == 0 || n >= first_gap || section != format!("Bookmarks_{n}"));
            budget.reserve(&EntryPreview {
                section: &section,
                folder: &folder,
                name: &name,
                raw_value: &raw_value,
                image: &image,
                ignored_by_source,
            })?;
            entries.push(BookmarkEntry {
                section: section.clone(),
                folder: folder.clone(),
                name,
                raw_value,
                image: image.clone(),
                ignored_by_source,
            });
        }
    }
    if repeats.values().any(|count| *count > 1) {
        budget.warning(
            &mut warnings,
            "Repeated bookmark sections were retained in source order.".into(),
        )?;
    }
    budget.warning(&mut warnings, "Bookmark values are opaque inventory. They are never interpreted as commands or connection credentials.".into())?;
    Ok(BookmarkInventory {
        entries,
        warnings,
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn duplicates_unknown_values_and_gap_survive() {
        let input=b"[Bookmarks]\nSubRep=main\nImgNum=42\nx=opaque==#value\nx=second\n[Bookmarks_1]\na=one\n[Bookmarks_3]\nz=retained\n";
        let inv = parse_bookmarks(input, "fixture".into()).unwrap();
        assert_eq!(inv.entries.len(), 4);
        assert_eq!(inv.entries[0].raw_value, "opaque==#value");
        assert!(inv.entries[3].ignored_by_source);
        assert!(!inv.entries[2].ignored_by_source);
    }
    #[test]
    fn utf16_and_lossy_input_are_explicit() {
        let input = "[Bookmarks]\nSubRep=日本\nname=value";
        let mut bytes = vec![0xff, 0xfe];
        for word in input.encode_utf16() {
            bytes.extend(word.to_le_bytes());
        }
        assert_eq!(
            parse_bookmarks(&bytes, "fixture".into()).unwrap().entries[0].folder,
            "日本"
        );
        assert!(parse_bookmarks(b"[Bookmarks]\na=\xff", "fixture".into())
            .unwrap()
            .warnings
            .iter()
            .any(|w| w.contains("Legacy")));
    }
    #[test]
    fn numbering_requires_base_and_exact_decimal_suffix() {
        let inv = parse_bookmarks(
            b"[Bookmarks_1]\na=x\n[Bookmarks_0]\nb=x\n[Bookmarks_01]\nc=x",
            "fixture".into(),
        )
        .unwrap();
        assert!(inv.entries.iter().all(|entry| entry.ignored_by_source));
    }

    #[test]
    fn shared_metadata_cannot_amplify_a_small_input_without_bound() {
        for metadata in ["SubRep", "ImgNum"] {
            let mut input = format!("[Bookmarks]\n{metadata}={}\n", "m".repeat(2 * 1024 * 1024));
            input.push_str(&"name=opaque\n".repeat(5000));
            assert!(input.len() < LIMIT as usize);
            let error = parse_bookmarks(input.as_bytes(), "fixture".into()).unwrap_err();
            assert!(error.contains("inventory exceeds 16 MiB"), "{error}");
        }
    }

    #[test]
    fn parsed_counts_are_bounded_before_inventory_filtering() {
        let input = "[Unrelated]\n".repeat(MAX_SECTIONS + 1);
        let error = parse_bookmarks(input.as_bytes(), "fixture".into()).unwrap_err();
        assert!(error.contains("parsed section count"), "{error}");

        let input = format!("[Unrelated]\n{}", "=\n".repeat(MAX_ROWS + 1));
        let error = parse_bookmarks(input.as_bytes(), "fixture".into()).unwrap_err();
        assert!(error.contains("parsed row count"), "{error}");
    }

    #[test]
    fn serialization_budget_counts_escaped_values_and_the_full_envelope() {
        let source = "fixture\\\"\n";
        let entry = BookmarkEntry {
            section: "Bookmarks".into(),
            folder: "folder\\\"\u{0000}".into(),
            name: "name\t".into(),
            raw_value: "opaque=\"\\雪".into(),
            image: "image\r".into(),
            ignored_by_source: false,
        };
        let preview = EntryPreview {
            section: &entry.section,
            folder: &entry.folder,
            name: &entry.name,
            raw_value: &entry.raw_value,
            image: &entry.image,
            ignored_by_source: entry.ignored_by_source,
        };
        assert_eq!(
            serde_json::to_vec(&preview).unwrap(),
            serde_json::to_vec(&entry).unwrap()
        );
        let mut budget = InventoryBudget::new(source).unwrap();
        let mut warnings = vec![];
        budget.warning(&mut warnings, "warning\n\"".into()).unwrap();
        budget.reserve(&preview).unwrap();
        let inventory = BookmarkInventory {
            entries: vec![entry],
            warnings,
            source: source.into(),
        };
        let actual = serde_json::to_vec(&inventory).unwrap().len();
        assert_eq!(budget.bytes, actual + 3);
    }

    #[test]
    fn json_escaping_counts_toward_the_inventory_limit() {
        let input = format!("[Bookmarks]\nname={}", "\"".repeat(LIMIT as usize - 20));
        assert!(input.len() <= LIMIT as usize);
        let error = parse_bookmarks(input.as_bytes(), "fixture".into()).unwrap_err();
        assert!(error.contains("inventory exceeds 16 MiB"), "{error}");
    }
}
