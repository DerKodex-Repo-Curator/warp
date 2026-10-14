use super::*;

fn arguments(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}

#[test]
fn selects_only_the_app_library_test_target() {
    assert!(is_warp_libtest(&arguments(&[
        "--crate-name",
        "warp",
        "--test",
        "app/src/lib.rs",
    ])));
    assert!(is_warp_libtest(&arguments(&[
        "--crate-name=warp",
        "--test",
        r"C:\runner path\warp\app\src\lib.rs",
    ])));
    assert!(is_warp_libtest(&arguments(&[
        "--crate-name",
        "warp",
        "--test",
        "/workspace/warp/app/src/lib.rs",
    ])));
    assert!(!is_warp_libtest(&arguments(&[
        "--crate-name",
        "warp",
        "app/src/lib.rs",
    ])));
    assert!(!is_warp_libtest(&arguments(&[
        "--crate-name",
        "warp_tui",
        "--test",
        "app/src/lib.rs",
    ])));
    assert!(!is_warp_libtest(&arguments(&[
        "--crate-name",
        "warp",
        "--test",
        "app/src/bin/local.rs",
    ])));
}

#[test]
fn rejects_ambiguous_names_and_source_paths_embedded_in_flags() {
    assert!(!is_warp_libtest(&arguments(&[
        "--crate-name",
        "warp",
        "--crate-name=other",
        "--test",
        "app/src/lib.rs",
    ])));
    assert!(!is_warp_libtest(&arguments(&[
        "--crate-name",
        "warp",
        "--test",
        "--extern",
        "other=/workspace/app/src/lib.rs",
    ])));
    assert!(!is_warp_libtest(&arguments(&["-vV"])));
}

#[test]
fn retains_only_bounded_time_pass_json_records() {
    assert_eq!(
        time_pass_record(b"time: {\"pass\":\"total\",\"time\":1.5}\r\n"),
        Some(b"{\"pass\":\"total\",\"time\":1.5}".as_slice())
    );
    assert_eq!(time_pass_record(b"time: plain text\n"), None);
    assert_eq!(
        time_pass_record(b"{\"$message_type\":\"diagnostic\",\"message\":\"not captured\"}\n"),
        None
    );
    assert_eq!(time_pass_record(b"other stderr output\n"), None);
    let oversized = [
        b"time: {\"pass\":\"".as_slice(),
        &vec![b'x'; 16_385],
        b"\"}\n",
    ]
    .concat();
    assert_eq!(time_pass_record(&oversized), None);
}
