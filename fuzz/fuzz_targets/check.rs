#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    if let Some(src) = byld_fuzz::text(data) {
        byld_fuzz::check(&src);
    }
});
