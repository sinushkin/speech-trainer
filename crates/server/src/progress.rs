//! What the learner has already done: dialogs finished and videos watched.
//! Kept in a small JSON file so the next session starts with something new.
//! When everything has been done the list starts over.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Data {
    #[serde(default)]
    dialogs: BTreeSet<String>,
    #[serde(default)]
    videos: BTreeSet<String>,
}

pub struct Progress {
    /// `None` keeps everything in memory (tests).
    path: Option<PathBuf>,
    data: Mutex<Data>,
}

/// `$SPEECH_TRAINER_PROGRESS`, else `$XDG_DATA_HOME` or `~/.local/share` + `/speech-trainer/progress.json`.
pub fn default_path(env: impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    if let Some(p) = env("SPEECH_TRAINER_PROGRESS") {
        return Some(PathBuf::from(p));
    }
    let base = env("XDG_DATA_HOME").map(PathBuf::from).or_else(|| env("HOME").map(|h| PathBuf::from(h).join(".local/share")))?;
    Some(base.join("speech-trainer/progress.json"))
}

impl Progress {
    pub fn in_memory() -> Self {
        Self { path: None, data: Mutex::new(Data::default()) }
    }

    /// Reads the file; a missing or unreadable file means a fresh start.
    pub fn load(path: &Path) -> Self {
        let data = match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
                tracing::warn!("progress file {} is not valid, starting empty: {e}", path.display());
                Data::default()
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Data::default(),
            Err(e) => {
                tracing::warn!("cannot read progress file {}: {e}", path.display());
                Data::default()
            }
        };
        Self { path: Some(path.to_path_buf()), data: Mutex::new(data) }
    }

    fn lock(&self) -> MutexGuard<'_, Data> {
        self.data.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn save(&self, data: &Data) {
        let Some(path) = &self.path else { return };
        let result = (|| -> std::io::Result<()> {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let tmp = path.with_extension("json.tmp");
            let json = serde_json::to_string_pretty(data).map_err(std::io::Error::other)?;
            std::fs::write(&tmp, json)?;
            std::fs::rename(&tmp, path)
        })();
        if let Err(e) = result {
            tracing::warn!("cannot save progress to {}: {e}", path.display());
        }
    }

    pub fn dialog_done(&self, name: &str) {
        let mut data = self.lock();
        if data.dialogs.insert(name.to_string()) {
            self.save(&data);
        }
    }

    pub fn video_seen(&self, id: &str) {
        let mut data = self.lock();
        if data.videos.insert(id.to_string()) {
            self.save(&data);
        }
    }

    /// The finished dialogs among `all`. When every one of them is finished the
    /// list is cleared and a new round begins.
    pub fn done_dialogs<'a>(&self, all: impl Iterator<Item = &'a str>) -> BTreeSet<String> {
        let mut data = self.lock();
        let all: Vec<&str> = all.collect();
        let done = round(&mut data.dialogs, &all);
        self.save(&data);
        done
    }

    /// The watched videos among `all`, with the same new-round rule.
    pub fn seen_videos<'a>(&self, all: impl Iterator<Item = &'a str>) -> BTreeSet<String> {
        let mut data = self.lock();
        let all: Vec<&str> = all.collect();
        let seen = round(&mut data.videos, &all);
        self.save(&data);
        seen
    }
}

fn round(stored: &mut BTreeSet<String>, all: &[&str]) -> BTreeSet<String> {
    let relevant: BTreeSet<String> = all.iter().filter(|n| stored.contains(**n)).map(|n| n.to_string()).collect();
    if !all.is_empty() && relevant.len() == all.len() {
        for n in all {
            stored.remove(*n);
        }
        return BTreeSet::new();
    }
    relevant
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("st-progress-{tag}-{}", std::process::id()))
    }

    #[test]
    fn what_was_done_survives_a_restart() -> std::io::Result<()> {
        let d = dir("restart");
        let path = d.join("nested/progress.json");
        let p = Progress::load(&path);
        p.dialog_done("16_the_job_interview");
        p.video_seen("2oScAyV9p0A");
        let again = Progress::load(&path);
        assert_eq!(again.done_dialogs(["16_the_job_interview", "17_x"].into_iter()).len(), 1);
        assert!(again.seen_videos(["2oScAyV9p0A", "b"].into_iter()).contains("2oScAyV9p0A"));
        std::fs::remove_dir_all(&d)
    }

    #[test]
    fn a_broken_file_means_a_fresh_start() -> std::io::Result<()> {
        let d = dir("broken");
        std::fs::create_dir_all(&d)?;
        let path = d.join("progress.json");
        std::fs::write(&path, "{ not json")?;
        let p = Progress::load(&path);
        assert!(p.seen_videos(["a", "b"].into_iter()).is_empty());
        std::fs::remove_dir_all(&d)
    }

    #[test]
    fn names_that_no_longer_exist_are_ignored() {
        let p = Progress::in_memory();
        p.dialog_done("deleted_dialog");
        p.dialog_done("a");
        assert_eq!(p.done_dialogs(["a", "b"].into_iter()), BTreeSet::from(["a".to_string()]));
    }

    #[test]
    fn when_everything_is_done_a_new_round_begins() {
        let p = Progress::in_memory();
        p.video_seen("a");
        p.video_seen("b");
        assert!(p.seen_videos(["a", "b"].into_iter()).is_empty(), "all watched: start over");
        p.video_seen("a");
        assert_eq!(p.seen_videos(["a", "b"].into_iter()).len(), 1, "the new round counts again");
        assert!(p.seen_videos(std::iter::empty()).is_empty());
    }

    #[test]
    fn the_default_path_follows_the_environment() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| v.to_string())
        };
        assert_eq!(default_path(env(&[("SPEECH_TRAINER_PROGRESS", "/x/p.json"), ("HOME", "/h")])), Some(PathBuf::from("/x/p.json")));
        assert_eq!(default_path(env(&[("HOME", "/h")])), Some(PathBuf::from("/h/.local/share/speech-trainer/progress.json")));
        assert_eq!(default_path(env(&[("XDG_DATA_HOME", "/d"), ("HOME", "/h")])), Some(PathBuf::from("/d/speech-trainer/progress.json")));
        assert_eq!(default_path(env(&[])), None);
    }
}
