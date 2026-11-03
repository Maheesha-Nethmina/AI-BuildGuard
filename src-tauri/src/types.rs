use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProductionBuildResult {
    pub success: bool,
    pub raw_output: String,
    pub duration_ms: u64,
}