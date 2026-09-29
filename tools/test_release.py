import base64
import hashlib
import unittest

import release


class ChangelogTests(unittest.TestCase):
    def test_publication_orders_releases_and_retries_without_duplication(self) -> None:
        empty = '+++\ntitle = "Changelog"\n+++\n\nNo published releases yet.\n'
        first = release.changelog_with_release(empty, "v0.1.0", "2026-09-26", "## Highlights\n\nFirst release.\n")
        result = release.changelog_with_release(first, "v0.2.0", "2026-10-01", "## Fixes\n\nResolved startup failure.\n")

        self.assertLess(result.index("v0.2.0"), result.index("v0.1.0"))
        self.assertIn("### Fixes\n\nResolved startup failure.", result)
        self.assertNotIn("No published releases yet.", result)
        self.assertEqual(result, release.changelog_with_release(result, "v0.2.0", "2026-10-02", "## Fixes\n\nResolved startup failure.\n"))

        with self.assertRaisesRegex(release.ReleaseError, "differs from reviewed notes"):
            release.changelog_with_release(result, "v0.2.0", "2026-10-02", "## Fixes\n\nDifferent claim.\n")


class DownloadTableTests(unittest.TestCase):
    def test_links_and_hashes_match_all_published_assets(self) -> None:
        source = "# AXE\n\n## Downloads\n\nNo verified release.\n\n## What's inside\n\nUseful tools.\n"
        assets = {}
        records = {}
        for binary in release.BINARIES:
            records[binary] = {}
            for target in release.TARGETS:
                name = release.asset_name(binary, target)
                digest = hashlib.sha256(name.encode()).digest()
                assets[name] = digest.hex()
                records[binary][target] = {
                    "version": "0.2.0",
                    "url": f"https://example.org/axe/releases/v0.2.0/{target}/{binary}",
                    "stable_url": f"https://example.org/axe/stable/{target}/{binary}",
                    "hash": "sha256-" + base64.b64encode(digest).decode(),
                }

        result = release.render_downloads(source, "v0.2.0", records, assets)
        for binary in release.BINARIES:
            for target in release.TARGETS:
                name = release.asset_name(binary, target)
                self.assertIn(f"/releases/v0.2.0/{target}/{binary}", result)
                self.assertIn(f"/v0.2.0/{name}", result)
                self.assertIn(assets[name], result)

        self.assertIn("## What's inside\n\nUseful tools.", result)
        self.assertEqual(result, release.render_downloads(result, "v0.2.0", records, assets))

        original_hash = records["vzik"]["x86_64-linux"]["hash"]
        records["vzik"]["x86_64-linux"]["hash"] = "sha256-" + base64.b64encode(bytes(32)).decode()
        with self.assertRaisesRegex(release.ReleaseError, "does not match GitHub asset"):
            release.render_downloads(source, "v0.2.0", records, assets)

        records["vzik"]["x86_64-linux"]["hash"] = original_hash
        records["axe-relay"]["aarch64-linux"]["url"] = records["axe-relay"]["aarch64-linux"]["url"].replace("/v0.2.0/", "/v0.1.0/")
        with self.assertRaisesRegex(release.ReleaseError, "unexpected release URL"):
            release.render_downloads(source, "v0.2.0", records, assets)


if __name__ == "__main__":
    unittest.main()
