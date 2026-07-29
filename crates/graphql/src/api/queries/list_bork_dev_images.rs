use crate::error::UserFacingError;
use crate::schema;

#[derive(cynic::QueryVariables, Debug)]
pub struct ListBorkDevImagesVariables {}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "RootQuery", variables = "ListBorkDevImagesVariables")]
pub struct ListBorkDevImages {
    #[cynic(rename = "listBorkDevImages")]
    pub list_bork_dev_images: ListBorkDevImagesResult,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct ListBorkDevImagesOutput {
    pub images: Vec<ImageTag>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct ImageTag {
    pub image: String,
    pub repository: String,
    pub tag: String,
}

#[derive(cynic::InlineFragments, Debug)]
pub enum ListBorkDevImagesResult {
    ListBorkDevImagesOutput(ListBorkDevImagesOutput),
    UserFacingError(UserFacingError),
    #[cynic(fallback)]
    Unknown,
}

crate::client::define_operation! {
    ListBorkDevImages(ListBorkDevImagesVariables) -> ListBorkDevImages;
}
