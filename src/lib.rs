use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
pub struct CommandRequest {
    pub command: String,
    pub timeout_ms: u64,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct CommandResponse {
    pub timed_out: bool,
    pub exit_code: Option<i32>, // None if killed or signal
    pub output: String,
}
