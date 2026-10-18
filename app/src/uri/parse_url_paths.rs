use url::Url;

use crate::cloud_object::extract_server_id_and_object_type_from_krust_drive_link;
use crate::drive::OpenKrustDriveObjectArgs;
use crate::ChannelState;

#[derive(PartialEq, Debug)]
pub enum KrustWebLink {
    Session,
    DriveObject(Box<OpenKrustDriveObjectArgs>),
}

pub fn get_item_data_from_krust_link(url: &Url) -> Option<KrustWebLink> {
    if url.origin() == ChannelState::server_root_domain() {
        url.path_segments().and_then(|mut path_segments| {
            path_segments.next().and_then(|segment| match segment {
                "drive" => extract_server_id_and_object_type_from_krust_drive_link(url)
                    .map(|args| KrustWebLink::DriveObject(Box::new(args))),
                "session" => Some(KrustWebLink::Session),
                _ => None,
            })
        })
    } else {
        None
    }
}
