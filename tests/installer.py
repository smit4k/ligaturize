"""Exercise installation and failure paths without a network or a published release."""

import hashlib
import io
import os
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest

INSTALLER = Path(__file__).resolve().parents[1] / "install.sh"


class InstallerTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.mockbin = self.root / "mockbin"
        self.mockbin.mkdir()
        self.assets = self.root / "assets"
        self.assets.mkdir()
        self.destination = self.root / "install with spaces"
        self.env = {
            **os.environ,
            "PATH": f"{self.mockbin}:{os.environ['PATH']}",
            "LIGATURIZE_REPO": "example/ligaturize",
            "LIGATURIZE_INSTALL_DIR": str(self.destination),
            "TEST_ASSETS": str(self.assets),
            "TEST_OS": "Darwin",
            "TEST_ARCH": "arm64",
        }
        self.executable("uname", '#!/bin/sh\ncase "$1" in -s) echo "$TEST_OS";; -m) echo "$TEST_ARCH";; esac\n')
        self.executable("curl", f"#!{sys.executable}\n" + '''import os, pathlib, shutil, sys
args = sys.argv[1:]
url = next(a for a in args if a.startswith("https://"))
assert url.startswith("https://github.com/example/ligaturize/releases/")
assert "--proto" in args and "=https" in args
target = args[args.index("-o") + 1]
shutil.copyfile(pathlib.Path(os.environ["TEST_ASSETS"]) / url.rsplit("/", 1)[1], target)
''')

    def executable(self, name, contents):
        path = self.mockbin / name
        path.write_text(contents)
        path.chmod(0o755)

    def archive(self, target, corrupt=False, symlink=False):
        archive = self.assets / f"ligaturize-{target}.tar.gz"
        contents = b"#!/bin/sh\nprintf 'ligaturize 0.1.0\\n'\n"
        with tarfile.open(archive, "w:gz") as tar:
            entry = tarfile.TarInfo("ligaturize")
            entry.mode = 0o755
            if symlink:
                entry.type = tarfile.SYMTYPE
                entry.linkname = "/bin/sh"
                tar.addfile(entry)
            else:
                entry.size = len(contents)
                tar.addfile(entry, io.BytesIO(contents))
        checksum = "0" * 64 if corrupt else hashlib.sha256(archive.read_bytes()).hexdigest()
        (self.assets / "SHA256SUMS").write_text(f"{checksum}  {archive.name}\n")

    def run_installer(self):
        return subprocess.run(["sh", str(INSTALLER)], env=self.env, capture_output=True, text=True)

    def test_all_platforms_and_version_override(self):
        for system, arch, target in [
            ("Darwin", "arm64", "aarch64-apple-darwin"),
            ("Darwin", "x86_64", "x86_64-apple-darwin"),
            ("Linux", "aarch64", "aarch64-unknown-linux-musl"),
            ("Linux", "x86_64", "x86_64-unknown-linux-musl"),
        ]:
            with self.subTest(target=target):
                self.env.update(TEST_OS=system, TEST_ARCH=arch, LIGATURIZE_VERSION="v0.1.0")
                self.archive(target)
                result = self.run_installer()
                self.assertEqual(result.returncode, 0, result.stderr)
                installed = subprocess.check_output([str(self.destination / "ligaturize")], text=True)
                self.assertEqual(installed, "ligaturize 0.1.0\n")
                self.assertEqual(list(self.destination.glob(".ligaturize.*")), [])

    def test_bad_checksum_preserves_existing_install(self):
        self.destination.mkdir()
        binary = self.destination / "ligaturize"
        binary.write_bytes(b"existing version")
        self.archive("aarch64-apple-darwin", corrupt=True)
        result = self.run_installer()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("checksum mismatch", result.stderr)
        self.assertEqual(binary.read_bytes(), b"existing version")

    def test_release_installer_has_a_working_default_repository(self):
        configured = self.root / "release-install.sh"
        configured.write_text(INSTALLER.read_text().replace("__REPOSITORY__", "example/ligaturize"))
        self.env.pop("LIGATURIZE_REPO")
        self.archive("aarch64-apple-darwin")
        result = subprocess.run(["sh", str(configured)], env=self.env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue((self.destination / "ligaturize").exists())

    def test_unsupported_platform_and_unconfigured_repo_fail(self):
        self.env["TEST_OS"] = "Windows"
        self.assertNotEqual(self.run_installer().returncode, 0)
        self.env.pop("LIGATURIZE_REPO")
        self.assertIn("set LIGATURIZE_REPO", self.run_installer().stderr)
        self.assertFalse(self.destination.exists())

    def test_symlink_in_archive_is_rejected(self):
        self.archive("aarch64-apple-darwin", symlink=True)
        result = self.run_installer()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("regular executable", result.stderr)
        self.assertFalse(self.destination.exists())


if __name__ == "__main__":
    unittest.main()
