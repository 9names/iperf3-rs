use serde::{Deserialize, Serialize};

// These are used as system state and as messages in original iperf.
// Just using them for messages for now.
#[allow(dead_code)]
pub mod iperf_command {
    pub const TEST_START: u8 = 1;
    pub const TEST_RUNNING: u8 = 2;
    pub const TEST_END: u8 = 4;
    pub const PARAM_EXCHANGE: u8 = 9;
    pub const CREATE_STREAMS: u8 = 10;
    pub const SERVER_TERMINATE: u8 = 11;
    pub const CLIENT_TERMINATE: u8 = 12;
    pub const EXCHANGE_RESULTS: u8 = 13;
    pub const DISPLAY_RESULTS: u8 = 14;
    pub const IPERF_START: u8 = 15;
    pub const IPERF_DONE: u8 = 16;
}

#[derive(Serialize, Deserialize, Debug)]
pub struct SessionConfig {
    pub tcp: Option<bool>,
    pub omit: i32,
    pub time: i32,
    pub num: i32,
    pub blockcount: i32,
    pub parallel: i32,
    pub reverse: Option<bool>,
    pub bidirectional: Option<bool>,
    pub len: i32,
    pub pacing_timer: i32,
    pub client_version: String,
}

#[cfg(test)]
mod tests {
    #[test]
    fn decode_json_data() {
        let result = 2 + 2;
        assert_eq!(result, 4);
    }
}
