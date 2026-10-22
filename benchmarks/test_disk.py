import os
import tempfile
import unittest
from pathlib import Path

import disk
from disk import build_repository, content, measure


class DiskTests(unittest.TestCase):
    def test_measure_counts_hard_links_once_and_never_follows_symlinks(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "data.bin").write_bytes(b"x" * 10_000)
            os.link(root / "data.bin", root / "linked.bin")
            outside = Path(tempfile.mkdtemp())
            try:
                (outside / "big.bin").write_bytes(b"y" * 100_000)
                os.symlink(outside, root / "outside")
                apparent, allocated = measure(root)
                self.assertGreaterEqual(apparent, 10_000)
                self.assertLess(apparent, 20_000)
                self.assertGreaterEqual(allocated, 8_192)
            finally:
                (outside / "big.bin").unlink()
                outside.rmdir()

    def test_settle_waits_for_late_writers_and_cleanup_survives_them(self):
        import threading
        import time
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "tool"
            root.mkdir()

            def late_writer():
                for index in range(4):
                    time.sleep(0.3)
                    (root / f"cache{index}.db").write_bytes(b"z" * 5_000)

            writer = threading.Thread(target=late_writer)
            writer.start()
            (apparent, _), settled, _ = disk.settle(root)
            writer.join()
            self.assertTrue(settled)
            self.assertGreaterEqual(apparent, 20_000)
            disk.remove_tree(root)
            self.assertFalse(root.exists())

    def test_synthetic_repository_is_deterministic(self):
        self.assertEqual(content(3, 500), content(3, 500))
        self.assertEqual(len(content(3, 500)), 500)
        with tempfile.TemporaryDirectory() as temporary:
            heads = [
                build_repository(Path(temporary) / name, 10, 200).joinpath("src").exists()
                for name in ("a", "b")
            ]
            self.assertEqual(heads, [True, True])
            files = sorted(path.relative_to(Path(temporary) / "a").as_posix()
                           for path in (Path(temporary) / "a").rglob("*.txt"))
            self.assertEqual(len(files), 10)
            self.assertTrue(all(file.split("/")[0] in {"src", "docs", "assets", "tests", "vendor"}
                                for file in files))


if __name__ == "__main__":
    unittest.main()
