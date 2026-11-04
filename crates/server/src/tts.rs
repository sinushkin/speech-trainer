//! Speech synthesis. Native English and Russian voices are not implemented yet,
//! so for now the server only sends text; the lesson itself keeps working.

use crate::backend::{BackendError, Synthesizer};
use crate::markup::Lang;

pub struct NoSpeech;

impl Synthesizer for NoSpeech {
    async fn synthesize(&self, lang: Lang, _text: &str) -> Result<Vec<u8>, BackendError> {
        Err(BackendError::Failed(format!("no {} voice is configured yet", lang.code())))
    }
}
