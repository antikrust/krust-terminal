use url::Url;

use crate::cloud_object::extract_server_id_and_object_type_from_derp_drive_link;
use crate::drive::OpenDerpDriveObjectArgs;
use crate::ChannelState;

#[derive(PartialEq, Debug)]
pub enum DerpWebLink {
    Session,
    DriveObject(Box<OpenDerpDriveObjectArgs>),
}

pub fn get_item_data_from_derp_link(url: &Url) -> Option<DerpWebLink> {
    if url.origin() == ChannelState::server_root_domain() {
        url.path_segments().and_then(|mut path_segments| {
            path_segments.next().and_then(|segment| match segment {
                "drive" => extract_server_id_and_object_type_from_derp_drive_link(url)
                    .map(|args| DerpWebLink::DriveObject(Box::new(args))),
                "session" => Some(DerpWebLink::Session),
                _ => None,
            })
        })
    } else {
        None
    }
}
