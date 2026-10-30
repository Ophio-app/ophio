// SPDX-License-Identifier: Apache-2.0
//! Opens a web address in the user's default browser, handed over whole: no
//! shell reads it, so `&` and the like stay part of the address.
#[cfg(windows)]
pub use crate::win::open_url::open_url;
