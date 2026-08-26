import copy
import io
import json
import tarfile
import tempfile
import unittest
from pathlib import Path

import install_pty_release as installer

TARGET = "x86_64-unknown-linux-gnu"
ARTIFACT_FILE = f"{installer.SIDECAR_ID}-{installer.VERSION}-{TARGET}.tar.gz"


def release_document() -> dict:
    return {
        "kind": "sidecar",
        "id": installer.SIDECAR_ID,
        "version": installer.VERSION,
        "manifest": {"file": "sidecar.json", "size": 1, "sha256": "c" * 64},
        "source": {"repository": installer.REPOSITORY, "commit": installer.COMMIT},
        "artifacts": [{"target": TARGET, "file": ARTIFACT_FILE, "size": 1, "sha256": "a" * 64, "format": "tar.gz", "manifest": "sidecar.json"}],
        "evidence": [{"file": "report.json", "size": 1, "sha256": "b" * 64}],
    }


class InstallerTest(unittest.TestCase):
    def test_release_identity_and_target_are_exact(self):
        document = release_document()
        self.assertEqual(installer.select_artifact(document, TARGET)["target"], TARGET)
        document["source"]["commit"] = "0" * 40
        with self.assertRaises(ValueError):
            installer.select_artifact(document, TARGET)

    def test_artifact_location_is_derived_from_release_root_and_file(self):
        artifact = installer.select_artifact(release_document(), TARGET)
        self.assertEqual(artifact["file"], ARTIFACT_FILE)
        self.assertEqual(installer.artifact_url(artifact), f"{installer.RELEASE_ROOT}/{ARTIFACT_FILE}")

    def test_artifact_file_must_be_a_bare_filename(self):
        for file in ["", ".", "..", "dist/artifact.tar.gz", "/artifact.tar.gz", installer.RELEASE_ROOT + "/artifact.tar.gz"]:
            document = release_document()
            document["artifacts"][0]["file"] = file
            with self.assertRaises(ValueError, msg=repr(file)):
                installer.select_artifact(document, TARGET)

    def test_document_with_url_is_refused(self):
        location = f"{installer.RELEASE_ROOT}/{ARTIFACT_FILE}"
        with_artifact_url = release_document()
        with_artifact_url["artifacts"][0]["url"] = location
        with_evidence_url = release_document()
        with_evidence_url["evidence"][0]["url"] = f"{installer.RELEASE_ROOT}/report.json"
        with_manifest_url = release_document()
        with_manifest_url["manifest"]["url"] = f"{installer.RELEASE_ROOT}/sidecar.json"
        url_instead_of_file = release_document()
        del url_instead_of_file["artifacts"][0]["file"]
        url_instead_of_file["artifacts"][0]["url"] = location
        for document in [with_artifact_url, with_evidence_url, with_manifest_url, url_instead_of_file]:
            with self.assertRaises(ValueError, msg=json.dumps(document)):
                installer.select_artifact(copy.deepcopy(document), TARGET)

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
                installer.extract_verified_archive(archive.getvalue(), Path(directory), TARGET)


if __name__ == "__main__":
    unittest.main()
