use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct MailEdsSearchRequest {
    pub query: String,
    pub limit: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct MailEdsSearchResponse {
    pub ok: bool,
    pub message: String,
    pub results: Vec<MailEdsMessageSummary>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct MailEdsMessageSummary {
    pub message_id: String,
    pub folder_uri: String,
    pub subject: String,
    pub sender: String,
    pub sender_email: Option<String>,
    pub date_label: String,
    pub snippet: String,
    pub unread: bool,
    pub has_attachment: bool,
    pub openable: bool,
    pub replyable: bool,
    pub composable: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct MailEdsActionRequest {
    pub message_id: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct MailEdsActionResponse {
    pub ok: bool,
    pub message: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct MailEdsStatus {
    pub ok: bool,
    pub message: String,
}
