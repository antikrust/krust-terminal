//! Pictures of the images a message was sent with: a small one at the top right of the message and a
//! larger one under its text. They are drawn from the image data the message already carries, so
//! they are for the eye only: nothing here goes to a model, and a model's context is unchanged.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use base64::Engine as _;
use bytes::Bytes;
use krustui::assets::asset_cache::{AssetSource, AsyncAssetId, AsyncAssetType};

use crate::ai::agent::{AIAgentContext, AIAgentInput};

/// Namespace of these pictures in the asset cache.
struct SubmittedImageAsset;
impl AsyncAssetType for SubmittedImageAsset {}

/// The sources already made, by exchange: a block draws again whenever its reply streams in, and
/// the image data is large, so it is wrapped once and reused.
fn made() -> &'static Mutex<HashMap<String, Vec<AssetSource>>> {
    static MADE: OnceLock<Mutex<HashMap<String, Vec<AssetSource>>>> = OnceLock::new();
    MADE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// A source that decodes one image's base64 data when the picture is first drawn.
fn source_for(exchange_id: &str, index: usize, data: &str) -> AssetSource {
    let data: Arc<str> = Arc::from(data);
    AssetSource::Async {
        id: AsyncAssetId::new::<SubmittedImageAsset>(format!("{exchange_id}:{index}")),
        fetch: Arc::new(move || {
            let data = data.clone();
            Box::pin(async move {
                let bytes = base64::engine::general_purpose::STANDARD.decode(data.as_bytes())?;
                Ok(Bytes::from(bytes))
            })
        }),
    }
}

/// The pictures for the images in `inputs`, in the order the lightbox numbers them.
pub(super) fn sources(exchange_id: &str, inputs: &[AIAgentInput]) -> Vec<AssetSource> {
    let images: Vec<&str> = inputs
        .iter()
        .filter_map(|input| input.context())
        .flat_map(|contexts| contexts.iter())
        .filter_map(|context| match context {
            AIAgentContext::Image(image) => Some(image.data.as_str()),
            _ => None,
        })
        .collect();
    if images.is_empty() {
        return Vec::new();
    }
    let Ok(mut made) = made().lock() else {
        return images
            .iter()
            .enumerate()
            .map(|(index, data)| source_for(exchange_id, index, data))
            .collect();
    };
    made.entry(exchange_id.to_string())
        .or_insert_with(|| {
            images
                .iter()
                .enumerate()
                .map(|(index, data)| source_for(exchange_id, index, data))
                .collect()
        })
        .clone()
}
