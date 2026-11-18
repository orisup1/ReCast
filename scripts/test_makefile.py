"""Exercise build ordering without compiling or cleaning the real workspace."""

from pathlib import Path
import os
import plistlib
import shutil
import subprocess
import sys
import tempfile
import unittest


@unittest.skipUnless(shutil.which("make"), "make is not installed")
class BuildOrderingTests(unittest.TestCase):
    def test_service_paths_survive_spaces_and_special_characters(self):
        makefile = Path(__file__).resolve().parents[1] / "Makefile"
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            (root / "Cargo.toml").write_text('version = "0.0.0"\n')
            binary = root / "target/release/recast"
            binary.parent.mkdir(parents=True)
            binary.write_text("test binary")
            for tool in ("systemctl", "launchctl"):
                stub = root / tool
                stub.write_text("#!/bin/sh\nexit 0\n")
                stub.chmod(0o755)
            prefix = root / "Tools & <Games> 100%"
            for target in ("service-macos", "service-linux"):
                subprocess.run(
                    ["make", "-f", str(makefile), "-o", "setup-input-group", target,
                     "SRC=", f"PREFIX={prefix}", f"HOME={root}/test home", f"XDG_CONFIG_HOME={root}/settings dir"],
                    cwd=root, env={**os.environ, "PATH": str(root) + ":" + os.environ["PATH"]},
                    capture_output=True, text=True, check=True,
                )
            installed = str(prefix / "bin/recast")
            plist = root / "test home/Library/LaunchAgents/org.recast.plist"
            self.assertEqual(plistlib.loads(plist.read_bytes())["ProgramArguments"], [installed])
            unit = root / "settings dir/systemd/user/recast.service"
            self.assertIn(f'ExecStart="{installed.replace("%", "%%")}"', unit.read_text().splitlines())

    def test_parallel_rebuild_and_deploy_use_a_fresh_binary(self):
        makefile = Path(__file__).resolve().parents[1] / "Makefile"
        for target in ("rebuild", "deploy"):
            with self.subTest(target=target), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                shutil.copyfile(makefile, root / "Makefile")
                (root / "Cargo.toml").write_text('version = "0.0.0"\n')
                binary = root / "target/release/recast"
                binary.parent.mkdir(parents=True)
                binary.write_text("old binary\n")
                (root / "cargo.py").write_text('''\
from pathlib import Path
import shutil
import sys
import time

if sys.argv[1] == "clean":
    time.sleep(0.1)
    shutil.rmtree("target")
else:
    path = Path("target/release/recast")
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("fresh binary\\n")
''')
                result = subprocess.run(
                    ["make", "-j4", target, "SRC=Cargo.toml",
                     f"CARGO={sys.executable} cargo.py", "PREFIX=installed"],
                    cwd=root, capture_output=True, text=True, timeout=10,
                )
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertTrue(binary.exists(), result.stdout + result.stderr)
                self.assertEqual(binary.read_text(), "fresh binary\n")
                if target == "deploy":
                    self.assertEqual((root / "installed/bin/recast").read_text(), "fresh binary\n")


if __name__ == "__main__":
    unittest.main()
