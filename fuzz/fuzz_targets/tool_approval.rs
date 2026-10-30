// SPDX-License-Identifier: Apache-2.0
//! What an agent asks permission for, shown to a person on the phone.
//! Any request reads into a bounded preview, and the raw request written for
//! inspection is JSON that reads back to itself. (A number with more digits
//! than an f64 holds is already rounded when the request is read, so the raw
//! text can differ from the agent's in those digits.)
#![no_main]
use libfuzzer_sys::fuzz_target;
use ports::tool_approval::{raw_request, tool_approval};

const TOOLS: [&str; 8] = [
    "Bash", "Edit", "Write", "Read", "WebFetch", "AskUserQuestion", "mcp__x__y", "Task",
];

fuzz_target!(|data: &[u8]| {
    let Some((&pick, json)) = data.split_first() else {
        return;
    };
    let Ok(input) = serde_json::from_slice::<serde_json::Value>(json) else {
        return;
    };
    let facts = tool_approval(TOOLS[usize::from(pick) % TOOLS.len()], &input);
    assert!(facts.title.len() <= 64 * 1024, "title {}", facts.title.len());
    let raw = raw_request(&input);
    let read: serde_json::Value = serde_json::from_str(&raw).expect("raw request is JSON");
    assert_eq!(raw_request(&read), raw);
});
