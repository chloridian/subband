use std::vec::Vec;

use crate::error::{DecodeError, Result};

pub(crate) const CHUNK: usize = 1 << 16;

#[derive(Clone, Copy)]
pub(crate) struct Cancel<'a>(pub(crate) &'a dyn Fn() -> bool);

impl Cancel<'_> {
    #[inline(always)]
    pub(crate) fn is_cancelled(self) -> bool {
        (self.0)()
    }

    #[inline(always)]
    pub(crate) fn check(self) -> Result<()> {
        if self.is_cancelled() {
            Err(DecodeError::Cancelled)
        } else {
            Ok(())
        }
    }

    pub(crate) fn ensure_len(self, buf: &mut Vec<f32>, len: usize) -> Result<()> {
        if buf.len() >= len {
            return Ok(());
        }
        buf.reserve(len - buf.len());

        while buf.len() < len {
            self.check()?;

            let next = len.min(buf.len() + CHUNK);
            buf.resize(next, 0.0);
        }

        Ok(())
    }

    pub(crate) fn zero_prefix(self, buf: &mut Vec<f32>, len: usize) -> Result<()> {
        let existing = buf.len().min(len);
        for chunk in buf
            .get_mut(..existing)
            .unwrap_or_default()
            .chunks_mut(CHUNK)
        {
            self.check()?;
            chunk.fill(0.0);
        }

        self.ensure_len(buf, len)
    }
}
