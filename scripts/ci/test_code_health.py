#!/usr/bin/env python3
"""Tests of scripts/ci/code_health.py on small fabricated trees.

    python3 scripts/ci/test_code_health.py
"""

from __future__ import annotations

import io
import json
import os
import sys
import tempfile
import textwrap
import unittest
from contextlib import redirect_stdout

sys.dont_write_bytecode = True  # leave no __pycache__ in the checkout
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import code_health as ch  # noqa: E402


def long_fn(name: str, lines: int, prefix: str = "pub fn") -> str:
    body = "".join(f"    let _v{i} = {i};\n" for i in range(lines - 2))
    return f"{prefix} {name}() {{\n{body}}}\n"


class Tree:
    """A throwaway repository with the folders the gate reads."""

    def __init__(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.root = self._tmp.name
        os.makedirs(os.path.join(self.root, ".github"))
        self.write("src-tauri/src/lib.rs", "pub mod a;\n")
        self.write("src/main.tsx", "export {}\n")

    def write(self, rel: str, text: str) -> None:
        path = os.path.join(self.root, rel)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w", encoding="utf-8") as fh:
            fh.write(text)

    def run(self, *args: str) -> tuple[int, str]:
        out = io.StringIO()
        with redirect_stdout(out):
            code = ch.main(["--repo", self.root, *args])
        return code, out.getvalue()

    def baseline(self) -> dict:
        with open(os.path.join(self.root, ch.BASELINE), encoding="utf-8") as fh:
            return json.load(fh)

    def close(self) -> None:
        self._tmp.cleanup()


class Lexing(unittest.TestCase):
    def test_literals_and_comments_hide_their_braces(self):
        src = textwrap.dedent(
            r'''
            fn f<'a>(x: &'a str) -> char {
                let s = "{ not a brace \" still }";
                let r = r#"{ raw "quoted" }"#;
                let c = '{';
                let e = '\'';
                /* outer /* nested { */ still } */
                // line { comment
                '}'
            }
            '''
        )
        code, nocom = ch.lex(src)
        self.assertEqual(len(code), len(src))
        self.assertEqual(len(nocom), len(src))
        self.assertEqual(code.count("{"), 1)
        self.assertEqual(code.count("}"), 1)
        self.assertIn("'a", code)  # a lifetime stays code
        self.assertIn('"{ not a brace', nocom)  # literals stay in nocomment
        self.assertNotIn("nested", nocom)

    def test_identifier_ending_in_r_or_b_is_not_a_prefix(self):
        code, _ = ch.lex('let bar = 1; let x = bar; let s = "}";\n')
        self.assertEqual(code.count("}"), 0)


class CfgPredicates(unittest.TestCase):
    def test_test_only(self):
        for attr in ("cfg(test)", "cfg(all(test, unix))", 'cfg(any(test, feature = "test-support"))',
                     'cfg(feature = "test-support")'):
            self.assertTrue(ch.cfg_is_test_only(attr), attr)

    def test_production(self):
        for attr in ("cfg(not(test))", "cfg(unix)", "cfg(any(test, unix))", 'cfg(feature = "runner-window")',
                     "cfg_attr(test, derive(Debug))", "allow(dead_code)"):
            self.assertFalse(ch.cfg_is_test_only(attr), attr)


class RustAnalysis(unittest.TestCase):
    def test_test_modules_are_not_production(self):
        src = "fn a() {}\n#[cfg(not(test))]\nfn b() {}\n#[cfg(test)]\nmod tests {\n    fn t() {}\n}\n"
        f = ch.analyse_rust("x.rs", src)
        self.assertEqual(ch.production_lines(f), 3)
        self.assertEqual([n for n, _, _ in ch.functions(f)], ["a", "b"])

    def test_function_length_and_signatures(self):
        src = "trait T {\n    fn decl(&self);\n}\n" + long_fn("big", 120) + "fn arr(x: [u8; 4]) -> u8 {\n    x[0]\n}\n"
        f = ch.analyse_rust("x.rs", src)
        lengths = {n: length for n, _, length in ch.functions(f)}
        self.assertEqual(lengths, {"big": 120, "arr": 3})

    def test_cfg_test_mod_declaration_is_recorded(self):
        f = ch.analyse_rust("x.rs", "#[cfg(test)]\nmod tests;\nfn a() {}\n")
        self.assertEqual(f.test_mods, ["tests"])
        self.assertEqual(ch.production_lines(f), 1)


class Measure(unittest.TestCase):
    def setUp(self):
        self.t = Tree()

    def tearDown(self):
        self.t.close()

    def test_measures_each_check(self):
        t = self.t
        t.write(
            "src-tauri/src/a.rs",
            long_fn("used_long", 130)
            + "pub fn caller() { used_long(); }\n"
            + "pub fn never_called_anywhere() {}\n"
            + "#[allow(clippy::too_many_arguments, reason = \"a, b\")]\nfn many() {}\n"
            + "#[cfg(test)]\nmod tests;\n",
        )
        # Declared under cfg(test): neither its length nor its names count as production.
        t.write("src-tauri/src/a/tests.rs", long_fn("in_a_test_file", 300) + "fn t() { caller(); }\n")
        t.write(
            "src-tauri/src/server/routes/x.rs",
            long_fn("handler", 70, "pub async fn") + "fn q(ctx: C) { let c = ctx.sqlite.conn(); }\n",
        )
        t.write("src/pages/Big.tsx", "x\n" * 1001)
        t.write("src/lib/trading/chromeIcons.ts", "x\n" * 5000)  # generated: exempt
        t.write("src/pages/Big.test.tsx", "x\n" * 5000)  # tests: exempt
        m = ch.measure(t.root)
        self.assertEqual(m["rust_functions"], {
            "src-tauri/src/a.rs::used_long": 130,
            "src-tauri/src/server/routes/x.rs::handler": 70,  # a handler's limit is lower
        })
        self.assertEqual(m["handler_db_access"], {"src-tauri/src/server/routes/x.rs": 1})
        self.assertEqual(m["lint_allows"], {"src-tauri/src/a.rs::clippy::too_many_arguments": 1})
        self.assertIn("src-tauri/src/a.rs::never_called_anywhere", m["dead_public_functions"])
        self.assertNotIn("src-tauri/src/a.rs::caller", m["dead_public_functions"])
        self.assertEqual(m["frontend_files"], {"src/pages/Big.tsx": 1001})
        self.assertEqual(m["rust_files"], {})


class Ratchet(unittest.TestCase):
    def setUp(self):
        self._may_grow = ch.EXISTING_MAY_GROW
        self.t = Tree()
        self.t.write("src-tauri/src/a.rs", long_fn("old_long", 150) + "pub fn user() { old_long(); }\n")
        code, _ = self.t.run("--init")
        self.assertEqual(code, 0)

    def tearDown(self):
        self.t.close()
        ch.EXISTING_MAY_GROW = self._may_grow

    def test_unchanged_tree_passes(self):
        self.assertEqual(self.t.run()[0], 0)

    def test_a_new_long_function_fails(self):
        self.t.write("src-tauri/src/b.rs", long_fn("new_long", 101) + "pub fn u() { new_long(); }\n")
        code, out = self.t.run()
        self.assertEqual(code, 1)
        self.assertIn("new_long", out)

    def test_growth_is_a_warning_only_while_the_flag_allows_it(self):
        self.t.write("src-tauri/src/a.rs", long_fn("old_long", 160) + "pub fn user() { old_long(); }\n")
        ch.EXISTING_MAY_GROW = True
        code, out = self.t.run()
        self.assertEqual(code, 0)
        self.assertIn("warning", out)
        ch.EXISTING_MAY_GROW = False
        self.assertEqual(self.t.run()[0], 1)

    def test_the_pass_is_done_so_a_listed_item_that_grows_fails(self):
        # The module's own setting, not one a test chose.
        self.assertFalse(self._may_grow)
        self.t.write("src-tauri/src/a.rs", long_fn("old_long", 151) + "pub fn user() { old_long(); }\n")
        code, out = self.t.run()
        self.assertEqual(code, 1)
        self.assertIn("old_long grew to", out)

    def test_an_improvement_must_be_written_back_and_update_never_raises(self):
        self.t.write("src-tauri/src/a.rs", long_fn("old_long", 120) + "pub fn user() { old_long(); }\n")
        code, out = self.t.run()
        self.assertEqual(code, 1)
        self.assertIn("--update", out)
        self.assertEqual(self.t.run("--update")[0], 0)
        self.assertEqual(self.t.baseline()["rust_functions"], {"src-tauri/src/a.rs::old_long": 120})
        # Growing back is not written into the baseline by --update.
        self.t.write("src-tauri/src/a.rs", long_fn("old_long", 140) + "pub fn user() { old_long(); }\n")
        self.t.run("--update")
        self.assertEqual(self.t.baseline()["rust_functions"], {"src-tauri/src/a.rs::old_long": 120})

    def test_a_fixed_item_leaves_the_baseline(self):
        self.t.write("src-tauri/src/a.rs", long_fn("old_long", 50) + "pub fn user() { old_long(); }\n")
        self.assertEqual(self.t.run()[0], 1)
        self.assertEqual(self.t.run("--update")[0], 0)
        self.assertEqual(self.t.baseline()["rust_functions"], {})


class Gained(unittest.TestCase):
    def test_added_or_raised_entries_are_refused_and_renames_are_not(self):
        base = {"rust_functions": {"a.rs::f": 150, "a.rs::g": 120}, "dead_public_functions": ["a.rs::x"]}
        same = json.loads(json.dumps(base))
        self.assertEqual(ch.gained(same, base), [])
        raised = {"rust_functions": {"a.rs::f": 151, "a.rs::g": 120}}
        self.assertEqual(len(ch.gained(raised, base)), 1)
        added = {"rust_functions": {"a.rs::f": 150, "a.rs::g": 120, "b.rs::h": 101}}
        self.assertEqual(len(ch.gained(added, base)), 1)
        renamed = {"rust_functions": {"a.rs::f": 150, "b.rs::g2": 118}, "dead_public_functions": ["b.rs::x"]}
        self.assertEqual(ch.gained(renamed, base), [])



class CodeqlScope(unittest.TestCase):
    """Only test code may be left out of CodeQL (alerts 292, 293, 301)."""

    CONFIG = textwrap.dedent(
        """\
        name: test
        paths-ignore:
          # a comment
          - src-tauri/vendor/**
          - "src-tauri/src/**/*_tests.rs"
          - "src-tauri/src/**/*_tests/**"
        """
    )

    def setUp(self):
        self.t = Tree()
        self.t.write(ch.CODEQL_CONFIG, self.CONFIG)
        self.t.write(
            "src-tauri/src/lib.rs",
            "pub mod a;\n#[cfg(test)]\nmod a_tests;\n#[cfg(test)]\nmod b_tests;\n",
        )
        self.t.write("src-tauri/src/a.rs", "pub fn f() {}\n")
        self.t.write("src-tauri/src/a_tests.rs", "fn t() { crate::a::f(); }\n")
        self.t.write("src-tauri/src/b_tests/mod.rs", "mod c;\n")
        self.t.write("src-tauri/src/b_tests/c.rs", "fn t() {}\n")
        self.assertEqual(self.t.run("--init")[0], 0)

    def tearDown(self):
        self.t.close()

    def test_test_files_and_folders_may_be_left_out(self):
        self.assertEqual(ch.measure(self.t.root)["codeql_hidden"], [])
        self.assertEqual(self.t.run()[0], 0)

    def test_a_glob_that_leaves_out_production_code_fails(self):
        # Named like a test, but compiled into the app.
        self.t.write("src-tauri/src/lib.rs", "pub mod a;\npub mod a_tests;\n#[cfg(test)]\nmod b_tests;\n")
        self.assertEqual(ch.measure(self.t.root)["codeql_hidden"], ["src-tauri/src/a_tests.rs"])
        code, out = self.t.run()
        self.assertEqual(code, 1)
        self.assertIn("keeps this production file out of CodeQL", out)

    def test_globs_follow_codeql(self):
        g = ch._glob_re
        self.assertTrue(g("src-tauri/src/**/tests.rs").match("src-tauri/src/tests.rs"))
        self.assertTrue(g("src-tauri/src/**/tests.rs").match("src-tauri/src/a/b/tests.rs"))
        self.assertFalse(g("src-tauri/src/*.rs").match("src-tauri/src/a/b.rs"))
        self.assertFalse(g("src-tauri/src/**/tests.rs").match("src-tauri/src/a/tests.rs.bak"))
        # A folder glob leaves out what is inside it.
        self.assertTrue(ch._hidden("src-tauri/src/x_tests/deep/y.rs", [g("src-tauri/src/**/*_tests")]))

    def test_the_repository_config_hides_no_production_code(self):
        repo = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
        self.assertEqual(ch.measure(repo)["codeql_hidden"], [])


if __name__ == "__main__":
    unittest.main(verbosity=2)
