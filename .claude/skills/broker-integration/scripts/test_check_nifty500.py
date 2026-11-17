"""check_nifty500.py never opens the trader's data folder, however its path is spelled.

The guard compared path text, and macOS reaches one folder by several texts:
another letter case (APFS is case-insensitive by default) and the
/System/Volumes/Data firmlink. Every test points HOME and the data-folder
variables at a temporary folder, so the real data folder is never touched.

Run: python3 -I .claude/skills/broker-integration/scripts/test_check_nifty500.py
"""

from __future__ import annotations

import importlib.util
import os
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
ENV = ("HOME", "USERPROFILE", "XDG_DATA_HOME", "APPDATA", "LOCALAPPDATA")


def load_script():
    spec = importlib.util.spec_from_file_location("check_nifty500", HERE / "check_nifty500.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


check = load_script()


class RealDataFolderGuard(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.root = Path(os.path.realpath(self._tmp.name))
        home = self.root / "home"
        self.folder = home / "Library" / "Application Support" / check.APP_IDENTIFIER
        self.folder.mkdir(parents=True)
        self.db = self.folder / "openalgo.db"
        self.db.write_bytes(b"")
        self._saved = {name: os.environ.get(name) for name in ENV}
        os.environ["HOME"] = str(home)
        os.environ["USERPROFILE"] = str(home)
        for name in ("XDG_DATA_HOME", "APPDATA", "LOCALAPPDATA"):
            os.environ.pop(name, None)

    def tearDown(self) -> None:
        for name, value in self._saved.items():
            if value is None:
                os.environ.pop(name, None)
            else:
                os.environ[name] = value
        self._tmp.cleanup()

    def refused(self, path: Path) -> bool:
        try:
            check.refuse_real_data_folder(path)
        except SystemExit:
            return True
        return False

    def test_the_folder_as_written_is_refused(self) -> None:
        self.assertTrue(self.refused(self.db))
        self.assertTrue(self.refused(self.folder))

    def test_a_link_to_the_folder_is_refused(self) -> None:
        link = self.root / "link"
        try:
            os.symlink(self.folder, link, target_is_directory=True)
        except OSError as exc:
            self.skipTest(f"no symbolic links here: {exc}")
        self.assertTrue(self.refused(link / "openalgo.db"))

    def test_another_letter_case_is_refused(self) -> None:
        variant = self.folder.with_name(check.APP_IDENTIFIER.upper()) / "OPENALGO.DB"
        if not variant.exists():
            self.skipTest("case-sensitive file system: that spelling is another folder")
        self.assertTrue(self.refused(variant))

    def test_the_data_volume_spelling_is_refused(self) -> None:
        firmlink = Path("/System/Volumes/Data" + str(self.db))
        if sys.platform != "darwin" or not firmlink.exists():
            self.skipTest("no /System/Volumes/Data firmlink on this system")
        self.assertTrue(self.refused(firmlink))

    def test_a_throwaway_data_folder_is_allowed(self) -> None:
        elsewhere = self.root / "dev" / check.APP_IDENTIFIER
        elsewhere.mkdir(parents=True)
        self.assertFalse(self.refused(elsewhere / "openalgo.db"))
        self.assertFalse(self.refused(self.root / "missing" / "openalgo.db"))


if __name__ == "__main__":
    unittest.main()
