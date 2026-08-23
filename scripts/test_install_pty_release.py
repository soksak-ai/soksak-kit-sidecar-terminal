import io
import json
import tarfile
import tempfile
import unittest
from pathlib import Path

import install_pty_release as installer


class InstallerTest(unittest.TestCase):
    def test_release_identity_and_target_are_exact(self):
        document = {
            "sidecar": {"id": installer.SIDECAR_ID, "version": installer.VERSION},
            "source": {"repository": installer.REPOSITORY, "commit": installer.COMMIT},
            "artifacts": [{"target": "x86_64-unknown-linux-gnu", "url": installer.RELEASE_ROOT + "/artifact.tar.gz", "sha256": "a" * 64, "size": 1, "format": "tar.gz", "manifest": "sidecar.json"}],
            "reports": [{"url": installer.RELEASE_ROOT + "/report.json", "sha256": "b" * 64}],
        }
        self.assertEqual(installer.select_artifact(document, "x86_64-unknown-linux-gnu")["target"], "x86_64-unknown-linux-gnu")
        document["source"]["commit"] = "0" * 40
        with self.assertRaises(ValueError):
            installer.select_artifact(document, "x86_64-unknown-linux-gnu")

    def test_archive_rejects_links_and_parent_paths(self):
        for name, link in [("../escape", None), ("dist/process", "../escape")]:
            archive = io.BytesIO()
            with tarfile.open(fileobj=archive, mode="w:gz") as writer:
                info = tarfile.TarInfo(name)
                if link is None:
                    info.size = 1
                    writer.addfile(info, io.BytesIO(b"x"))
                else:
                    info.type = tarfile.SYMTYPE
                    info.linkname = link
                    writer.addfile(info)
            with tempfile.TemporaryDirectory() as directory, self.assertRaises(ValueError):
                installer.extract_verified_archive(archive.getvalue(), Path(directory), "x86_64-unknown-linux-gnu")


if __name__ == "__main__":
    unittest.main()
