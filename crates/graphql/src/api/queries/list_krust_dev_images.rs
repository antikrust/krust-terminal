use crate::error::UserFacingError;
use crate::schema;

#[derive(cynic::QueryVariables, Debug)]
pub struct ListKrustDevImagesVariables {}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "RootQuery", variables = "ListKrustDevImagesVariables")]
pub struct ListKrustDevImages {
    #[cynic(rename = "listKrustDevImages")]
    pub list_krust_dev_images: ListKrustDevImagesResult,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct ListKrustDevImagesOutput {
    pub images: Vec<ImageTag>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct ImageTag {
    pub image: String,
    pub repository: String,
    pub tag: String,
}

#[derive(cynic::InlineFragments, Debug)]
pub enum ListKrustDevImagesResult {
    ListKrustDevImagesOutput(ListKrustDevImagesOutput),
    UserFacingError(UserFacingError),
    #[cynic(fallback)]
    Unknown,
}

crate::client::define_operation! {
    ListKrustDevImages(ListKrustDevImagesVariables) -> ListKrustDevImages;
}
