//! The dialogs folder as the server sees it. Claude itself reads and picks
//! dialogs (it runs inside that folder with read-only tools); the server only
//! scans it to give the tutor a catalogue and to know which video links on
//! cards are real, so that an invented link is never forwarded.

use crate::dialog;
use std::collections::{BTreeSet, HashSet};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// File name without `.md`, e.g. `16_the_job_interview`.
    pub name: String,
    pub title: String,
    pub cards: usize,
}

#[derive(Debug, Default)]
pub struct Library {
    pub entries: Vec<Entry>,
    /// Every video link found on any card.
    pub urls: HashSet<String>,
}

impl Library {
    /// Reads every `*.md` file in `dir`; files that are not dialogs are skipped with a warning.
    pub fn scan(dir: &Path) -> std::io::Result<Self> {
        let mut paths: Vec<_> = std::fs::read_dir(dir)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("md"))
            .collect();
        paths.sort();
        let mut lib = Library::default();
        for path in paths {
            let Some(name) = path.file_stem().and_then(|s| s.to_str()).map(str::to_string) else { continue };
            let parsed = std::fs::read_to_string(&path)
                .map_err(|e| e.to_string())
                .and_then(|text| dialog::parse(&text).map_err(|e| e.to_string()));
            match parsed {
                Ok(d) => {
                    lib.urls.extend(d.cards.iter().map(|c| c.url.clone()));
                    lib.entries.push(Entry { name, title: d.title, cards: d.cards.len() });
                }
                Err(e) => tracing::warn!("skipping {}: {e}", path.display()),
            }
        }
        Ok(lib)
    }

    pub fn has(&self, name: &str) -> bool {
        self.entries.iter().any(|e| e.name == name)
    }

    /// One line per dialog for the tutor's system prompt; finished dialogs are marked `[done]`.
    pub fn catalogue_marking(&self, done: &BTreeSet<String>) -> String {
        self.entries
            .iter()
            .map(|e| {
                let mark = if done.contains(&e.name) { " [done]" } else { "" };
                format!("- {}.md: {} ({} cards){mark}", e.name, e.title, e.cards)
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixtures() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
    }

    #[test]
    fn scans_dialogs_and_collects_card_links() -> std::io::Result<()> {
        let lib = Library::scan(&fixtures())?;
        assert_eq!(lib.entries.len(), 1);
        assert_eq!(lib.entries[0], Entry { name: "16_the_job_interview".into(), title: "The Job Interview".into(), cards: 12 });
        assert_eq!(lib.urls.len(), 12);
        assert!(lib.urls.iter().all(|u| u.starts_with("https://youtu.be/")));
        assert!(lib.has("16_the_job_interview") && !lib.has("17_flatmates_at_war"));
        assert_eq!(lib.catalogue_marking(&BTreeSet::new()), "- 16_the_job_interview.md: The Job Interview (12 cards)");
        Ok(())
    }

    #[test]
    fn finished_dialogs_are_marked_in_the_catalogue() -> std::io::Result<()> {
        let lib = Library::scan(&fixtures())?;
        let done = BTreeSet::from(["16_the_job_interview".to_string()]);
        assert_eq!(lib.catalogue_marking(&done), "- 16_the_job_interview.md: The Job Interview (12 cards) [done]");
        Ok(())
    }

    #[test]
    fn a_missing_folder_is_an_error_and_a_non_dialog_file_is_skipped() -> std::io::Result<()> {
        assert!(Library::scan(Path::new("/definitely/not/here")).is_err());
        let dir = std::env::temp_dir().join(format!("st-lib-{}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join("notes.md"), "just some notes")?;
        let lib = Library::scan(&dir)?;
        std::fs::remove_dir_all(&dir)?;
        assert!(lib.entries.is_empty());
        Ok(())
    }
}
