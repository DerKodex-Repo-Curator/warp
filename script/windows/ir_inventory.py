"""Summary-only inventory for one compiler-owned set of LLVM IR files."""

from collections import defaultdict
from pathlib import Path
import json
import re
import subprocess
import sys

MAX_LINE = 4 * 1024 * 1024
MAX_HEADER = 1024 * 1024
MAX_IR_BYTES = 32 * 1024**3
MAX_SYMBOLS = 2_000_000
MAX_ROWS = 200
OPCODES = set(
    """
ret br switch indirectbr invoke callbr resume catchswitch catchret cleanupret unreachable
fneg add fadd sub fsub mul fmul udiv sdiv fdiv urem srem frem
shl lshr ashr and or xor extractelement insertelement shufflevector
extractvalue insertvalue alloca load store fence cmpxchg atomicrmw getelementptr
trunc zext sext fptrunc fpext fptoui fptosi uitofp sitofp ptrtoint inttoptr
bitcast addrspacecast icmp fcmp phi select freeze call va_arg landingpad catchpad cleanuppad
""".split()
)
ASSIGNMENT = re.compile(r'^%(?:"(?:[^"\\]|\\.)*"|[-a-zA-Z$._0-9]+)\s*=\s*')
LABEL = re.compile(r'^(?:"(?:[^"\\]|\\.)*"|[-a-zA-Z$._0-9]+):(?:\s|$)')
OMITTED = "<non-rust-or-omitted>"


class InventoryError(Exception):
    """Carries only a fixed error code, never source text or a private symbol."""


def bounded_lines(stream):
    while True:
        line = stream.readline(MAX_LINE + 1)
        if not line:
            return
        oversized = len(line) > MAX_LINE
        if oversized:
            while line and not line.endswith(b"\n"):
                line = stream.readline(MAX_LINE + 1)
            yield None
        else:
            yield line.decode("utf-8")


def symbol_from_header(header):
    quoted = False
    escaped = False
    position = None
    for index, character in enumerate(header):
        if escaped:
            escaped = False
        elif character == "\\" and quoted:
            escaped = True
        elif character == '"':
            quoted = not quoted
        elif character == "@" and not quoted:
            position = index + 1
            break
    if position is None:
        raise InventoryError("invalid-function-header")
    if header[position] != '"':
        end = header.find("(", position)
        if end < 0:
            raise InventoryError("invalid-function-header")
        return header[position:end].strip()
    output = bytearray()
    position += 1
    while position < len(header):
        character = header[position]
        if character == '"':
            if not header[position + 1 :].lstrip().startswith("("):
                raise InventoryError("invalid-function-header")
            return output.decode("utf-8")
        if character == "\\":
            try:
                output.append(int(header[position + 1 : position + 3], 16))
            except ValueError as error:
                raise InventoryError("invalid-symbol-escape") from error
            position += 3
        else:
            output.extend(character.encode())
            position += 1
    raise InventoryError("invalid-function-header")


def bracket_delta(line):
    delta = 0
    quoted = False
    escaped = False
    for character in line:
        if escaped:
            escaped = False
        elif quoted and character == "\\":
            escaped = True
        elif character == '"':
            quoted = not quoted
        elif not quoted:
            if character == ";":
                break
            delta += (character == "[") - (character == "]")
    return delta


def parse_file(path, functions):
    header = ""
    symbol = None
    instructions = lines = nesting = 0
    definitions = 0
    with path.open("rb") as stream:
        for line in bounded_lines(stream):
            if line is None:
                if symbol is not None or header:
                    raise InventoryError("oversized-function-line")
                continue
            stripped = line.strip()
            if symbol is None:
                if not header and not line.startswith("define "):
                    continue
                header += line
                if len(header) > MAX_HEADER:
                    raise InventoryError("oversized-function-header")
                if not stripped.endswith("{"):
                    continue
                symbol = symbol_from_header(header)
                header = ""
                instructions = lines = nesting = 0
                continue
            if stripped == "}":
                if nesting != 0:
                    raise InventoryError("unclosed-instruction")
                stats = functions[symbol]
                stats["definitions"] += 1
                stats["instruction_records"] += instructions
                stats["body_lines"] += lines
                stats["largest_definition_instruction_records"] = max(
                    stats["largest_definition_instruction_records"], instructions
                )
                definitions += 1
                if len(functions) > MAX_SYMBOLS:
                    raise InventoryError("symbol-count-limit")
                symbol = None
                continue
            if not stripped or stripped.startswith((";", "!", "#dbg_")) or LABEL.match(stripped):
                continue
            lines += 1
            if nesting > 0:
                nesting += bracket_delta(stripped)
                if nesting < 0:
                    raise InventoryError("invalid-instruction-continuation")
                continue
            operation = ASSIGNMENT.sub("", stripped, count=1)
            token = operation.split(None, 1)[0].rstrip(",")
            if token in ("tail", "musttail", "notail"):
                remainder = operation.split(None, 1)
                if len(remainder) != 2:
                    raise InventoryError("invalid-call-prefix")
                token = remainder[1].split(None, 1)[0].rstrip(",")
            if token in ("to", "unwind", "cleanup", "catch", "filter"):
                continue
            if token not in OPCODES:
                raise InventoryError("unsupported-instruction-record")
            instructions += 1
            nesting = bracket_delta(stripped)
            if nesting < 0:
                raise InventoryError("invalid-instruction-continuation")
    if symbol is not None or header:
        raise InventoryError("unterminated-function")
    return definitions


def fresh_stats():
    return {
        "definitions": 0,
        "instruction_records": 0,
        "body_lines": 0,
        "largest_definition_instruction_records": 0,
    }


def normalized_groups(functions, normalizer):
    groups = {}
    symbols = list(functions)
    for begin in range(0, len(symbols), 4096):
        batch = symbols[begin : begin + 4096]
        eligible = [
            s
            if s.startswith(("_ZN", "_R", "__ZN", "__R")) and not any(c in s for c in "\r\n\0")
            else ""
            for s in batch
        ]
        result = subprocess.run(
            [str(normalizer)],
            input="\n".join(eligible) + "\n",
            capture_output=True,
            text=True,
            timeout=120,
        )
        if result.returncode != 0:
            raise InventoryError("normalizer-failed")
        names = result.stdout.splitlines()
        if len(names) != len(batch):
            raise InventoryError("normalizer-record-count")
        for symbol, value in zip(batch, names):
            name = json.loads(value) or OMITTED
            if (
                not isinstance(name, str)
                or len(name.encode()) > 2048
                or any(c in name for c in "\"'\\/\r\n")
            ):
                raise InventoryError("unsafe-normalized-name")
            group = groups.setdefault(
                name, fresh_stats() | {"normalized_name": name, "distinct_symbols": 0}
            )
            stats = functions[symbol]
            group["distinct_symbols"] += name != OMITTED
            for field in ("definitions", "instruction_records", "body_lines"):
                group[field] += stats[field]
            group["largest_definition_instruction_records"] = max(
                group["largest_definition_instruction_records"],
                stats["largest_definition_instruction_records"],
            )
    if OMITTED in groups:
        groups[OMITTED]["distinct_symbols"] = None
    return list(groups.values())


def ownership_files(capture):
    owner = (capture / "ir-owner.txt").read_text().splitlines()
    if len(owner) != 3 or owner[0] != "warp-ir-owner-v1":
        raise InventoryError("invalid-owner-record")
    directory = Path(owner[1]).resolve(strict=True)
    stem = owner[2]
    if not re.fullmatch(r"warp-[A-Za-z0-9-]+", stem):
        raise InventoryError("invalid-owner-filestem")
    files = sorted(
        p
        for p in directory.iterdir()
        if (
            p.name == stem + ".ll"
            or (p.name.startswith(stem + ".") and p.name.endswith(".rcgu.ll"))
        )
        and p.is_file()
        and not p.is_symlink()
    )
    return files


def collect(capture, artifact, normalizer, expected_codegen_units=16):
    if artifact.exists():
        raise InventoryError("artifact-directory-exists")
    artifact.mkdir(parents=True)
    files = []
    complete = False
    issue = None
    groups = []
    total_bytes = definitions = 0
    invocation = {}
    try:
        files = ownership_files(capture)
        invocation = json.loads((capture / "invocation.json").read_text())
        if (
            invocation.get("crate") != "warp"
            or invocation.get("source") != "app/src/lib.rs"
            or not invocation.get("libtest")
            or invocation.get("mode") != "ir-inventory"
            or invocation.get("compiler_exit_code") != 0
            or invocation.get("bootstrap_scope") != "none"
            or invocation.get("codegen_units") != expected_codegen_units
            or invocation.get("incremental") is not False
            or not invocation.get("existing_outputs_preserved")
            or not (capture / "warp-libtest.started").exists()
            or (capture / "duplicate-match").exists()
        ):
            raise InventoryError("invalid-invocation")
        if not files or len(files) != invocation.get("ir_file_count"):
            raise InventoryError("missing-or-changed-ir-files")
        indices = {
            int(match.group(1))
            for path in files
            if (match := re.search(r"-cgu\.(\d+)\.rcgu\.ll$", path.name))
        }
        if len(files) != expected_codegen_units or indices != set(range(expected_codegen_units)):
            raise InventoryError("unexpected-codegen-unit-set")
        total_bytes = sum(p.stat().st_size for p in files)
        if total_bytes > MAX_IR_BYTES:
            raise InventoryError("raw-ir-size-limit")
        functions = defaultdict(fresh_stats)
        for path in files:
            definitions += parse_file(path, functions)
        if definitions == 0:
            raise InventoryError("no-function-definitions")
        groups = normalized_groups(functions, normalizer)
        complete = True
    except InventoryError as error:
        issue = str(error)
    except Exception:
        issue = "inventory-read-or-parse-failed"
    finally:
        cleanup_failed = False
        for path in files:
            try:
                path.unlink()
            except OSError:
                cleanup_failed = True
        if cleanup_failed:
            complete = False
            issue = "owned-ir-cleanup-failed"
    summary = {
        "complete": complete,
        "issue": issue,
        "raw_ir_published": False,
        "owned_ir_deleted": not any(p.exists() for p in files),
        "ir_files": len(files),
        "raw_ir_bytes_processed": total_bytes,
        "expected_codegen_units": expected_codegen_units,
        "definition_copies": definitions,
        "normalized_families": len(groups),
        "instruction_records": sum(g["instruction_records"] for g in groups),
        "physical_body_lines": sum(g["body_lines"] for g in groups),
        "metrics": "LLVM instruction-start records and physical body lines; not CPU time or native code size",
        "normalization": "Rust names only; legacy hashes and concrete generic/array contents omitted",
        "compiler_scope": {
            key: invocation.get(key)
            for key in (
                "crate",
                "source",
                "libtest",
                "mode",
                "pid",
                "compiler_exit_code",
                "elapsed_seconds",
                "bootstrap_scope",
                "existing_outputs_preserved",
                "codegen_units",
                "codegen_units_was_implicit",
                "incremental",
            )
        },
        "top_by_instruction_records": sorted(
            groups, key=lambda g: g["instruction_records"], reverse=True
        )[:MAX_ROWS],
        "top_by_definition_copies": sorted(groups, key=lambda g: g["definitions"], reverse=True)[
            :MAX_ROWS
        ],
    }
    (artifact / "ir-inventory.json").write_text(json.dumps(summary, ensure_ascii=True, indent=2))
    return complete


def main():
    capture, artifact, normalizer = map(Path, sys.argv[1:])
    try:
        return 0 if collect(capture, artifact, normalizer) else 1
    except Exception:
        print("IR inventory failed before publication.", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
