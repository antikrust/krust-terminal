use crate::error::UserFacingError;
use crate::schema;

#[derive(cynic::QueryVariables, Debug)]
pub struct ListDerpDevImagesVariables {}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "RootQuery", variables = "ListDerpDevImagesVariables")]
pub struct ListDerpDevImages {
    #[cynic(rename = "listDerpDevImages")]
    pub list_derp_dev_images: ListDerpDevImagesResult,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct ListDerpDevImagesOutput {
    pub images: Vec<ImageTag>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct ImageTag {
    pub image: String,
    pub repository: String,
    pub tag: String,
}

#[derive(cynic::InlineFragments, Debug)]
pub enum ListDerpDevImagesResult {
    ListDerpDevImagesOutput(ListDerpDevImagesOutput),
    UserFacingError(UserFacingError),
    #[cynic(fallback)]
    Unknown,
}

crate::client::define_operation! {
    ListDerpDevImages(ListDerpDevImagesVariables) -> ListDerpDevImages;
}
