//! Command line of the client. A flag wins over its environment variable,
//! which wins over the default.

pub const DEFAULT_PAUSE_MS: u32 = 2000;
pub const DEFAULT_MAX_UTTERANCE_SEC: u32 = 30;

pub const USAGE: &str = concat!(
    "speech-trainer client: speak English with a tutor; it listens, answers by voice and corrects you.\n",
    "\n",
    "usage: client [dialog-name | --video] [--text] [--no-calibration] [--pause-ms N] [--max-utterance-sec N]\n",
    "\n",
    "options:\n",
    "  dialog-name            dialog to start with; without it the server picks a random dialog\n",
    "                         you have not finished yet\n",
    "  --video                video mode: the tutor picks a random unwatched video and gives you the\n",
    "                         link; watch it on YouTube, press Enter, and the training on it starts\n",
    "  --text                 type your answers instead of speaking (no microphone needed);\n",
    "                         the line /continue acts as the silence window after a correction\n",
    "  --no-calibration       skip the microphone check at start (quiet, then say something)\n",
    "  --pause-ms N           silence that ends your answer, in milliseconds (default 2000,\n",
    "                         env SPEECH_TRAINER_PAUSE_MS); raise it if you need time to think\n",
    "  --max-utterance-sec N  longest single answer in seconds (default 30,\n",
    "                         env SPEECH_TRAINER_MAX_UTTERANCE_SEC)\n",
    "  -h, --help             this text\n",
    "\n",
    "environment:\n",
    "  SPEECH_TRAINER_URL             server address (default ws://127.0.0.1:8765/ws)\n",
    "  SPEECH_TRAINER_ARECORD_DEVICE  microphone device for arecord (default: system default)\n",
    "  SPEECH_TRAINER_APLAY_DEVICE    speaker device for aplay (default: system default)\n",
    "\n",
    "while the lesson runs:\n",
    "  A beep means the microphone is open and you may speak. Enter after a video link starts the\n",
    "  training; Enter on a pause resumes the lesson.\n",
    "  After a correction (in English) say \"in Russian\" within a few seconds for the Russian version,\n",
    "  or stay silent and the lesson goes on.\n",
    "  Every command starts with the word \"command\" (anything else is taken as your answer):\n",
    "    command next dialog | previous dialog | another dialog | dialog about <topic> | dialog <number>\n",
    "    command start over | next | skip | repeat | answer\n",
    "    command video mode | random video | another video | next video\n",
    "    command pause / command resume   pause the lesson (the microphone stays on, nothing is answered)\n",
    "  Progress (finished dialogs, watched videos) is kept on the server in\n",
    "  ~/.local/share/speech-trainer/progress.json.",
);

#[derive(Debug, PartialEq, Eq)]
pub struct Args {
    pub dialog: Option<String>,
    pub video: bool,
    pub text_mode: bool,
    pub calibrate: bool,
    pub pause_ms: u32,
    pub max_utterance_sec: u32,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Parsed {
    Help,
    Run(Args),
}

fn number(name: &str, value: &str, min: u32, max: u32) -> Result<u32, String> {
    value
        .trim()
        .parse::<u32>()
        .ok()
        .filter(|n| (min..=max).contains(n))
        .ok_or_else(|| format!("{name} must be a whole number from {min} to {max}, got {value:?}"))
}

pub fn parse(args: &[String], env: impl Fn(&str) -> Option<String>) -> Result<Parsed, String> {
    let mut dialog = None;
    let mut text_mode = false;
    let mut video = false;
    let mut calibrate = true;
    let mut pause = env("SPEECH_TRAINER_PAUSE_MS").map(|v| ("SPEECH_TRAINER_PAUSE_MS", v));
    let mut max_sec = env("SPEECH_TRAINER_MAX_UTTERANCE_SEC").map(|v| ("SPEECH_TRAINER_MAX_UTTERANCE_SEC", v));

    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
            _ => (arg.as_str(), None),
        };
        let mut value = |name: &str| -> Result<String, String> {
            inline.clone().or_else(|| it.next().cloned()).ok_or_else(|| format!("{name} needs a value"))
        };
        match flag {
            "-h" | "--help" => return Ok(Parsed::Help),
            "--text" => text_mode = true,
            "--video" => video = true,
            "--no-calibration" => calibrate = false,
            "--pause-ms" => pause = Some(("--pause-ms", value("--pause-ms")?)),
            "--max-utterance-sec" => max_sec = Some(("--max-utterance-sec", value("--max-utterance-sec")?)),
            f if f.starts_with('-') => return Err(format!("unknown option {f}")),
            name => {
                if dialog.replace(name.to_string()).is_some() {
                    return Err("only one dialog name can be given".into());
                }
            }
        }
    }

    let pause_ms = match pause {
        Some((name, v)) => number(name, &v, 100, 60_000)?,
        None => DEFAULT_PAUSE_MS,
    };
    let max_utterance_sec = match max_sec {
        Some((name, v)) => number(name, &v, 1, 300)?,
        None => DEFAULT_MAX_UTTERANCE_SEC,
    };
    if max_utterance_sec * 1000 <= pause_ms {
        return Err(format!("the longest answer ({max_utterance_sec} s) must be longer than the pause ({pause_ms} ms)"));
    }
    if video && dialog.is_some() {
        return Err("give either a dialog name or --video, not both".into());
    }
    Ok(Parsed::Run(Args { dialog, video, text_mode, calibrate, pause_ms, max_utterance_sec }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(args: &[&str], env: &[(&str, &str)]) -> Result<Parsed, String> {
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        parse(&args, |k| env.iter().find(|(n, _)| *n == k).map(|(_, v)| v.to_string()))
    }

    fn args(p: Result<Parsed, String>) -> Result<Args, String> {
        match p? {
            Parsed::Run(a) => Ok(a),
            Parsed::Help => Err("unexpected help".into()),
        }
    }

    #[test]
    fn defaults_are_two_seconds_of_pause_and_thirty_seconds_per_answer() -> Result<(), String> {
        let a = args(run(&[], &[]))?;
        assert_eq!(a, Args { dialog: None, video: false, text_mode: false, calibrate: true, pause_ms: 2000, max_utterance_sec: 30 });
        Ok(())
    }

    #[test]
    fn flags_are_read_with_a_space_or_an_equals_sign_and_do_not_become_the_dialog() -> Result<(), String> {
        let a = args(run(&["--pause-ms", "3500", "16_the_job_interview", "--text", "--max-utterance-sec=45"], &[]))?;
        assert_eq!(a, Args { dialog: Some("16_the_job_interview".into()), video: false, text_mode: true, calibrate: true, pause_ms: 3500, max_utterance_sec: 45 });
        assert_eq!(args(run(&["--pause-ms=1200"], &[]))?.pause_ms, 1200);
        Ok(())
    }

    #[test]
    fn a_flag_beats_the_environment_which_beats_the_default() -> Result<(), String> {
        let env = [("SPEECH_TRAINER_PAUSE_MS", "2500"), ("SPEECH_TRAINER_MAX_UTTERANCE_SEC", "60")];
        let a = args(run(&[], &env))?;
        assert_eq!((a.pause_ms, a.max_utterance_sec), (2500, 60));
        assert_eq!(args(run(&["--pause-ms", "900"], &env))?.pause_ms, 900);
        Ok(())
    }

    #[test]
    fn bad_input_is_rejected_with_a_clear_message() {
        for (input, needle) in [
            (vec!["--pause-ms", "abc"], "whole number"),
            (vec!["--pause-ms", "10"], "from 100"),
            (vec!["--pause-ms"], "needs a value"),
            (vec!["--pause-ms", "5000", "--max-utterance-sec", "4"], "longer than the pause"),
            (vec!["--bogus"], "unknown option"),
            (vec!["a", "b"], "only one dialog"),
            (vec!["--video", "16_the_job_interview"], "either a dialog name or --video"),
        ] {
            match run(&input, &[]) {
                Err(m) => assert!(m.contains(needle), "{input:?}: {m}"),
                Ok(p) => panic!("{input:?} was accepted: {p:?}"),
            }
        }
    }

    #[test]
    fn the_video_flag_is_read() -> Result<(), String> {
        let a = args(run(&["--video", "--text"], &[]))?;
        assert!(a.video && a.text_mode && a.dialog.is_none());
        Ok(())
    }

    #[test]
    fn calibration_can_be_switched_off() -> Result<(), String> {
        assert!(!args(run(&["--no-calibration"], &[]))?.calibrate);
        Ok(())
    }

    #[test]
    fn the_help_text_mentions_every_option_and_variable() {
        for needle in [
            "--video", "--text", "--no-calibration", "--pause-ms", "--max-utterance-sec", "--help",
            "SPEECH_TRAINER_URL", "SPEECH_TRAINER_PAUSE_MS", "SPEECH_TRAINER_MAX_UTTERANCE_SEC",
            "SPEECH_TRAINER_ARECORD_DEVICE", "SPEECH_TRAINER_APLAY_DEVICE", "command pause", "in Russian",
        ] {
            assert!(USAGE.contains(needle), "the help text does not mention {needle}");
        }
    }

    #[test]
    fn help_is_recognised() {
        assert_eq!(run(&["--help"], &[]), Ok(Parsed::Help));
    }
}
