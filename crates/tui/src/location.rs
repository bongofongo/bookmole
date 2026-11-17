//! The one status line the TUI shows when the library sits in a cloud-synced
//! folder.
//!
//! The fact is the engine's (`bookmole::cloud_folder`); the sentence is
//! this frontend's, and deliberately the CLI's sentence without the root's
//! path, which a one-row status line cannot afford. It is set once, at launch,
//! through the ordinary status line (`App::status`), and goes the way every
//! status does: the next thing the app has to say replaces it. The library
//! opens either way.

use bookmole::{CloudFolder, CloudService};

/// The sync service, as its maker spells it.
fn service_name(s: CloudService) -> &'static str {
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
        "The library is in {}. Syncing can damage a library while it is open; \
         moving it out of the synced folder avoids that. \
         BOOKMOLE_DATA_DIR sets where it lives.",
        service_name(f.service)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn the_line_names_the_service_and_the_variable() {
        let f = CloudFolder {
            service: CloudService::ICloudDrive,
            root: PathBuf::from("/Users/u/Library/Mobile Documents"),
        };
        assert_eq!(
            cloud_folder_line(&f),
            "The library is in iCloud Drive. Syncing can damage a library while it \
             is open; moving it out of the synced folder avoids that. \
             BOOKMOLE_DATA_DIR sets where it lives."
        );
    }

    /// The axiom's banned words, and no line break: it is one status row.
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
            let l = cloud_folder_line(&CloudFolder {
                service: s,
                root: PathBuf::from("/r"),
            })
            .to_lowercase();
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
