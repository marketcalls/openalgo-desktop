#!/usr/bin/env python3
"""Tests of scripts/ci/check_windows_dlls.py.

    python3 scripts/ci/test_check_windows_dlls.py

The check runs on PE files built here with chosen import and delay-import
tables. The workflow tests read the YAML as text: every Windows build that
sets RUSTFLAGS keeps the static C runtime .cargo/config.toml asks for, and
the shipped binary is checked where it is built and where it is installed.
"""

from __future__ import annotations

import io
import os
import re
import sys
import tempfile
import unittest
from contextlib import redirect_stdout

sys.dont_write_bytecode = True  # leave no __pycache__ in the checkout
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import check_windows_dlls as check  # noqa: E402

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..")
WORKFLOWS = os.path.join(ROOT, ".github", "workflows")
SECTION_RVA, SECTION_RAW = 0x1000, 0x400
IMAGE_BASE, IMAGE_BASE_32 = 0x140000000, 0x400000


def pe(imports=(), delayed=(), *, pe32=False, delay_by_va=False) -> bytes:
    """A PE file with one section holding the import descriptors, the
    delay-import descriptors and the DLL names they point to."""
    body = bytearray()
    names = {}
    for dll in (*imports, *delayed):
        names[dll] = SECTION_RVA + 0x200 + sum(len(n) + 1 for n in names)
    import_rva = SECTION_RVA
    for dll in imports:
        body += bytes(12) + names[dll].to_bytes(4, "little") + (1).to_bytes(4, "little")
    body += bytes(20)
    delay_rva = SECTION_RVA + len(body)
    for dll in delayed:
        rva = names[dll] + (IMAGE_BASE_32 if delay_by_va else 0)
        body += (0 if delay_by_va else 1).to_bytes(4, "little") + rva.to_bytes(4, "little") + bytes(24)
    body += bytes(32)
    body = body.ljust(0x200, b"\0") + b"".join(n.encode() + b"\0" for n in names)

    head = bytearray(SECTION_RAW)
    head[:2] = b"MZ"
    head[0x3C:0x40] = (0x80).to_bytes(4, "little")
    head[0x80:0x84] = b"PE\0\0"
    head[0x86:0x88] = (1).to_bytes(2, "little")  # NumberOfSections
    opt = 0x80 + 24
    directories = opt + (96 if pe32 else 112)
    head[0x94:0x96] = (directories - opt + 16 * 8).to_bytes(2, "little")  # SizeOfOptionalHeader
    head[opt : opt + 2] = (0x10B if pe32 else 0x20B).to_bytes(2, "little")
    if pe32:
        head[opt + 28 : opt + 32] = IMAGE_BASE_32.to_bytes(4, "little")
    else:
        head[opt + 24 : opt + 32] = IMAGE_BASE.to_bytes(8, "little")
    head[directories - 4 : directories] = (16).to_bytes(4, "little")
    if imports:
        head[directories + 8 : directories + 12] = import_rva.to_bytes(4, "little")
    if delayed:
        head[directories + 8 * 13 : directories + 8 * 13 + 4] = delay_rva.to_bytes(4, "little")
    section = directories + 16 * 8
    head[section : section + 8] = b".idata\0\0"
    head[section + 8 : section + 12] = len(body).to_bytes(4, "little")
    head[section + 12 : section + 16] = SECTION_RVA.to_bytes(4, "little")
    head[section + 16 : section + 20] = len(body).to_bytes(4, "little")
    head[section + 20 : section + 24] = SECTION_RAW.to_bytes(4, "little")
    return bytes(head) + bytes(body)


def run(data: bytes) -> tuple[int, str]:
    with tempfile.TemporaryDirectory() as tmp:
        path = os.path.join(tmp, "openalgo-desktop.exe")
        with open(path, "wb") as f:
            f.write(data)
        out = io.StringIO()
        with redirect_stdout(out):
            code = check.main([path])
    return code, out.getvalue()


class ImportedDlls(unittest.TestCase):
    def test_reads_the_import_table(self):
        self.assertEqual(check.imported_dlls(pe(["KERNEL32.dll", "MSVCP140.dll"])), ["KERNEL32.dll", "MSVCP140.dll"])

    def test_reads_the_delay_import_table(self):
        data = pe(["KERNEL32.dll"], ["VCRUNTIME140.dll"])
        self.assertEqual(check.imported_dlls(data), ["KERNEL32.dll", "VCRUNTIME140.dll"])

    def test_reads_an_old_delay_import_table_of_virtual_addresses(self):
        # Before Visual C++ 7, a 32-bit executable's delay-import names were
        # virtual addresses (attribute bit 0 clear).
        data = pe(["KERNEL32.dll"], ["MSVCP60.dll"], pe32=True, delay_by_va=True)
        self.assertEqual(check.imported_dlls(data), ["KERNEL32.dll", "MSVCP60.dll"])

    def test_reads_a_32_bit_executable(self):
        self.assertEqual(check.imported_dlls(pe(["USER32.dll"], pe32=True)), ["USER32.dll"])

    def test_refuses_a_file_that_is_not_an_executable(self):
        with self.assertRaises(ValueError):
            check.imported_dlls(b"\x7fELF" + bytes(100))


class Main(unittest.TestCase):
    def test_passes_windows_dlls_and_api_sets_in_any_case(self):
        code, out = run(pe(["KERNEL32.dll", "RstrtMgr.DLL", "api-ms-win-crt-heap-l1-1-0.dll"], ["ext-ms-win-ntuser-l1.dll"]))
        self.assertEqual(code, 0, out)
        self.assertIn("OK: every DLL", out)

    def test_refuses_the_visual_cpp_runtime(self):
        code, out = run(pe(["KERNEL32.dll", "MSVCP140.dll"], ["VCRUNTIME140_1.dll"]))
        self.assertEqual(code, 1)
        self.assertIn("needs MSVCP140.dll, which Windows does not ship", out)
        self.assertIn("needs VCRUNTIME140_1.dll, which Windows does not ship", out)

    def test_refuses_any_dll_that_is_not_part_of_windows(self):
        code, out = run(pe(["KERNEL32.dll", "libssl-3-x64.dll"]))
        self.assertEqual(code, 1)
        self.assertIn("needs libssl-3-x64.dll", out)


def text(name: str) -> str:
    with open(os.path.join(ROOT, name), encoding="utf-8") as f:
        return f.read()


class Workflows(unittest.TestCase):
    def test_cargo_config_links_the_c_runtime_statically_for_msvc(self):
        config = text(".cargo/config.toml")
        self.assertRegex(config, r"\[target\.'cfg\(all\(windows, target_env = \"msvc\"\)\)'\]\nrustflags = \[\"-C\", \"target-feature=\+crt-static\"\]")

    def test_every_windows_build_that_sets_rustflags_keeps_the_static_c_runtime(self):
        ci = text(".github/workflows/ci.yml")
        test = ci[ci.index("\n  rust-test:\n") : ci.index("\n  rust-coverage:\n")]
        # Windows first, with the static C runtime; ARM64 Linux links with
        # lld (its debug test binaries pass AArch64's branch range); the rest
        # take the plain flags.
        self.assertIn("RUSTFLAGS: ${{ matrix.os == 'windows-latest' && '-D warnings -C target-feature=+crt-static' || matrix.os == 'ubuntu-22.04-arm' && '-D warnings -C link-arg=-fuse-ld=lld' || '-D warnings' }}", test)
        e2e = text(".github/workflows/native-e2e-test-build.yml")
        windows = e2e[e2e.index("--target x86_64-pc-windows-msvc") - 400 : e2e.index("--target x86_64-pc-windows-msvc")]
        self.assertIn("RUSTFLAGS: -D warnings -C target-feature=+crt-static", windows)
        for name in ("release.yml", "install-smoke.yml"):
            self.assertNotIn("RUSTFLAGS", text(f".github/workflows/{name}"), name)
        # CI's tauri-build makes the installers a release ships, so it takes
        # the static C runtime from .cargo/config.toml, never a RUSTFLAGS:
        # neither its own nor one from ci.yml's top-level env, which every
        # job inherits.
        build = ci[ci.index("\n  tauri-build:\n") : ci.index("\n  install-smoke:\n")]
        self.assertNotIn("RUSTFLAGS", build)
        self.assertNotIn("RUSTFLAGS", ci[: ci.index("\njobs:\n")])

    def test_the_shipped_and_the_installed_binary_are_checked(self):
        release = text(".github/workflows/release.yml")
        self.assertIn('python scripts/ci/check_windows_dlls.py "src-tauri/target/${TARGET}/release/openalgo-desktop.exe"', release)
        smoke = text(".github/workflows/install-smoke.yml")
        windows = smoke[smoke.index("\n  windows:\n") : smoke.index("\n  macos:\n")]
        self.assertIn('python scripts/ci/check_windows_dlls.py "$APP_EXE"', windows)
        e2e = text(".github/workflows/native-e2e-test-build.yml")
        self.assertIn("python scripts/ci/check_windows_dlls.py src-tauri/target/x86_64-pc-windows-msvc/e2e/openalgo-desktop.exe", e2e)


if __name__ == "__main__":
    unittest.main(verbosity=2)
