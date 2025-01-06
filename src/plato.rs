//! Helper functions for interacting with the Plato e-reader software.

use serde_json::json;

/// Show a notification on the device with the given `message`.
pub fn notify(message: &str) {
    let event = json!({
        "type": "notify",
        "message": message,
    });
    println!("{event}");
}

/// Set the device's Wi-Fi state to `enable`.
pub fn set_wifi(enable: bool) {
    let event = json!({
        "type": "setWifi",
        "enable": enable,
    });
    println!("{event}");
}
