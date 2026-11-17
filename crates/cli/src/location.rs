//! The one line the CLI says when the library sits in a cloud-synced folder.
//!
//! The fact is the engine's (`bookmole::cloud_folder`); the sentence is
//! this frontend's. It goes to **stderr**, once per invocation that opens the
//! library, and never to stdout, which scripts parse. `--quiet` silences it,
//! and `config` never reaches it because `config` never opens the engine.
//!
//! Once per invocation rather than once ever: each invocation opens the
//! database again, so the risk the line names is present every time, and a
//! "shown already" marker would be state written somewhere for a warning's
//! sake. It is a statement of what is the case, never an instruction, and the
//! library opens either way.

use bookmole::{CloudFolder, CloudService};

/// The sync service, as its maker spells it. An unnamed File Provider client
/// under `~/Library/CloudStorage` is "a cloud storage folder".
pub fn service_name(s: CloudService) -> &'static str {
    match s {
        CloudService::ICloudDrive => "iCloud Drive",
        CloudService::Dropbox => "Dropbox",
        CloudService::GoogleDrive => "Google Drive",
        CloudService::OneDrive => "OneDrive",
        CloudService::ProtonDrive => "Proton Drive",
        CloudService::Box => "Box",
        CloudService::CloudStorage => "a cloud storage folder",
    }
}

/// The line, whole.
pub fn cloud_folder_line(f: &CloudFolder) -> String {
    format!(
        "The library is in {} ({}). Syncing can damage a library while it is open; \
         moving it out of the synced folder avoids that. \
         BOOKMOLE_DATA_DIR sets where it lives.",
        service_name(f.service),
        f.root.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn line(service: CloudService) -> String {
        cloud_folder_line(&CloudFolder {
            service,
            root: PathBuf::from("/Users/u/Library/Mobile Documents"),
        })
    }

    #[test]
    fn the_line_names_the_service_the_root_and_the_variable() {
        assert_eq!(
            line(CloudService::ICloudDrive),
            "The library is in iCloud Drive (/Users/u/Library/Mobile Documents). \
             Syncing can damage a library while it is open; moving it out of the \
             synced folder avoids that. BOOKMOLE_DATA_DIR sets where it lives."
        );
    }

    /// The axiom's banned words, and no line break: it is one line on stderr.
    #[test]
    fn the_line_is_one_line_and_frames_nothing_as_a_task() {
        for s in [
            CloudService::ICloudDrive,
            CloudService::Dropbox,
            CloudService::GoogleDrive,
            CloudService::OneDrive,
            CloudService::ProtonDrive,
            CloudService::Box,
            CloudService::CloudStorage,
        ] {
            let l = line(s).to_lowercase();
            assert!(!l.contains('\n'), "{l}");
            for banned in [
                "yet",
                " left",
                "to go",
                "remaining",
                "behind",
                "on track",
                "goal",
                "streak",
                "reward",
            ] {
                assert!(!l.contains(banned), "{banned:?} in {l}");
            }
        }
    }
}
