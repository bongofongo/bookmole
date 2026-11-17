//! The one line printed when the library sits in a cloud-synced folder. Goes to stderr once per
//! invocation, never stdout; `--quiet` silences it; the library opens either way.

use bookmole::{CloudFolder, CloudService};

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
