// SPDX-License-Identifier: Apache-2.0
//! JSON frames on every connection, as both ends read them: the integer
//! bound first, then the typed frame. A frame that reads must write back to
//! the same value and stay within the bound.
#![no_main]
use libfuzzer_sys::fuzz_target;
use protocol::{
    ClientFrame, ServerFrame,
    media::{MediaClientFrame, MediaServerFrame, SpeechClientFrame, SpeechServerFrame},
    preview::{PreviewClientFrame, PreviewServerFrame},
    terminal::{TerminalClientFrame, TerminalServerFrame},
    transfer::{TransferClientFrame, TransferServerFrame},
};
use serde::{Serialize, de::DeserializeOwned};

fn round_trip<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(
    value: &serde_json::Value,
) {
    let Ok(frame) = serde_json::from_value::<T>(value.clone()) else {
        return;
    };
    let written = serde_json::to_value(&frame).expect("a read frame writes");
    protocol::integer::validate(&written).expect("a read frame stays within the bound");
    let again: T = serde_json::from_value(written).expect("a written frame reads");
    assert_eq!(again, frame);
}

fuzz_target!(|data: &[u8]| {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(data) else {
        return;
    };
    if protocol::integer::validate(&value).is_err() {
        return;
    }
    round_trip::<ClientFrame>(&value);
    round_trip::<ServerFrame>(&value);
    round_trip::<MediaClientFrame>(&value);
    round_trip::<MediaServerFrame>(&value);
    round_trip::<SpeechClientFrame>(&value);
    round_trip::<SpeechServerFrame>(&value);
    round_trip::<PreviewClientFrame>(&value);
    round_trip::<PreviewServerFrame>(&value);
    round_trip::<TerminalClientFrame>(&value);
    round_trip::<TerminalServerFrame>(&value);
    round_trip::<TransferClientFrame>(&value);
    round_trip::<TransferServerFrame>(&value);
});
