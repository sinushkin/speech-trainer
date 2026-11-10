//! The phrasebook: phrases the learner did not manage, kept in a CSV file and
//! brought back by spaced repetition (the Leitner system).
//!
//! A phrase sits in a box 0..=5. A miss puts it in box 0 and it is due at once;
//! every success moves it one box up and the next review is pushed further away
//! (0, 1, 3, 7, 14, 30 days), which follows the forgetting curve: review just
//! before the memory fades. The CSV can be opened in any spreadsheet.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

/// Days from now until the next review, by box.
const INTERVALS: [i64; 6] = [0, 1, 3, 7, 14, 30];
const TOP_BOX: u8 = 5;

/// A day as a number of days since 1970-01-01 (UTC).
pub type Day = i64;

pub fn today() -> Day {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    i64::try_from(secs / 86_400).unwrap_or(0)
}

/// Howard Hinnant's civil-from-days.
pub fn iso(day: Day) -> String {
    let z = day + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

pub fn parse_iso(text: &str) -> Option<Day> {
    let mut parts = text.trim().splitn(3, '-').map(|p| p.parse::<i64>().ok());
    let (y, m, d) = (parts.next()??, parts.next()??, parts.next()??);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let y = y - i64::from(m <= 2);
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Row {
    pub phrase: String,
    pub hint_ru: String,
    pub model_answer: String,
    /// The dialog file name or the video id the phrase came from.
    pub source: String,
    /// Link to the exact moment in the video, empty when unknown.
    pub url: String,
    #[serde(rename = "box")]
    pub level: u8,
    pub due: String,
    pub fails: u32,
    pub successes: u32,
    pub last_seen: String,
}

/// What the tutor reports about a phrase that went badly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Missed {
    pub phrase: String,
    pub hint_ru: String,
    pub model_answer: String,
    pub source: String,
    pub url: String,
}

pub struct Phrasebook {
    /// `None` keeps everything in memory (tests).
    path: Option<PathBuf>,
    rows: Mutex<Vec<Row>>,
}

/// `$SPEECH_TRAINER_PHRASES`, else `~/.local/share/speech-trainer/phrases.csv`.
pub fn default_path(env: impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    if let Some(p) = env("SPEECH_TRAINER_PHRASES") {
        return Some(PathBuf::from(p));
    }
    let base = env("XDG_DATA_HOME").map(PathBuf::from).or_else(|| env("HOME").map(|h| PathBuf::from(h).join(".local/share")))?;
    Some(base.join("speech-trainer/phrases.csv"))
}

fn key(phrase: &str) -> String {
    phrase.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

impl Phrasebook {
    pub fn in_memory() -> Self {
        Self { path: None, rows: Mutex::new(Vec::new()) }
    }

    /// Reads the file. A file that cannot be read is moved aside (`.bad`), never overwritten.
    pub fn load(path: &Path) -> Self {
        let rows = match read_rows(path) {
            Ok(rows) => rows,
            Err(e) if e.contains("No such file") => Vec::new(),
            Err(e) => {
                let aside = path.with_extension("csv.bad");
                tracing::warn!("phrasebook {} is unreadable ({e}); moving it to {}", path.display(), aside.display());
                if let Err(e) = std::fs::rename(path, &aside) {
                    tracing::warn!("cannot move it aside: {e}");
                }
                Vec::new()
            }
        };
        Self { path: Some(path.to_path_buf()), rows: Mutex::new(rows) }
    }

    fn lock(&self) -> MutexGuard<'_, Vec<Row>> {
        self.rows.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn save(&self, rows: &[Row]) {
        let Some(path) = &self.path else { return };
        if let Err(e) = write_rows(path, rows) {
            tracing::warn!("cannot save the phrasebook to {}: {e}", path.display());
        }
    }

    /// The learner did not manage the phrase: box 0, due at once.
    pub fn record_miss(&self, m: &Missed, today: Day) {
        let phrase = m.phrase.trim();
        if phrase.is_empty() {
            return;
        }
        let mut rows = self.lock();
        let now = iso(today);
        match rows.iter_mut().find(|r| key(&r.phrase) == key(phrase)) {
            Some(r) => {
                r.level = 0;
                r.due = now.clone();
                r.fails += 1;
                r.last_seen = now;
                for (field, new) in [(&mut r.hint_ru, &m.hint_ru), (&mut r.model_answer, &m.model_answer), (&mut r.source, &m.source), (&mut r.url, &m.url)] {
                    if !new.trim().is_empty() {
                        *field = new.trim().to_string();
                    }
                }
            }
            None => rows.push(Row {
                phrase: phrase.to_string(),
                hint_ru: m.hint_ru.trim().to_string(),
                model_answer: m.model_answer.trim().to_string(),
                source: m.source.trim().to_string(),
                url: m.url.trim().to_string(),
                level: 0,
                due: now.clone(),
                fails: 1,
                successes: 0,
                last_seen: now,
            }),
        }
        self.save(&rows);
    }

    /// The learner managed a phrase of the phrasebook: one box up, the next review later.
    /// Phrases that are not in the phrasebook are ignored.
    pub fn record_pass(&self, phrase: &str, today: Day) {
        let mut rows = self.lock();
        let Some(r) = rows.iter_mut().find(|r| key(&r.phrase) == key(phrase)) else { return };
        r.level = (r.level + 1).min(TOP_BOX);
        r.successes += 1;
        r.due = iso(today + INTERVALS[usize::from(r.level)]);
        r.last_seen = iso(today);
        self.save(&rows);
    }

    /// Phrases due today or earlier, the weakest (lowest box) first, then the longest overdue.
    pub fn due(&self, today: Day, limit: usize) -> Vec<Row> {
        let mut due: Vec<Row> = self.lock().iter().filter(|r| parse_iso(&r.due).is_none_or(|d| d <= today)).cloned().collect();
        due.sort_by(|a, b| a.level.cmp(&b.level).then_with(|| a.due.cmp(&b.due)));
        due.truncate(limit);
        due
    }

    /// How many phrases are due.
    pub fn due_count(&self, today: Day) -> usize {
        self.lock().iter().filter(|r| parse_iso(&r.due).is_none_or(|d| d <= today)).count()
    }

    pub fn len(&self) -> usize {
        self.lock().len()
    }
}

fn read_rows(path: &Path) -> Result<Vec<Row>, String> {
    let mut reader = csv::Reader::from_path(path).map_err(|e| e.to_string())?;
    reader.deserialize().collect::<Result<Vec<Row>, _>>().map_err(|e| e.to_string())
}

fn write_rows(path: &Path, rows: &[Row]) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension("csv.tmp");
    let mut writer = csv::Writer::from_path(&tmp).map_err(|e| e.to_string())?;
    for row in rows {
        writer.serialize(row).map_err(|e| e.to_string())?;
    }
    writer.flush().map_err(|e| e.to_string())?;
    drop(writer);
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

/// The due phrases as lines for the tutor's prompt.
pub fn review_text(rows: &[Row]) -> String {
    rows.iter()
        .enumerate()
        .map(|(i, r)| {
            let link = if r.url.is_empty() { String::new() } else { format!(" | link {}", r.url) };
            format!("{}. {} | hint: {} | model answer: {} | source {}{link}", i + 1, r.phrase, r.hint_ru, r.model_answer, r.source)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn missed(phrase: &str) -> Missed {
        Missed {
            phrase: phrase.into(),
            hint_ru: "работать круглосуточно".into(),
            model_answer: "We work around the clock, seven days a week.".into(),
            source: "16_the_job_interview".into(),
            url: "https://youtu.be/x?t=5".into(),
        }
    }

    const DAY0: Day = 20_000; // 2024-10-04

    #[test]
    fn dates_round_trip_and_match_known_values() {
        assert_eq!(iso(0), "1970-01-01");
        assert_eq!(iso(DAY0), "2024-10-04");
        assert_eq!(iso(19_782), "2024-02-29");
        for d in [-1, 0, 59, 365, 11_016, DAY0, 25_000] {
            assert_eq!(parse_iso(&iso(d)), Some(d), "{d}");
        }
        assert_eq!(parse_iso("2024-13-01"), None);
        assert_eq!(parse_iso("tomorrow"), None);
    }

    #[test]
    fn a_missed_phrase_is_due_at_once_and_a_repeated_miss_does_not_duplicate_it() {
        let p = Phrasebook::in_memory();
        p.record_miss(&missed("Around the clock"), DAY0);
        p.record_miss(&missed("around  the CLOCK"), DAY0 + 1);
        assert_eq!(p.len(), 1);
        let due = p.due(DAY0 + 1, 10);
        assert_eq!(due.len(), 1);
        assert_eq!((due[0].level, due[0].fails, due[0].due.as_str()), (0, 2, iso(DAY0 + 1).as_str()));
    }

    #[test]
    fn successes_push_the_next_review_further_away_and_a_miss_starts_over() {
        let p = Phrasebook::in_memory();
        p.record_miss(&missed("around the clock"), DAY0);
        let mut day = DAY0;
        for expected_gap in [1, 3, 7, 14, 30, 30] {
            p.record_pass("Around the clock", day);
            assert!(p.due(day, 10).is_empty(), "not due on the same day");
            assert!(p.due(day + expected_gap - 1, 10).is_empty());
            assert_eq!(p.due(day + expected_gap, 10).len(), 1, "due after {expected_gap} days");
            day += expected_gap;
        }
        p.record_miss(&missed("around the clock"), day);
        let due = p.due(day, 10);
        assert_eq!((due[0].level, due[0].successes), (0, 6));
    }

    #[test]
    fn unknown_phrases_are_not_added_by_a_pass_and_the_weakest_come_first() {
        let p = Phrasebook::in_memory();
        p.record_pass("never missed", DAY0);
        assert_eq!(p.len(), 0);
        p.record_miss(&missed("a"), DAY0);
        p.record_pass("a", DAY0);
        p.record_miss(&missed("b"), DAY0 + 1);
        let order: Vec<String> = p.due(DAY0 + 5, 10).into_iter().map(|r| r.phrase).collect();
        assert_eq!(order, ["b", "a"], "box 0 before box 1");
        assert_eq!(p.due(DAY0 + 5, 1).len(), 1);
        assert_eq!(p.due_count(DAY0 + 5), 2);
    }

    #[test]
    fn the_csv_survives_a_restart_and_keeps_commas_quotes_and_russian() -> Result<(), String> {
        let dir = std::env::temp_dir().join(format!("st-phrasebook-{}", std::process::id()));
        let path = dir.join("nested/phrases.csv");
        let p = Phrasebook::load(&path);
        let mut m = missed("get on my nerves");
        m.model_answer = "He's loud, \"really\" loud, and he gets on my nerves.".into();
        p.record_miss(&m, DAY0);
        let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
        assert!(text.starts_with("phrase,hint_ru,model_answer,source,url,box,due,fails,successes,last_seen"), "{text}");
        let again = Phrasebook::load(&path);
        let rows = again.due(DAY0, 10);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].model_answer, m.model_answer);
        assert_eq!(rows[0].hint_ru, "работать круглосуточно");
        std::fs::remove_dir_all(&dir).map_err(|e| e.to_string())
    }

    #[test]
    fn a_broken_file_is_moved_aside_not_overwritten() -> Result<(), String> {
        let dir = std::env::temp_dir().join(format!("st-phrasebook-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let path = dir.join("phrases.csv");
        std::fs::write(&path, "phrase,box\nx,not-a-number\n").map_err(|e| e.to_string())?;
        let p = Phrasebook::load(&path);
        assert_eq!(p.len(), 0);
        assert!(dir.join("phrases.csv.bad").is_file());
        std::fs::remove_dir_all(&dir).map_err(|e| e.to_string())
    }

    #[test]
    fn the_review_text_lists_phrase_hint_answer_source_and_link() {
        let p = Phrasebook::in_memory();
        p.record_miss(&missed("around the clock"), DAY0);
        let text = review_text(&p.due(DAY0, 10));
        assert_eq!(
            text,
            "1. around the clock | hint: работать круглосуточно | model answer: We work around the clock, seven days a week. | source 16_the_job_interview | link https://youtu.be/x?t=5"
        );
    }

    #[test]
    fn the_default_path_follows_the_environment() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| v.to_string())
        };
        assert_eq!(default_path(env(&[("SPEECH_TRAINER_PHRASES", "/x.csv")])), Some(PathBuf::from("/x.csv")));
        assert_eq!(default_path(env(&[("HOME", "/h")])), Some(PathBuf::from("/h/.local/share/speech-trainer/phrases.csv")));
    }
}
