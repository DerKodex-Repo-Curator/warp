// CI-only tooling uses native processes and wall-clock timing, not application abstractions.
#![allow(clippy::disallowed_types)]

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

fn is_warp_libtest(arguments: &[OsString]) -> bool {
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
        && arguments.iter().any(|argument| is_app_library(argument))
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
        "{{\"crate\":\"warp\",\"libtest\":true,\"pid\":{pid},\
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
    if !is_warp_libtest(&arguments) {
        return Ok(Command::new(compiler)
            .args(arguments)
            .status()?
            .code()
            .unwrap_or(1));
    }
    let directory = std::env::var_os("WARP_CI_RUSTC_PROFILE_DIR")
        .ok_or_else(|| io::Error::other("Missing compiler profiling directory."))?;
    profile_compiler(&compiler, &arguments, Path::new(&directory))
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
