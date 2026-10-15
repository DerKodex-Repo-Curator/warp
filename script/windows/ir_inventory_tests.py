from collections import defaultdict
from pathlib import Path
import importlib.util
import json
import os
import tempfile
import unittest

spec = importlib.util.spec_from_file_location(
    "ir_inventory", Path(__file__).with_name("ir_inventory.py")
)
inventory = importlib.util.module_from_spec(spec)
spec.loader.exec_module(inventory)

IR = r"""
@private = private constant [29 x i8] c"PRIVATE_FIXTURE_DO_NOT_PUBLISH"
declare i32 @callee()
define internal i32 @"_ZN4demo3run17h0123456789abcdefE"(
    i32 %arg
) {
entry:
  %n = add i32 %arg, 1
  switch i32 %n, label %default [
    i32 1, label %one
    i32 2, label %two
  ]
one:
  ret i32 1
two:
  %r = invoke i32 @callee()
          to label %normal unwind label %cleanup
normal:
  ret i32 %r
cleanup:
  %lp = landingpad { ptr, i32 }
          cleanup
  resume { ptr, i32 } %lp
default:
  #dbg_value(i32 %arg, !1, !DIExpression(), !2)
  ; PRIVATE_COMMENT_DO_NOT_PUBLISH
  ret i32 0
}
!1 = !DILocation(line: 1, scope: !2)
"""


class ParserTests(unittest.TestCase):
    def test_multiline_switch_invoke_and_exception_records(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "fixture.ll"
            path.write_text(IR)
            functions = defaultdict(inventory.fresh_stats)
            self.assertEqual(inventory.parse_file(path, functions), 1)
            self.assertEqual(
                functions["_ZN4demo3run17h0123456789abcdefE"]["instruction_records"], 8
            )
            self.assertEqual(functions["_ZN4demo3run17h0123456789abcdefE"]["body_lines"], 13)

    def test_repeated_definition_counts_are_not_deduplicated(self):
        with tempfile.TemporaryDirectory() as directory:
            first = Path(directory) / "first.ll"
            second = Path(directory) / "second.ll"
            first.write_text(IR)
            second.write_text(IR)
            functions = defaultdict(inventory.fresh_stats)
            inventory.parse_file(first, functions)
            inventory.parse_file(second, functions)
            self.assertEqual(len(functions), 1)
            self.assertEqual(functions["_ZN4demo3run17h0123456789abcdefE"]["definitions"], 2)
            self.assertEqual(
                functions["_ZN4demo3run17h0123456789abcdefE"]["instruction_records"], 16
            )

    def test_operand_free_instruction_with_metadata(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "fixture.ll"
            path.write_text(
                "define void @_ZN4demo3run17h0123456789abcdefE() {\nstart:\n  unreachable, !dbg !1\n}\n"
            )
            functions = defaultdict(inventory.fresh_stats)
            self.assertEqual(inventory.parse_file(path, functions), 1)
            self.assertEqual(
                functions["_ZN4demo3run17h0123456789abcdefE"]["instruction_records"], 1
            )

    def test_unsupported_instruction_fails_without_source_text(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "fixture.ll"
            path.write_text(IR.replace("%n = add", "%n = PRIVATE_UNSUPPORTED"))
            with self.assertRaisesRegex(
                inventory.InventoryError, "^unsupported-instruction-record$"
            ):
                inventory.parse_file(path, defaultdict(inventory.fresh_stats))

    def test_quoted_symbols_and_debug_text_are_not_confused_with_headers(self):
        self.assertEqual(
            inventory.symbol_from_header(
                'define %"return@type" @"_ZN4demo3run17h0123456789abcdefE"() {'
            ),
            "_ZN4demo3run17h0123456789abcdefE",
        )
        self.assertEqual(inventory.bracket_delta('call void asm "PRIVATE [ ; ]", ""()'), 0)


class PublicationTests(unittest.TestCase):
    def make_capture(self, directory, ir):
        root = Path(directory)
        output = root / "target"
        capture = root / "capture"
        output.mkdir()
        capture.mkdir()
        path = output / "warp-test.warp.hash-cgu.00.rcgu.ll"
        path.write_text(ir)
        unrelated = output / "other-crate.rcgu.ll"
        unrelated.write_text("UNRELATED_PRIVATE_FILE")
        (capture / "ir-owner.txt").write_text(f"warp-ir-owner-v1\n{output}\nwarp-test\n")
        (capture / "warp-libtest.started").write_text("1")
        (capture / "invocation.json").write_text(
            json.dumps(
                {
                    "crate": "warp",
                    "source": "app/src/lib.rs",
                    "libtest": True,
                    "mode": "ir-inventory",
                    "compiler_exit_code": 0,
                    "bootstrap_scope": "none",
                    "existing_outputs_preserved": True,
                    "ir_file_count": 1,
                    "codegen_units": 1,
                    "codegen_units_was_implicit": False,
                    "incremental": False,
                }
            )
        )
        return root, capture, path, unrelated

    def test_failed_parser_still_deletes_only_owned_ir(self):
        with tempfile.TemporaryDirectory() as directory:
            root, capture, path, unrelated = self.make_capture(
                directory, IR.replace("%n = add", "%n = PRIVATE_UNSUPPORTED")
            )
            self.assertFalse(
                inventory.collect(
                    capture,
                    root / "artifact",
                    Path("not-used"),
                    expected_codegen_units=1,
                )
            )
            report = (root / "artifact/ir-inventory.json").read_text()
            self.assertFalse(path.exists())
            self.assertTrue(unrelated.exists())
            self.assertNotIn("PRIVATE_UNSUPPORTED", report)
            self.assertNotIn("PRIVATE_FIXTURE", report)
            self.assertEqual(json.loads(report)["issue"], "unsupported-instruction-record")

    @unittest.skipUnless(
        os.environ.get("IR_NORMALIZER_TEST_BINARY"),
        "requires the temporary pinned normalizer",
    )
    def test_success_publishes_only_normalized_names_and_counts(self):
        with tempfile.TemporaryDirectory() as directory:
            root, capture, path, unrelated = self.make_capture(directory, IR)
            normalizer = Path(os.environ["IR_NORMALIZER_TEST_BINARY"])
            self.assertTrue(
                inventory.collect(capture, root / "artifact", normalizer, expected_codegen_units=1)
            )
            report = (root / "artifact/ir-inventory.json").read_text()
            data = json.loads(report)
            self.assertFalse(path.exists())
            self.assertTrue(unrelated.exists())
            self.assertEqual(data["top_by_instruction_records"][0]["normalized_name"], "demo::run")
            self.assertEqual(data["instruction_records"], 8)
            self.assertNotIn("PRIVATE_", report)
            self.assertNotIn("0123456789abcdef", report)
            self.assertEqual([p.name for p in (root / "artifact").iterdir()], ["ir-inventory.json"])

    def test_duplicate_capture_metadata_cannot_be_published_as_complete(self):
        with tempfile.TemporaryDirectory() as directory:
            root, capture, path, unrelated = self.make_capture(directory, IR)
            (capture / "duplicate-match").write_text("")
            self.assertFalse(
                inventory.collect(
                    capture,
                    root / "artifact",
                    Path("not-used"),
                    expected_codegen_units=1,
                )
            )
            self.assertEqual(
                json.loads((root / "artifact/ir-inventory.json").read_text())["issue"],
                "invalid-invocation",
            )
            self.assertFalse(path.exists())
            self.assertTrue(unrelated.exists())

    def test_flat_ir_is_rejected_and_deleted_without_touching_unrelated_files(self):
        for keep_cgu in (False, True):
            with (
                self.subTest(keep_cgu=keep_cgu),
                tempfile.TemporaryDirectory() as directory,
            ):
                root, capture, path, unrelated = self.make_capture(directory, IR)
                flat = path.parent / "warp-test.ll"
                flat.write_text(IR)
                if not keep_cgu:
                    path.unlink()
                metadata_path = capture / "invocation.json"
                metadata = json.loads(metadata_path.read_text())
                metadata_path.write_text(json.dumps(metadata | {"ir_file_count": 1 + keep_cgu}))
                adjacent = path.parent / "warp-test-other.ll"
                adjacent.write_text("UNRELATED_PRIVATE_FILE")
                self.assertFalse(
                    inventory.collect(
                        capture,
                        root / "artifact",
                        Path("not-used"),
                        expected_codegen_units=1,
                    )
                )
                data = json.loads((root / "artifact/ir-inventory.json").read_text())
                self.assertEqual(data["issue"], "unexpected-codegen-unit-set")
                self.assertTrue(data["owned_ir_deleted"])
                self.assertFalse(flat.exists())
                self.assertFalse(path.exists())
                self.assertTrue(unrelated.exists())
                self.assertTrue(adjacent.exists())

    def test_invalid_codegen_scope_is_rejected_and_owned_ir_is_deleted(self):
        for override in ({"codegen_units": 16}, {"incremental": True}):
            with (
                self.subTest(override=override),
                tempfile.TemporaryDirectory() as directory,
            ):
                root, capture, path, unrelated = self.make_capture(directory, IR)
                metadata_path = capture / "invocation.json"
                metadata = json.loads(metadata_path.read_text())
                metadata_path.write_text(json.dumps(metadata | override))
                self.assertFalse(
                    inventory.collect(
                        capture,
                        root / "artifact",
                        Path("not-used"),
                        expected_codegen_units=1,
                    )
                )
                data = json.loads((root / "artifact/ir-inventory.json").read_text())
                self.assertEqual(data["issue"], "invalid-invocation")
                self.assertFalse(path.exists())
                self.assertTrue(unrelated.exists())


if __name__ == "__main__":
    unittest.main()
