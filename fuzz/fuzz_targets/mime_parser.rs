//! MIME parser: never panics; never loses bytes; terminates.
//!
//! The body lives in `sandesh_fuzz::check_mime` so a stable toolchain typechecks it
//! and `tests/corpus.rs` can replay a fixed corpus through the same code.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    sandesh_fuzz::check_mime(data);
});
