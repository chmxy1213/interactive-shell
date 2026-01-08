use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "action")] // Use tagged enum for better JSON: {"action": "StartSession", ...}
pub enum AgentRequest {
    StartSession {
        user: Option<String>,
    },
    ExecCommand {
        session_id: String,
        command: String,
        timeout_ms: u64,
    },
    CloseSession {
        session_id: String,
    },
}

#[derive(Serialize, Deserialize, Debug)]
pub struct AgentResponse {
    pub success: bool,
    pub session_id: Option<String>,
    pub output: String,
    pub exit_code: Option<i32>,
    pub error: Option<String>,
}
