use url::Url;

use crate::cloud_object::extract_server_id_and_object_type_from_bork_drive_link;
use crate::drive::OpenBorkDriveObjectArgs;
use crate::ChannelState;

#[derive(PartialEq, Debug)]
pub enum BorkWebLink {
    Session,
    DriveObject(Box<OpenBorkDriveObjectArgs>),
}

pub fn get_item_data_from_bork_link(url: &Url) -> Option<BorkWebLink> {
    if url.origin() == ChannelState::server_root_domain() {
        url.path_segments().and_then(|mut path_segments| {
            path_segments.next().and_then(|segment| match segment {
                "drive" => extract_server_id_and_object_type_from_bork_drive_link(url)
                    .map(|args| BorkWebLink::DriveObject(Box::new(args))),
                "session" => Some(BorkWebLink::Session),
                _ => None,
            })
        })
    } else {
        None
    }
}
