//! Drafts, image reads and history share one owner; epochs discard stale replies.
use std::collections::HashMap;

use bone_proto::types::ImageAttachment;

use crate::editor::TextBuffer;
use crate::layout::BufferId;

#[derive(Clone, Default, PartialEq, Eq)]
pub(crate) struct Draft {
    pub text: String,
    pub images: Vec<ImageAttachment>,
    pub pastes: Vec<(String, String)>,
    pub epoch: u64,
}

#[derive(Default)]
pub(crate) struct Composer {
    pub images: Vec<ImageAttachment>,
    pub pastes: Vec<(String, String)>,
    pub epoch: u64,
    pub next_epoch: u64,
    pub drafts: HashMap<BufferId, Draft>,
    pub pending: HashMap<(BufferId, u64), usize>,
    pub saved: Option<Draft>,
    pub history: Vec<Draft>,
    pub history_pos: Option<usize>,
}

impl Composer {
    pub fn snapshot(&self, prompt: &TextBuffer) -> Draft {
        Draft {
            text: prompt.text(),
            images: self.images.clone(),
            pastes: self.pastes.clone(),
            epoch: self.epoch,
        }
    }

    pub fn apply(&mut self, prompt: &mut TextBuffer, draft: Draft) {
        prompt.set_text(&draft.text);
        self.images = draft.images;
        self.pastes = draft.pastes;
        self.epoch = draft.epoch;
    }

    pub fn invalidate(&mut self) {
        self.next_epoch += 1;
        self.epoch = self.next_epoch;
    }

    pub fn reading(&self, buf: BufferId) -> bool {
        self.pending.contains_key(&(buf, self.epoch))
    }

    pub fn finish_read(&mut self, buf: BufferId, epoch: u64) {
        if let Some(n) = self.pending.get_mut(&(buf, epoch)) {
            *n -= 1;
            if *n == 0 {
                self.pending.remove(&(buf, epoch));
            }
        }
    }

    /// Returns true when an image reaches the visible draft. Stale replies are discarded.
    pub fn attach(
        &mut self,
        current: BufferId,
        buf: BufferId,
        epoch: u64,
        image: ImageAttachment,
    ) -> Result<bool, String> {
        let visible = buf == current && epoch == self.epoch;
        let images = if visible {
            &mut self.images
        } else if let Some(draft) = self.drafts.get_mut(&buf).filter(|d| d.epoch == epoch) {
            &mut draft.images
        } else {
            return Ok(false);
        };
        let total: u64 = images.iter().map(|i| i.bytes).sum();
        if images.len() >= bone_media::MAX_IMAGES
            || total + image.bytes > bone_media::MAX_MESSAGE_IMAGE_BYTES
        {
            return Err(
                "image would exceed the message attachment limit; remove an image first".into(),
            );
        }
        images.push(image);
        Ok(visible)
    }
}
