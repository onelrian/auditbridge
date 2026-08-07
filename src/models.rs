use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// Activity type constants from NetBird source code
// Source: https://github.com/netbirdio/netbird/blob/main/management/server/activity/codes.go
// Activity type constants were removed as the API provides string codes and names directly.

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub id: String,
    pub timestamp: String,
    pub activity: String,
    pub activity_code: String,
    pub initiator_id: Option<String>,
    pub initiator_email: Option<String>,
    pub initiator_name: Option<String>,
    pub target_id: Option<String>,
    pub account_id: Option<String>,
    pub meta: Option<HashMap<String, serde_json::Value>>,
}
