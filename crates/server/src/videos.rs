//! The video library for video mode: the transcripts in
//! `english-transcripts/transcript/<video id>.md`. A video is drilled as a whole:
//! the tutor sends the learner to watch it on YouTube and then asks about its
//! phrases. Claude reads the transcript files itself; the server only decides
//! which videos are suitable, picks a random order for every session (a language
//! model is a poor random number generator) and knows which links are real.

use rand::seq::SliceRandom;
use std::collections::{BTreeSet, HashSet};
use std::path::Path;

/// A video needs at least this many phrases with an English example to be worth a drill.
const MIN_ITEMS: usize = 5;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoEntry {
    pub id: String,
    pub title: String,
    /// Phrases that have an English example sentence.
    pub items: usize,
    pub url: String,
}

#[derive(Debug, Default)]
pub struct VideoLibrary {
    pub videos: Vec<VideoEntry>,
    /// The video page of every video and every timestamped link inside the transcripts.
    pub urls: HashSet<String>,
}

pub fn video_url(id: &str) -> String {
    format!("https://www.youtube.com/watch?v={id}")
}

/// Parses one transcript. Returns the entry and all timestamped links in it, or
/// `None` when the file is not a drillable lesson (not analysed, or a plain word
/// list with no example sentences).
pub fn parse(id: &str, text: &str) -> Option<(VideoEntry, Vec<String>)> {
    if text.contains("НЕ РАЗОБРАНО") {
        return None;
    }
    let title = text.lines().find_map(|l| l.strip_prefix("# ")).map(str::trim).filter(|t| !t.is_empty())?.to_string();
    let items = text
        .lines()
        .filter(|l| l.strip_prefix("- **EN:** ").is_some_and(|rest| rest.chars().any(|c| c.is_ascii_alphabetic())))
        .count();
    if items < MIN_ITEMS {
        return None;
    }
    let marker = format!("](https://youtu.be/{id}?t=");
    let mut links = Vec::new();
    let mut rest = text;
    while let Some(pos) = rest.find(&marker) {
        let tail = &rest[pos + 2..];
        let Some(end) = tail.find(')') else { break };
        links.push(tail[..end].to_string());
        rest = &tail[end..];
    }
    Some((VideoEntry { id: id.to_string(), title, items, url: video_url(id) }, links))
}

impl VideoLibrary {
    /// Reads every `*.md` in `dir`; files that are not drillable are skipped.
    pub fn scan(dir: &Path) -> std::io::Result<Self> {
        let mut paths: Vec<_> = std::fs::read_dir(dir)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("md"))
            .collect();
        paths.sort();
        let mut lib = VideoLibrary::default();
        for path in paths {
            let Some(id) = path.file_stem().and_then(|s| s.to_str()).map(str::to_string) else { continue };
            let text = match std::fs::read_to_string(&path) {
                Ok(t) => t,
                Err(e) => {
                    tracing::warn!("cannot read {}: {e}", path.display());
                    continue;
                }
            };
            if let Some((entry, links)) = parse(&id, &text) {
                lib.urls.insert(entry.url.clone());
                lib.urls.extend(links);
                lib.videos.push(entry);
            }
        }
        Ok(lib)
    }

    /// Up to `n` different videos in random order, leaving out the ids in `seen`.
    pub fn queue<R: rand::Rng + ?Sized>(&self, rng: &mut R, n: usize, seen: &BTreeSet<String>) -> Vec<&VideoEntry> {
        let mut all: Vec<&VideoEntry> = self.videos.iter().filter(|v| !seen.contains(&v.id)).collect();
        all.shuffle(rng);
        all.truncate(n);
        all
    }
}

/// The video id of a page address made by `video_url`.
pub fn video_id(url: &str) -> Option<&str> {
    url.strip_prefix("https://www.youtube.com/watch?v=").filter(|id| !id.is_empty())
}

/// The queue as lines for the tutor's prompt.
pub fn queue_text(queue: &[&VideoEntry]) -> String {
    queue
        .iter()
        .enumerate()
        .map(|(i, v)| format!("{}. {} | {} | {} phrases | {}", i + 1, v.id, v.title, v.items, v.url))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;
    use rand::rngs::StdRng;

    fn fixtures() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/videos")
    }

    #[test]
    fn only_lessons_with_example_sentences_are_kept() -> std::io::Result<()> {
        let lib = VideoLibrary::scan(&fixtures())?;
        let ids: Vec<&str> = lib.videos.iter().map(|v| v.id.as_str()).collect();
        // 28wzLVPT-Wg: not a lesson; 8alwbrTPhSo: a word list; C9phq-McOXQ: no examples.
        assert_eq!(ids, vec!["2oScAyV9p0A"]);
        let v = &lib.videos[0];
        assert_eq!(v.items, 5);
        assert_eq!(v.url, "https://www.youtube.com/watch?v=2oScAyV9p0A");
        assert!(v.title.contains("take"), "title: {}", v.title);
        Ok(())
    }

    #[test]
    fn the_video_page_and_every_timestamp_link_are_known() -> std::io::Result<()> {
        let lib = VideoLibrary::scan(&fixtures())?;
        assert!(lib.urls.contains("https://www.youtube.com/watch?v=2oScAyV9p0A"));
        assert!(lib.urls.contains("https://youtu.be/2oScAyV9p0A?t=18"));
        assert!(lib.urls.iter().all(|u| u.contains("2oScAyV9p0A")), "{:?}", lib.urls);
        assert_eq!(lib.urls.len(), 1 + 5);
        assert!(!lib.urls.contains("https://youtu.be/2oScAyV9p0A?t=999"));
        Ok(())
    }

    #[test]
    fn a_missing_folder_is_an_error() {
        assert!(VideoLibrary::scan(Path::new("/definitely/not/here")).is_err());
    }

    fn many(n: usize) -> VideoLibrary {
        let videos = (0..n)
            .map(|i| VideoEntry { id: format!("id{i}"), title: format!("T{i}"), items: 10, url: video_url(&format!("id{i}")) })
            .collect();
        VideoLibrary { videos, urls: HashSet::new() }
    }

    #[test]
    fn the_queue_is_a_random_order_of_different_videos() {
        let lib = many(50);
        let a = lib.queue(&mut StdRng::seed_from_u64(1), 30, &BTreeSet::new());
        let b = lib.queue(&mut StdRng::seed_from_u64(2), 30, &BTreeSet::new());
        assert_eq!(a.len(), 30);
        let unique: HashSet<&str> = a.iter().map(|v| v.id.as_str()).collect();
        assert_eq!(unique.len(), 30, "no video twice");
        assert_ne!(a.iter().map(|v| &v.id).collect::<Vec<_>>(), b.iter().map(|v| &v.id).collect::<Vec<_>>());
        let natural: Vec<&String> = lib.videos.iter().take(30).map(|v| &v.id).collect();
        assert_ne!(a.iter().map(|v| &v.id).collect::<Vec<_>>(), natural, "not simply the first files in order");
    }

    #[test]
    fn a_small_library_gives_a_short_queue_and_an_empty_one_an_empty_queue() {
        assert_eq!(many(3).queue(&mut StdRng::seed_from_u64(1), 30, &BTreeSet::new()).len(), 3);
        assert!(many(0).queue(&mut StdRng::seed_from_u64(1), 30, &BTreeSet::new()).is_empty());
    }

    #[test]
    fn watched_videos_are_left_out_of_the_queue() {
        let lib = many(5);
        let seen: BTreeSet<String> = ["id0", "id1", "id2"].iter().map(|s| s.to_string()).collect();
        let ids: BTreeSet<&str> = lib.queue(&mut StdRng::seed_from_u64(1), 30, &seen).iter().map(|v| v.id.as_str()).collect();
        assert_eq!(ids, BTreeSet::from(["id3", "id4"]));
    }

    #[test]
    fn the_id_comes_out_of_the_page_address() {
        assert_eq!(video_id(&video_url("2oScAyV9p0A")), Some("2oScAyV9p0A"));
        assert_eq!(video_id("https://youtu.be/2oScAyV9p0A?t=5"), None);
    }

    #[test]
    fn queue_text_has_the_id_the_title_the_size_and_the_url() {
        let lib = many(2);
        let q = lib.queue(&mut StdRng::seed_from_u64(5), 2, &BTreeSet::new());
        let text = queue_text(&q);
        let first = q[0];
        assert!(text.starts_with(&format!("1. {} | {} | 10 phrases | https://www.youtube.com/watch?v={}", first.id, first.title, first.id)));
        assert_eq!(text.lines().count(), 2);
    }
}
