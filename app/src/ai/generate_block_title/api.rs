//! Orphaned by the block-sharing removal. The warp.dev permalink / embed flow
//! was removed from the UI, leaving this client (and the block payload model
//! and title API it drives) with no live callers. Kept rather than torn out
//! because the vertical also feeds telemetry and block-height code; a full
//! teardown is tracked separately.
#![allow(dead_code)]

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct GenerateBlockTitleRequest {
    pub command: String,
    pub output: String,
}

#[derive(Serialize, Deserialize)]
pub struct GenerateBlockTitleResponse {
    pub title: String,
}
