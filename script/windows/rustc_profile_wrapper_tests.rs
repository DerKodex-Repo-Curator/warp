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

#[test]
fn selects_from_a_cargo_response_file_without_rewriting_it() {
    let path = std::env::temp_dir().join(format!(
        "warp-compiler-args-{}-{}.txt",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let contents = "--crate-name\r\nwarp\r\n--test\r\nC:\\runner space\\warp\\app\\src\\lib.rs\r\n";
    fs::write(&path, contents).unwrap();
    let mut argument = OsString::from("@");
    argument.push(&path);
    let original = vec![argument];
    assert!(is_warp_libtest(&selection_arguments(&original).unwrap()));
    assert_eq!(fs::read_to_string(&path).unwrap(), contents);
    fs::remove_file(path).unwrap();
}
