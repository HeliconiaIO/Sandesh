//! POP3 command parser: never panics.
//!
//! The body lives in `sandesh_fuzz::check_pop3` so a stable toolchain typechecks it
//! and `tests/corpus.rs` can replay a fixed corpus through the same code.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    sandesh_fuzz::check_pop3(data);
});
