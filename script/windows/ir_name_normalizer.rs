use std::io::{self, BufRead, Write};

fn normalized_name(symbol: &str) -> Option<String> {
    if symbol.len() > 65_536 {
        return None;
    }
    let demangled = rustc_demangle::try_demangle(symbol).ok()?;
    let name = format!("{demangled:#}");
    let mut output = String::new();
    let mut characters = name.chars().peekable();
    while let Some(character) = characters.next() {
        let generic = character == '<'
            && (output.ends_with("::")
                || output
                    .chars()
                    .last()
                    .is_some_and(|previous| previous.is_alphanumeric() || previous == '>'));
        if generic || character == '[' {
            let (open, close) = if generic { ('<', '>') } else { ('[', ']') };
            let mut depth = 1;
            let mut previous = '\0';
            for next in characters.by_ref() {
                if next == open {
                    depth += 1;
                } else if next == close && previous != '-' {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                previous = next;
            }
            if depth != 0 {
                return None;
            }
            output.push_str(if generic { "<_>" } else { "[_]" });
        } else {
            if character.is_control() || matches!(character, '"' | '\'' | '/' | '\\') {
                return None;
            }
            output.push(character);
        }
        if output.len() > 2048 {
            return None;
        }
    }
    (!output.is_empty()).then_some(output)
}

fn main() -> io::Result<()> {
    let stdin = io::stdin().lock();
    let mut stdout = io::stdout().lock();
    for line in stdin.lines() {
        let name = normalized_name(&line?);
        serde_json::to_writer(&mut stdout, &name)?;
        stdout.write_all(b"\n")?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "ir_name_normalizer_tests.rs"]
mod tests;
