// CI-only tooling uses native processes and wall-clock timing, not application abstractions.
#![allow(clippy::disallowed_types)]

use std::borrow::Cow;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

fn is_app_library(argument: &OsStr) -> bool {
    let normalized = argument.to_string_lossy().replace('\\', "/");
    let normalized = normalized.strip_prefix("./").unwrap_or(&normalized);
    let bytes = normalized.as_bytes();
    let absolute = normalized.starts_with('/')
        || (bytes.len() > 2
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && bytes[2] == b'/');
    normalized == "app/src/lib.rs" || (absolute && normalized.ends_with("/app/src/lib.rs"))
}

fn is_warp_test_target(arguments: &[OsString]) -> bool {
    let mut names = arguments
        .iter()
        .enumerate()
        .filter_map(|(index, argument)| {
            if argument == "--crate-name" {
                arguments.get(index + 1).map(OsString::as_os_str)
            } else {
                argument
                    .to_str()
                    .and_then(|value| value.strip_prefix("--crate-name="))
                    .map(OsStr::new)
            }
        });
    matches!((names.next(), names.next()), (Some(name), None) if name == "warp")
        && arguments.iter().any(|argument| argument == "--test")
}

fn is_warp_libtest(arguments: &[OsString]) -> bool {
    is_warp_test_target(arguments) && arguments.iter().any(|argument| is_app_library(argument))
}

fn target_matches(arguments: &[OsString], expected_source: Option<&Path>) -> io::Result<bool> {
    let Some(expected_source) = expected_source else {
        return Ok(is_warp_libtest(arguments));
    };
    if !is_warp_test_target(arguments) {
        return Ok(false);
    }
    let expected = expected_source.canonicalize()?;
    Ok(arguments.iter().any(|argument| {
        let file = Path::new(argument);
        file.extension() == Some(OsStr::new("rs"))
            && file.canonicalize().is_ok_and(|path| path == expected)
    }))
}

fn selection_arguments(arguments: &[OsString]) -> io::Result<Cow<'_, [OsString]>> {
    // Cargo's Windows command-length fallback keeps rustc's arguments in a temporary UTF-8 file.
    if let [argument] = arguments
        && let Some(file) = argument.to_str().and_then(|value| value.strip_prefix('@'))
    {
        if fs::metadata(file)?.len() > 4 * 1024 * 1024 {
            return Err(io::Error::other(
                "Compiler argument file exceeds the profiling selection limit.",
            ));
        }
        let contents = fs::read_to_string(file)?;
        return Ok(Cow::Owned(contents.lines().map(OsString::from).collect()));
    }
    Ok(Cow::Borrowed(arguments))
}

fn time_pass_record(line: &[u8]) -> Option<&[u8]> {
    let record = line.strip_prefix(b"time: ")?;
    let record = record.strip_suffix(b"\n").unwrap_or(record);
    let record = record.strip_suffix(b"\r").unwrap_or(record);
    (record.starts_with(b"{\"pass\":") && record.ends_with(b"}") && record.len() <= 16_384)
        .then_some(record)
}

fn epoch_milliseconds() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn option_value(arguments: &[OsString], option: &str) -> io::Result<OsString> {
    let prefix = format!("{option}=");
    let mut values = arguments.iter().enumerate().filter_map(|(index, value)| {
        if value == option {
            arguments.get(index + 1).cloned()
        } else {
            value
                .to_str()
                .and_then(|value| value.strip_prefix(&prefix))
                .map(OsString::from)
        }
    });
    match (values.next(), values.next()) {
        (Some(value), None) => Ok(value),
        _ => Err(io::Error::other(
            "Missing or ambiguous compiler output directory.",
        )),
    }
}

fn ir_filestem(arguments: &[OsString]) -> io::Result<String> {
    let mut extra = None;
    for (index, argument) in arguments.iter().enumerate() {
        let value = if argument == "-C" {
            arguments.get(index + 1).and_then(|value| value.to_str())
        } else {
            argument.to_str().and_then(|value| value.strip_prefix("-C"))
        };
        if let Some(value) = value.and_then(|value| value.strip_prefix("extra-filename=")) {
            if extra.is_some()
                || !value.starts_with('-')
                || !value
                    .chars()
                    .all(|value| value.is_ascii_alphanumeric() || value == '-')
            {
                return Err(io::Error::other("Unexpected compiler filename suffix."));
            }
            extra = Some(value);
        }
    }
    let suffix =
        extra.ok_or_else(|| io::Error::other("Missing Cargo compiler filename suffix."))?;
    Ok(format!("warp{suffix}"))
}

fn owned_ir_files(directory: &Path, filestem: &str) -> io::Result<Vec<std::path::PathBuf>> {
    let prefix = format!("{filestem}.");
    let single = format!("{filestem}.ll");
    let mut files = Vec::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let name = entry.file_name();
        if name.to_str().is_some_and(|name| {
            name == single || (name.starts_with(&prefix) && name.ends_with(".rcgu.ll"))
        }) && entry.file_type()?.is_file()
        {
            files.push(entry.path());
        }
    }
    files.sort();
    Ok(files)
}

fn inventory_needs_explicit_cgus(arguments: &[OsString]) -> io::Result<bool> {
    let mut codegen_units = None;
    for (index, argument) in arguments.iter().enumerate() {
        let value = if argument == "-C" || argument == "--codegen" {
            arguments.get(index + 1).and_then(|value| value.to_str())
        } else {
            argument.to_str().and_then(|value| {
                value
                    .strip_prefix("-C")
                    .or_else(|| value.strip_prefix("--codegen="))
            })
        };
        let Some(value) = value else {
            continue;
        };
        if value == "incremental" || value.starts_with("incremental=") {
            return Err(io::Error::other(
                "IR inventory requires non-incremental compilation.",
            ));
        }
        if let Some(value) = value.strip_prefix("codegen-units=") {
            if codegen_units.is_some() || value.parse::<usize>() != Ok(16) {
                return Err(io::Error::other(
                    "IR inventory requires exactly 16 codegen units.",
                ));
            }
            codegen_units = Some(16);
        }
    }
    Ok(codegen_units.is_none())
}

fn inventory_compiler(
    compiler: &OsStr,
    arguments: &[OsString],
    selection: &[OsString],
    directory: &Path,
) -> io::Result<i32> {
    if directory.join("warp-libtest.started").exists() {
        let _ = fs::write(directory.join("duplicate-match"), []);
        return Err(io::Error::other(
            "More than one libtest matched IR collection.",
        ));
    }
    if selection.iter().any(|argument| {
        argument
            .to_str()
            .is_some_and(|value| value.starts_with("-o"))
    }) {
        return Err(io::Error::other(
            "Explicit output filenames would change codegen-unit behavior.",
        ));
    }
    let codegen_units_was_implicit = inventory_needs_explicit_cgus(selection)?;
    let output = Path::new(&option_value(selection, "--out-dir")?).canonicalize()?;
    let filestem = ir_filestem(selection)?;
    if !owned_ir_files(&output, &filestem)?.is_empty() {
        return Err(io::Error::other(
            "Existing root IR files prevent exclusive ownership.",
        ));
    }
    let output_text = output
        .to_str()
        .filter(|value| !value.contains(['\r', '\n']))
        .ok_or_else(|| io::Error::other("Unsupported compiler output path."))?;
    fs::create_dir_all(directory)?;
    let mut marker = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(directory.join("warp-libtest.started"))?;
    let started = epoch_milliseconds();
    write!(marker, "{started}")?;
    // This is execution-only ownership data, never part of the published artifact.
    fs::write(
        directory.join("ir-owner.txt"),
        format!("warp-ir-owner-v1\n{output_text}\n{filestem}\n"),
    )?;
    let clock = Instant::now();
    let mut command = Command::new(compiler);
    command.args(arguments);
    if codegen_units_was_implicit {
        // LLVM IR output otherwise silently replaces rustc's non-incremental default with one CGU.
        command.arg("-Ccodegen-units=16");
    }
    let mut child = command
        .arg("--emit=llvm-ir")
        .env_remove("RUSTC_BOOTSTRAP")
        .spawn()?;
    let pid = child.id();
    let code = child.wait()?.code().unwrap_or(1);
    let elapsed = clock.elapsed().as_secs_f64();
    let finished = epoch_milliseconds();
    let files = owned_ir_files(&output, &filestem)
        .map(|files| files.len())
        .ok();
    let file_count = files.map_or_else(|| "null".to_owned(), |count| count.to_string());
    let metadata = format!(
        "{{\"crate\":\"warp\",\"source\":\"app/src/lib.rs\",\"libtest\":true,\
         \"mode\":\"ir-inventory\",\"pid\":{pid},\"started_epoch_ms\":{started},\
         \"finished_epoch_ms\":{finished},\"elapsed_seconds\":{elapsed},\
         \"compiler_exit_code\":{code},\"ir_file_count\":{file_count},\
         \"codegen_units\":16,\"codegen_units_was_implicit\":{codegen_units_was_implicit},\
         \"incremental\":false,\
         \"bootstrap_scope\":\"none\",\"existing_outputs_preserved\":true}}\n"
    );
    if fs::write(directory.join("invocation.json"), metadata).is_err() {
        eprintln!("Could not save IR inventory metadata.");
    }
    Ok(code)
}
fn profile_compiler(compiler: &OsStr, arguments: &[OsString], directory: &Path) -> io::Result<i32> {
    fs::create_dir_all(directory.join("raw"))?;
    let started = epoch_milliseconds();
    let marker = directory.join("warp-libtest.started");
    let mut marker_file = match OpenOptions::new().write(true).create_new(true).open(marker) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let _ = fs::write(directory.join("duplicate-match"), []);
            return Err(io::Error::other(
                "More than one warp libtest invocation matched the profiling scope.",
            ));
        }
        Err(error) => return Err(error),
    };
    write!(marker_file, "{started}")?;
    let mut timings = File::create(directory.join("time-passes.jsonl"))?;
    let mut profile_argument = OsString::from("-Zself-profile=");
    profile_argument.push(directory.join("raw"));

    let clock = Instant::now();
    let mut child = Command::new(compiler)
        .args(arguments)
        .args([
            profile_argument,
            OsString::from("-Zself-profile-events=default"),
            OsString::from("-Ztime-passes"),
            OsString::from("-Ztime-passes-format=json"),
        ])
        // Only this selected compiler child receives the approved unstable-diagnostics override.
        .env("RUSTC_BOOTSTRAP", "1")
        .stderr(Stdio::piped())
        .spawn()?;
    let pid = child.id();
    let mut captured = true;
    {
        let mut reader = BufReader::new(child.stderr.take().expect("piped compiler stderr"));
        let mut stderr = io::stderr().lock();
        let mut line = Vec::new();
        loop {
            line.clear();
            match reader.read_until(b'\n', &mut line) {
                Ok(0) => break,
                Ok(_) => {
                    if stderr.write_all(&line).is_err() {
                        captured = false;
                    }
                    if let Some(record) = time_pass_record(&line)
                        && (timings.write_all(record).is_err() || timings.write_all(b"\n").is_err())
                    {
                        captured = false;
                    }
                }
                Err(_) => {
                    captured = false;
                    break;
                }
            }
        }
    }
    let status = child.wait()?;
    let code = status.code().unwrap_or(1);
    let elapsed = clock.elapsed().as_secs_f64();
    let finished = epoch_milliseconds();
    let metadata = format!(
        "{{\"crate\":\"warp\",\"source\":\"app/src/lib.rs\",\"libtest\":true,\"pid\":{pid},\
         \"started_epoch_ms\":{started},\"finished_epoch_ms\":{finished},\
         \"elapsed_seconds\":{elapsed},\"compiler_exit_code\":{code},\
         \"timing_capture_succeeded\":{captured},\
         \"events\":\"default\",\"counter\":\"wall-time\",\
         \"bootstrap_scope\":\"selected-compiler-child\"}}\n"
    );
    if fs::write(directory.join("invocation.json"), metadata).is_err() {
        eprintln!("Could not save compiler profiling metadata.");
    }
    // Retain the full Windows compiler exit code; capture validation is a separate workflow step.
    Ok(code)
}

fn run() -> io::Result<i32> {
    let mut arguments = std::env::args_os().skip(1);
    let compiler = arguments
        .next()
        .ok_or_else(|| io::Error::other("Missing compiler executable."))?;
    let arguments: Vec<_> = arguments.collect();
    let selection = selection_arguments(&arguments)?;
    let expected_source = std::env::var_os("WARP_CI_RUSTC_SOURCE");
    if !target_matches(&selection, expected_source.as_deref().map(Path::new))? {
        return Ok(Command::new(compiler)
            .args(arguments)
            .status()?
            .code()
            .unwrap_or(1));
    }
    let directory = std::env::var_os("WARP_CI_RUSTC_PROFILE_DIR")
        .ok_or_else(|| io::Error::other("Missing compiler profiling directory."))?;
    match std::env::var("WARP_CI_DIAGNOSTIC_MODE").as_deref() {
        Ok("ir-inventory") => {
            inventory_compiler(&compiler, &arguments, &selection, Path::new(&directory))
        }
        Ok("profile") | Err(_) => profile_compiler(&compiler, &arguments, Path::new(&directory)),
        _ => Err(io::Error::other("Unsupported compiler diagnostic mode.")),
    }
}

fn main() {
    let code = run().unwrap_or_else(|error| {
        eprintln!("Compiler profiling wrapper failed: {error}");
        1
    });
    std::process::exit(code);
}

#[cfg(test)]
#[path = "rustc_profile_wrapper_tests.rs"]
mod tests;
