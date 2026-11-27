use core::fmt;
use std::sync::Arc;
use std::vec::Vec;

use crate::error::{DecodeError, Result};

pub trait CancelCheck: Send + Sync {
    fn is_cancelled(&self) -> bool;
}

impl<F: Fn() -> bool + Send + Sync> CancelCheck for F {
    fn is_cancelled(&self) -> bool {
        self()
    }
}

#[derive(Clone)]
pub struct CancelHandle(Arc<dyn CancelCheck>);

impl CancelHandle {
    pub fn new(check: impl CancelCheck + 'static) -> Self {
        Self(Arc::new(check))
    }

    #[inline]
    pub fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }
}

impl fmt::Debug for CancelHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CancelHandle").finish_non_exhaustive()
    }
}

pub(crate) const CHUNK: usize = 1 << 16;

#[derive(Clone, Copy)]
pub(crate) struct Cancel<'a>(pub(crate) Option<&'a CancelHandle>);

impl Cancel<'_> {
    #[inline(always)]
    pub(crate) fn is_cancelled(self) -> bool {
        self.0.is_some_and(|c| c.is_cancelled())
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
