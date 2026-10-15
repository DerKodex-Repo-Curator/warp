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

#[test]
fn requires_the_exact_expected_source_file_when_configured() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let source = std::env::temp_dir().join(format!("warp-profile-source-{unique}.rs"));
    let other = std::env::temp_dir().join(format!("warp-profile-other-{unique}.rs"));
    fs::write(&source, []).unwrap();
    fs::write(&other, []).unwrap();
    let selected = vec![
        OsString::from("--crate-name=warp"),
        OsString::from("--test"),
        source.as_os_str().to_owned(),
    ];
    let wrong = vec![
        OsString::from("--crate-name=warp"),
        OsString::from("--test"),
        other.as_os_str().to_owned(),
    ];
    assert!(target_matches(&selected, Some(&source)).unwrap());
    assert!(!target_matches(&wrong, Some(&source)).unwrap());
    fs::remove_file(source).unwrap();
    fs::remove_file(other).unwrap();
}

#[test]
fn requires_unambiguous_inventory_output_scope() {
    assert_eq!(
        option_value(&arguments(&["--out-dir", "a directory"]), "--out-dir").unwrap(),
        OsString::from("a directory")
    );
    assert!(option_value(&arguments(&["--out-dir=a", "--out-dir=b"]), "--out-dir").is_err());
    assert_eq!(
        ir_filestem(&arguments(&["-C", "extra-filename=-abcdef0123"])).unwrap(),
        "warp-abcdef0123"
    );
    assert!(ir_filestem(&arguments(&["-Cextra-filename=-../other"])).is_err());
    assert!(ir_filestem(&arguments(&["-Cextra-filename=-a", "-Cextra-filename=-b"])).is_err());
}

#[test]
fn preserves_non_incremental_codegen_units_for_ir_output() {
    assert!(inventory_needs_explicit_cgus(&arguments(&["-Cdebuginfo=line-tables-only"])).unwrap());
    for values in [
        vec!["-C", "codegen-units=16"],
        vec!["-Ccodegen-units=16"],
        vec!["--codegen", "codegen-units=16"],
        vec!["--codegen=codegen-units=16"],
    ] {
        assert!(!inventory_needs_explicit_cgus(&arguments(&values)).unwrap());
    }
    for values in [
        vec!["-Ccodegen-units=1"],
        vec!["-Ccodegen-units=256"],
        vec!["-Ccodegen-units=invalid"],
        vec!["-Ccodegen-units=16", "-Ccodegen-units=16"],
        vec!["-Cincremental=private-directory"],
        vec!["--codegen", "incremental=private-directory"],
    ] {
        assert!(inventory_needs_explicit_cgus(&arguments(&values)).is_err());
    }
}

#[test]
fn owns_only_the_exact_flat_ir_or_codegen_unit_files() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory = std::env::temp_dir().join(format!("warp-ir-ownership-{unique}"));
    fs::create_dir(&directory).unwrap();
    for name in [
        "warp-owned.ll",
        "warp-owned.warp.hash-cgu.00.rcgu.ll",
        "warp-owned-other.ll",
        "warp-owned.private.ll",
        "other-crate.rcgu.ll",
    ] {
        fs::write(directory.join(name), []).unwrap();
    }
    assert_eq!(
        owned_ir_files(&directory, "warp-owned").unwrap(),
        vec![
            directory.join("warp-owned.ll"),
            directory.join("warp-owned.warp.hash-cgu.00.rcgu.ll"),
        ]
    );
    fs::remove_dir_all(directory).unwrap();
}

#[test]
#[ignore = "requires a runnable Rust 1.92.0 compiler via IR_INVENTORY_TEST_RUSTC"]
fn implicit_cgu_capture_emits_all_units_and_keeps_linking() {
    let compiler = std::env::var_os("IR_INVENTORY_TEST_RUSTC").unwrap();
    let version = Command::new(&compiler).arg("--version").output().unwrap();
    assert!(version.status.success());
    assert!(String::from_utf8_lossy(&version.stdout).starts_with("rustc 1.92.0 "));
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory = std::env::temp_dir().join(format!("warp-ir-real-cgu-{unique}"));
    let output = directory.join("output");
    fs::create_dir_all(&output).unwrap();
    let source = directory.join("fixture.rs");
    let mut contents = (0..64)
        .map(|i| format!("#[inline(never)] pub fn f{i}(x: u64) -> u64 {{ x.wrapping_add({i}) }}\n"))
        .collect::<String>();
    contents.push_str("#[test] fn works() { assert_eq!(f2(7), 9); }\n");
    fs::write(&source, contents).unwrap();
    let arguments = vec![
        OsString::from("--crate-name=warp"),
        OsString::from("--test"),
        source.into_os_string(),
        OsString::from("--edition=2024"),
        OsString::from("--emit=dep-info,link"),
        OsString::from("-Cextra-filename=-fixture"),
        OsString::from("--out-dir"),
        output.clone().into_os_string(),
    ];
    assert_eq!(
        inventory_compiler(
            &compiler,
            &arguments,
            &arguments,
            &directory.join("capture")
        )
        .unwrap(),
        0
    );
    let files = owned_ir_files(&output, "warp-fixture").unwrap();
    assert_eq!(files.len(), 16);
    assert!(
        files
            .iter()
            .all(|file| file.to_str().unwrap().ends_with(".rcgu.ll"))
    );
    assert!(output.join("warp-fixture.d").is_file());
    let executable = output.join(if cfg!(windows) {
        "warp-fixture.exe"
    } else {
        "warp-fixture"
    });
    assert!(Command::new(executable).output().unwrap().status.success());
    fs::remove_dir_all(directory).unwrap();
}
