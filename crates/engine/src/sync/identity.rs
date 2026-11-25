//! Who this device is in a synced library: the library's credentials and the server's address,
//! in one file in the data directory and never in the database, so a copy of the database
//! carries no secret.
//!
//! The file, `sync-identity`, is binary: the four bytes `BMSI`, a format byte (1), then the
//! server's origin and this device's label, each as a big-endian `u16` length and UTF-8 bytes,
//! then the 85 bytes of [`Credentials::to_bytes`], and nothing after. It is written to a
//! temporary file beside it, synced, and renamed over it, and is readable by its owner only on
//! Unix (0600); elsewhere it takes the directory's default permissions.

use std::fmt;
use std::io::Write;
use std::path::Path;

use fongo_sync_client::Credentials;
use zeroize::Zeroizing;

use super::http::Origin;
use crate::error::Result;

pub(crate) const FILE_NAME: &str = "sync-identity";

const MAGIC: &[u8; 4] = b"BMSI";
const FORMAT: u8 = 1;
const CREDENTIALS_LEN: usize = 85;
const TEMPORARY: &str = ".sync-identity-";

pub(crate) struct Identity {
    pub(crate) origin: Origin,
    pub(crate) device_label: String,
    pub(crate) credentials: Credentials,
}

impl fmt::Debug for Identity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Identity")
            .field("origin", &self.origin)
            .field("device_label", &self.device_label)
            .field("credentials", &self.credentials)
            .finish()
    }
}

/// What the data directory holds.
#[derive(Debug)]
pub(crate) enum Stored {
    Missing,
    /// A file that is not one this version wrote; its contents are never shown.
    Unreadable,
    Found(Box<Identity>),
}

pub(crate) fn read(path: &Path) -> Result<Stored> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => Zeroizing::new(bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Stored::Missing),
        Err(e) => return Err(e.into()),
    };
    Ok(decode(&bytes).map_or(Stored::Unreadable, |found| Stored::Found(Box::new(found))))
}

fn decode(bytes: &[u8]) -> Option<Identity> {
    let rest = bytes.strip_prefix(MAGIC)?;
    let (&format, rest) = rest.split_first()?;
    if format != FORMAT {
        return None;
    }
    let (origin, rest) = text(rest)?;
    let (device_label, rest) = text(rest)?;
    if rest.len() != CREDENTIALS_LEN {
        return None;
    }
    Some(Identity {
        origin: Origin::parse(&origin).ok()?,
        device_label,
        credentials: Credentials::from_bytes(rest).ok()?,
    })
}

fn text(bytes: &[u8]) -> Option<(String, &[u8])> {
    let (len, rest) = bytes.split_first_chunk::<2>()?;
    let len = usize::from(u16::from_be_bytes(*len));
    let (text, rest) = rest.split_at_checked(len)?;
    Some((String::from_utf8(text.to_vec()).ok()?, rest))
}

fn encode(identity: &Identity) -> Option<Zeroizing<Vec<u8>>> {
    let mut out = Zeroizing::new(Vec::with_capacity(256));
    out.extend_from_slice(MAGIC);
    out.push(FORMAT);
    for text in [identity.origin.as_str(), &identity.device_label] {
        out.extend_from_slice(&u16::try_from(text.len()).ok()?.to_be_bytes());
        out.extend_from_slice(text.as_bytes());
    }
    out.extend_from_slice(&identity.credentials.to_bytes()[..]);
    Some(out)
}

/// Replaces the file at `path` with `identity`, all or nothing.
pub(crate) fn write(path: &Path, identity: &Identity) -> Result<()> {
    let bytes = encode(identity).ok_or_else(|| {
        crate::error::EngineError::InvalidInput("the sync server's address is too long".into())
    })?;
    let dir = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let mut file = tempfile::Builder::new()
        .prefix(TEMPORARY)
        .tempfile_in(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(&bytes)?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|e| e.error)?;
    #[cfg(unix)]
    std::fs::File::open(dir)?.sync_all()?;
    remove_temporaries(dir)
}

/// Removes the file at `path`, and any temporary a write that stopped part-way left beside it.
pub(crate) fn remove(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
        _ => {}
    }
    match path.parent().filter(|d| !d.as_os_str().is_empty()) {
        Some(dir) if dir.exists() => remove_temporaries(dir),
        _ => Ok(()),
    }
}

/// A process that stops between writing a temporary and renaming it leaves the secret in it.
/// Called only under the data directory's sync lock, so no other write is part-way.
fn remove_temporaries(dir: &Path) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_name().to_string_lossy().starts_with(TEMPORARY) {
            match std::fs::remove_file(entry.path()) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
                _ => {}
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use fongo_sync_client::protocol::{LibraryId, LibraryKeys, SigningKey};

    use super::*;

    fn identity() -> Identity {
        Identity {
            origin: Origin::parse("https://sync.example.test").unwrap(),
            device_label: "Study".into(),
            credentials: Credentials::new(
                LibraryKeys::new(LibraryId([1; 16]), [0x5a; 32]),
                SigningKey::from_bytes(&[0x6b; 32]),
                3,
            ),
        }
    }

    fn secret_free(text: &str) {
        for secret in ["5a5a", "6b6b", "90, 90", "107, 107"] {
            assert!(!text.contains(secret), "{text}");
        }
    }

    #[test]
    fn an_identity_round_trips_through_its_file_and_only_its_owner_can_read_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        assert!(matches!(read(&path).unwrap(), Stored::Missing));
        write(&path, &identity()).unwrap();
        write(&path, &identity()).unwrap();
        let Stored::Found(back) = read(&path).unwrap() else {
            panic!("not read back");
        };
        assert_eq!(back.origin.as_str(), "https://sync.example.test");
        assert_eq!(back.device_label, "Study");
        assert_eq!(
            back.credentials.to_bytes()[..],
            identity().credentials.to_bytes()[..]
        );
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, [FILE_NAME]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        secret_free(&format!("{back:?}"));
        remove(&path).unwrap();
        remove(&path).unwrap();
        assert!(matches!(read(&path).unwrap(), Stored::Missing));
    }

    /// A write that stopped after filling its temporary leaves the secret on disk until the next
    /// write or removal, which take it away.
    #[test]
    fn a_temporary_left_by_a_stopped_write_is_removed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        let left = || {
            let stray = dir.path().join(format!("{TEMPORARY}stopped"));
            std::fs::write(&stray, encode(&identity()).unwrap()).unwrap();
            stray
        };
        let stray = left();
        write(&path, &identity()).unwrap();
        assert!(!stray.exists());
        assert!(path.exists());
        let stray = left();
        remove(&path).unwrap();
        assert!(!stray.exists());
        assert!(!path.exists());
        let stray = left();
        remove(&path).unwrap();
        assert!(!stray.exists());
    }

    #[test]
    fn a_file_this_version_did_not_write_is_unreadable_and_never_shown() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        let good = encode(&identity()).unwrap();
        let mut later = good.to_vec();
        later[4] = 2;
        let cut = good[..good.len() - 1].to_vec();
        let mut longer = good.to_vec();
        longer.push(0);
        for bytes in [later, cut, longer, b"BMSI".to_vec(), Vec::new()] {
            std::fs::write(&path, &bytes).unwrap();
            let stored = read(&path).unwrap();
            assert!(matches!(stored, Stored::Unreadable), "{stored:?}");
        }
    }
}
