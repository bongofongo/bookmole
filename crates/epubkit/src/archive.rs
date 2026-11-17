//! The bounded zip layer. Entries are looked up and decompressed into memory
//! under [`Limits`]; nothing is ever written to disk.

use std::borrow::Cow;
use std::io::{Read, Seek};

use zip::ZipArchive;

use crate::error::{Error, Limit};
use crate::{Limits, path};

pub(crate) struct Archive<R> {
    zip: ZipArchive<R>,
    limits: Limits,
    /// Decompressed bytes read through this handle so far, failed reads
    /// included.
    consumed: u64,
}

impl<R: Read + Seek> Archive<R> {
    pub(crate) fn new(reader: R, limits: Limits) -> Result<Self, Error> {
        let zip = ZipArchive::new(reader).map_err(Error::zip)?;
        if zip.len() > limits.max_entries {
            return Err(Error::LimitExceeded {
                limit: Limit::Entries,
                path: None,
            });
        }
        Ok(Archive {
            zip,
            limits,
            consumed: 0,
        })
    }

    pub(crate) fn limits(&self) -> &Limits {
        &self.limits
    }

    /// The entry index a name refers to: the name exactly, then
    /// percent-decoded (the `epub` 2.x crate's order), then — new — with its
    /// `.`/`..` segments folded, so `OEBPS/../cover.jpg` finds `cover.jpg`.
    pub(crate) fn locate(&self, name: &str) -> Option<usize> {
        if let Some(i) = self.zip.index_for_name(name) {
            return Some(i);
        }
        let decoded: Cow<'_, str> = percent_encoding::percent_decode_str(name)
            .decode_utf8()
            .ok()?;
        if let Some(i) = self.zip.index_for_name(&decoded) {
            return Some(i);
        }
        let folded = path::normalize(&decoded)?;
        if folded == decoded {
            return None;
        }
        self.zip.index_for_name(&folded)
    }

    pub(crate) fn contains(&self, name: &str) -> bool {
        self.locate(name).is_some()
    }

    /// Every entry name, in central-directory order. Names the zip cannot
    /// decode are skipped.
    pub(crate) fn names(&self) -> impl Iterator<Item = String> + '_ {
        self.zip
            .file_names()
            .filter_map(|n| n.ok().map(|n| n.into_owned()))
    }

    /// The decompressed bytes of an entry, bounded by the single-entry cap
    /// and by what is left of the whole-package budget. The bound is enforced
    /// on the bytes actually produced; the archive's declared size is only a
    /// fast way to refuse early.
    pub(crate) fn read(&mut self, name: &str) -> Result<Vec<u8>, Error> {
        let index = self.locate(name).ok_or_else(|| Error::MissingEntry {
            path: name.to_string(),
        })?;
        let left = self.limits.max_total_bytes.saturating_sub(self.consumed);
        let (budget, limit) = if left < self.limits.max_entry_bytes {
            (left, Limit::TotalBytes)
        } else {
            (self.limits.max_entry_bytes, Limit::EntryBytes)
        };
        let over = || Error::LimitExceeded {
            limit,
            path: Some(name.to_string()),
        };

        let file = self.zip.by_index(index).map_err(Error::zip)?;
        if file.size() > budget {
            return Err(over());
        }
        let mut buf = Vec::new();
        let read = file.take(budget.saturating_add(1)).read_to_end(&mut buf);
        self.consumed = self.consumed.saturating_add(buf.len() as u64);
        read.map_err(
            |e| match e.get_ref().map(|r| r.is::<zip::result::ZipError>()) {
                // zip reports a corrupt stream or a bad checksum as an i/o error
                // wrapping its own; it is still the archive's fault, not the disk's.
                Some(true) => Error::Zip(Box::new(e)),
                _ => Error::Io(e),
            },
        )?;
        if buf.len() as u64 > budget {
            return Err(over());
        }
        Ok(buf)
    }
}
