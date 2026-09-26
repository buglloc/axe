#!/usr/bin/env python3
"""Prepare and publish an OSS release from the local, trusted publisher checkout."""

import argparse
import base64
import hashlib
import json
import os
from datetime import datetime, timezone
from pathlib import Path
import re
import shutil
import subprocess
import stat
import sys
import tempfile
import tomllib
from urllib.request import urlopen


ROOT = Path(__file__).resolve().parent.parent
TARGETS = {
    "x86_64-linux": "axe-x86_64-unknown-linux-musl",
    "aarch64-linux": "axe-aarch64-unknown-linux-musl",
    "aarch64-darwin": "axe-aarch64-apple-darwin",
}
RELAY = (
    "axe-relay-x86_64-unknown-linux-musl",
    "axe-relay-aarch64-unknown-linux-musl",
    "axe-relay-aarch64-apple-darwin",
)
VERSION = re.compile(r"\d+\.\d+\.\d+\Z")


class ReleaseError(Exception):
    pass


def run(*args: str, cwd: Path = ROOT, capture: bool = False, check: bool = True) -> str:
    result = subprocess.run(args, cwd=cwd, text=True, capture_output=capture, check=False)
    if check and result.returncode:
        raise ReleaseError(f"{' '.join(args)} failed ({result.returncode}): {result.stderr or ''}")
    return result.stdout.strip() if capture else ""


def git(*args: str) -> str:
    return run("git", *args, capture=True)


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def atomic_write(path: Path, data: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(mode="w", dir=path.parent, prefix=".release-", delete=False) as stream:
        temporary = Path(stream.name)
        try:
            stream.write(data)
            stream.flush()
            os.fsync(stream.fileno())
        except BaseException:
            temporary.unlink(missing_ok=True)
            raise
    try:
        os.chmod(temporary, stat.S_IMODE(path.stat().st_mode) if path.exists() else 0o644)
        temporary.replace(path)
    except BaseException:
        temporary.unlink(missing_ok=True)
        raise


def clean() -> None:
    if git("status", "--porcelain", "--untracked-files=normal"):
        raise ReleaseError("checkout is not clean; commit/review changes before publishing")
    if git("branch", "--show-current") != "main":
        raise ReleaseError("release requires the main branch")


def current_version() -> str:
    version = tomllib.loads((ROOT / "Cargo.toml").read_text())["workspace"]["package"]["version"]
    if not VERSION.fullmatch(version):
        raise ReleaseError(f"expected numeric workspace SemVer, got {version}")
    return version


def last_tag() -> str | None:
    tags = git("tag", "--merged", "HEAD", "--list", "v[0-9]*")
    parsed = []
    for tag in tags.splitlines():
        if VERSION.fullmatch(tag[1:]):
            parsed.append((tuple(map(int, tag[1:].split("."))), tag))
    return max(parsed)[1] if parsed else None


def release_notes(page: Path) -> str:
    notes = page.read_text().strip() + "\n"
    if not notes.startswith("## "):
        raise ReleaseError("release notes must start with a Markdown section")
    return notes


def changelog_with_release(text: str, tag: str, date: str, notes: str) -> str:
    header = "+++\ntitle = \"Changelog\"\n+++\n"
    if not text.startswith(header):
        raise ReleaseError("invalid changelog frontmatter")
    body = text[len(header):].strip()

    entry = re.sub(r"(?m)^## ", "### ", notes.strip())
    existing = re.search(rf"(?m)^## \[{re.escape(tag)}\]\([^\n]+\) — [^\n]+$", body)
    if existing:
        following = body[existing.end():].split("\n## [", 1)[0].strip()
        if following != entry:
            raise ReleaseError(f"changelog entry differs from reviewed notes for {tag}")
        return text

    heading = f"## [{tag}](https://github.com/buglloc/axe/releases/tag/{tag}) — {date}"
    previous = "" if body == "No published releases yet." else body
    return header + "\n" + heading + "\n\n" + entry + "\n" + ("\n" + previous + "\n" if previous else "")


def page_for(version: str) -> Path:
    return ROOT / "release-notes" / f"v{version}.md"


def prepare(bump: str) -> None:
    clean()
    old = current_version()
    previous = last_tag()

    if previous is None:
        if bump != "initial":
            raise ReleaseError("first release requires 'initial' (uses the current workspace version)")
        version = old
    else:
        if bump == "initial":
            raise ReleaseError("only the first release accepts 'initial'")
        if old != previous[1:]:
            raise ReleaseError(f"Cargo version {old} does not match last tag {previous}")
        major, minor, patch = map(int, old.split("."))
        version = {"major": f"{major + 1}.0.0", "minor": f"{major}.{minor + 1}.0", "patch": f"{major}.{minor}.{patch + 1}"}[bump]

    tag = f"v{version}"
    if git("tag", "--list", tag):
        raise ReleaseError(f"tag {tag} already exists")
    page = page_for(version)
    if page.exists():
        raise ReleaseError(f"release notes already exist: {page}")

    source = f"{previous}..HEAD" if previous else "HEAD"
    history = git("log", "--format=%H%n%s%n%b%n---", source)
    if not history:
        raise ReleaseError("no commits since previous tag")
    changes = git("diff", "--stat", previous, "HEAD") if previous else git("log", "--stat", "--format=", "HEAD")
    paths = ("crates", "store", "config", "tools", "justfile", "flake.nix", "Cargo.toml", "web", "README.md", "BOOTSTRAP.md")
    patch = git("diff", "--no-ext-diff", previous, "HEAD", "--", *paths) if previous else ""
    if len(patch.encode()) > 150_000:
        patch = "Patch exceeds 150 KiB; no patch supplied. Do not infer behavior from filenames alone."

    context = (
        f"Release {tag}; source range: {previous or 'first release'}..HEAD\n\n"
        f"Commit messages:\n{history}\n\nChanged files:\n{changes}\n\n"
        f"Source diff:\n{patch}\n\n"
        f"Public project description:\n{(ROOT / 'README.md').read_text()[:25000]}\n"
    )
    prompt = (
        "Write English Markdown release notes for the OSS edition, using ONLY the attached "
        "source facts. Start with ## Highlights, then ## Changes and ## Fixes if supported. "
        "The README describes existing capabilities, not necessarily changes in this release: "
        "claim a new change only when supported by the commit range or source diff. "
        "Describe observable behavior, not guesses. Cite a commit hash for each claim. "
        "Do not include a heading for the version, preamble, code fence, or invented links. "
        "If the evidence is insufficient, explicitly say so in the relevant section. "
        "Output ONLY the Markdown body; a human will review before publication. Use skill://avoid-ai-writing"
    )

    with tempfile.TemporaryDirectory(prefix=".axe-release-", dir=ROOT) as temporary:
        evidence = Path(temporary) / "evidence.txt"
        evidence.write_text(context)
        notes = run("omp",  "--no-extensions", "--max-time", "10m", "-p", f"@{evidence}", prompt, capture=True)
    if not notes.startswith("## ") or "```" in notes:
        raise ReleaseError("OMP did not return reviewable Markdown notes")

    manifest = ROOT / "Cargo.toml"
    lock = ROOT / "Cargo.lock"
    original_manifest = manifest.read_text()
    original_lock = lock.read_bytes()
    prefix, separator, section = original_manifest.partition("[workspace.package]\n")
    updated, count = re.subn(r'(?m)^version = "[^"\n]+"$', f'version = "{version}"', section, count=1)
    if not separator or count != 1:
        raise ReleaseError("cannot locate workspace version")

    try:
        atomic_write(manifest, prefix + separator + updated)
        run("cargo", "update", "--workspace", "--offline")
        page.parent.mkdir(parents=True, exist_ok=True)
        atomic_write(page, notes.rstrip() + "\n")
    except BaseException:
        manifest.write_text(original_manifest)
        lock.write_bytes(original_lock)
        page.unlink(missing_ok=True)
        raise

    print(f"Prepared {tag}; review {page}, Cargo.toml, Cargo.lock, then commit and push main.")


def release_cli(edition: Path, stage: Path, phase: str, directory: Path | None) -> None:
    producer = Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target")).resolve() / "debug" / "axe-store"
    command = [str(producer), "--config", str(edition / "config" / "store.json"), "release", "--input", str(stage), "--metadata", str(stage / "axe-releases.json"), f"--{phase}-only"]
    for target in TARGETS:
        command.extend(("--target", target))
    if directory is not None:
        command.extend(("--backend", "directory", "--directory", str(directory)))
    run(*command, cwd=edition)


def verify_public_objects(stage: Path, *, stable: bool = False) -> None:
    records = json.loads((stage / "axe-releases.json").read_text())
    if set(records) != set(TARGETS):
        raise ReleaseError("release metadata does not contain all targets")

    for target, record in records.items():
        expected = sha256(stage / TARGETS[target])
        url = record["stable_url"] if stable else record["url"]
        if not url.startswith("https://"):
            raise ReleaseError(f"release URL is not HTTPS: {target}")
        digest = hashlib.sha256()

        with urlopen(url, timeout=60) as response:
            if response.status != 200 or response.geturl() != url:
                raise ReleaseError(f"release object unreadable at expected URL: {url}")
            remaining = (stage / TARGETS[target]).stat().st_size
            while remaining:
                block = response.read(min(1024 * 1024, remaining))
                if not block:
                    raise ReleaseError(f"truncated release object: {target}")
                digest.update(block)
                remaining -= len(block)
            if response.read(1):
                raise ReleaseError(f"oversized release object: {target}")

        if digest.hexdigest() != expected:
            raise ReleaseError(f"public release object differs from staged asset: {target}")


def render_downloads(readme: str, tag: str, records: dict, assets: dict) -> str:
    if set(records) != set(TARGETS):
        raise ReleaseError("cannot update README from incomplete release metadata")
    github = f"https://github.com/buglloc/axe/releases/download/{tag}"

    lines = [
        "## Downloads",
        "",
        f"Release [{tag}](https://github.com/buglloc/axe/releases/tag/{tag}). Verify the binary's SHA-256 before installation; [SHA256SUMS]({github}/SHA256SUMS) lists every asset.",
        "",
        "| Target | Binary | SHA-256 |",
        "| --- | --- | --- |",
    ]

    for target, name in TARGETS.items():
        record = records[target]
        if (
            record["version"] != tag[1:]
            or not record["url"].startswith("https://")
            or f"/releases/{tag}/{target}/axe" not in record["url"]
        ):
            raise ReleaseError(f"unexpected immutable release URL or version for {target}")
        digest = base64.b64decode(record["hash"].removeprefix("sha256-"), validate=True).hex()
        if record["hash"] != "sha256-" + base64.b64encode(bytes.fromhex(assets[name])).decode() or digest != assets[name]:
            raise ReleaseError(f"metadata hash does not match GitHub asset: {target}")
        lines.append(f"| axe {target} | [GitHub]({github}/{name}) · [S3]({record['url']}) | `{digest}` |")

    for target, name in zip(TARGETS, RELAY, strict=True):
        lines.append(f"| axe-relay {target} | [GitHub]({github}/{name}) | `{assets[name]}` |")
    section = "\n".join(lines) + "\n\n"

    marker = "## Downloads\n"
    if readme.count(marker) != 1:
        raise ReleaseError("README must contain exactly one Downloads section")
    start = readme.index(marker)
    end = readme.find("\n## ", start + len(marker))
    if end < 0:
        raise ReleaseError("README Downloads section must end at the next heading")
    return readme[:start] + section + readme[end + 1:]


def gh_release(tag: str) -> dict | None:
    result = subprocess.run(("gh", "release", "view", tag, "--json", "isDraft,body,assets"), cwd=ROOT, capture_output=True, text=True, check=False)

    if result.returncode == 0:
        return json.loads(result.stdout)
    if "release not found" in result.stderr.lower() or "http 404" in result.stderr.lower():
        return None
    raise ReleaseError(f"cannot inspect GitHub release {tag}: {result.stderr}")


def remote_tag(tag: str) -> str | None:
    output = git("ls-remote", "--tags", "origin", f"refs/tags/{tag}", f"refs/tags/{tag}^{{}}")
    refs = dict(line.split("\t", 1)[::-1] for line in output.splitlines())

    if not refs:
        return None
    return refs.get(f"refs/tags/{tag}^{{}}") or refs.get(f"refs/tags/{tag}")


def verify_remote_source(commit: str) -> None:
    remote = git("ls-remote", "origin", "refs/heads/main")
    if not remote or remote.split()[0] != commit:
        raise ReleaseError("push the reviewed release commit to origin/main first")

    if run("gh", "repo", "view", "--json", "nameWithOwner", "--jq", ".nameWithOwner", capture=True) != "buglloc/axe":
        raise ReleaseError("GitHub CLI is not targeting the public buglloc/axe repository")


def stage_assets(version: str, commit: str, out: Path, edition: Path, page: Path) -> Path:
    stage = out / "releases" / f"v{version}"
    index = edition / "store" / "bootstrap-index.cbor.zst"
    if not index.is_file():
        raise ReleaseError(f"signed Store bootstrap snapshot missing: {index}")
    snapshot = sha256(index)
    notes = release_notes(page)
    manifest_path = stage / "manifest.json"

    if manifest_path.exists():
        manifest = json.loads(manifest_path.read_text())
        if (
            manifest["commit"] != commit
            or manifest["version"] != version
            or manifest["snapshot"] != snapshot
            or manifest["notes"] != hashlib.sha256(notes.encode()).hexdigest()
        ):
            raise ReleaseError("staged release inputs changed; do not reuse or move an existing tag")
        for name, digest in manifest["assets"].items():
            if sha256(stage / name) != digest:
                raise ReleaseError(f"staged release asset changed: {name}")
        return stage
    if stage.exists():
        raise ReleaseError(f"incomplete release staging directory: {stage}; inspect before retry")

    checks = (
        ("cargo", "fmt", "--all", "--", "--check"),
        ("just", "check"),
        ("cargo", "clippy", "--locked", "--workspace", "--all-targets", "--all-features", "--", "-D", "warnings"),
        ("just", "smoke"),
        ("just", "build-all"),
        ("just", "build-relay-all"),
        ("cargo", "build", "--locked", "-p", "axe-store"),
    )
    for command in checks:
        run(*command)

    if sha256(index) != snapshot:
        raise ReleaseError("Store bootstrap changed during build")
    native = out / TARGETS["x86_64-linux"]
    output = run(str(native), "--version", capture=True)
    if f"axe {version}+{commit[:12]} (edition oss;" not in output:
        raise ReleaseError(f"built binary has wrong version, source commit or edition: {output}")

    stage.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".release-stage-", dir=stage.parent) as temporary:
        pending = Path(temporary)
        assets = {}
        for name in (*TARGETS.values(), *RELAY):
            source = out / name
            if not source.is_file():
                raise ReleaseError(f"missing built asset: {source}")
            shutil.copy2(source, pending / name)
            assets[name] = sha256(pending / name)

        atomic_write(pending / "notes.md", notes)
        atomic_write(pending / "SHA256SUMS", "".join(f"{digest}  {name}\n" for name, digest in sorted(assets.items())))
        assets["SHA256SUMS"] = sha256(pending / "SHA256SUMS")
        atomic_write(pending / "manifest.json", json.dumps({"version": version, "commit": commit, "edition": "oss", "snapshot": snapshot, "notes": hashlib.sha256(notes.encode()).hexdigest(), "assets": assets}, indent=2) + "\n")
        pending.rename(stage)
    return stage


def publish(version: str, out: Path, edition: Path, directory: Path | None) -> None:
    if edition != ROOT or json.loads((edition / "edition.json").read_text())["id"] != "oss":
        raise ReleaseError("public GitHub releases require the OSS edition root")
    tag = f"v{version}"

    page = page_for(version)
    if not page.is_file() or current_version() != version:
        raise ReleaseError(f"commit version {version} and reviewed notes {page} first")
    commit = git("rev-parse", "HEAD")
    stage = out / "releases" / tag
    dirty = git("status", "--porcelain", "--untracked-files=normal")
    if dirty:
        allowed = {"nix/axe-releases.json", "README.md", "web/content/changelog.md", str(page.relative_to(ROOT))}
        if not stage.joinpath("manifest.json").exists() or any(line[3:] not in allowed for line in dirty.splitlines()):
            raise ReleaseError("checkout has changes unrelated to this staged release")

        metadata = stage / "axe-releases.json"
        if metadata.exists() and (edition / "nix" / "axe-releases.json").read_text() != metadata.read_text():
            raise ReleaseError("release metadata changed independently of staged publication")
    if not stage.joinpath("manifest.json").exists():
        clean()
    if directory is None:
        verify_remote_source(commit)

    local_tag = git("rev-parse", "-q", "--verify", f"refs/tags/{tag}^{{}}") if git("tag", "--list", tag) else None
    if local_tag is not None and local_tag != commit:
        raise ReleaseError(f"existing tag {tag} points to another commit")
    if directory is None:
        remote = remote_tag(tag)
        if remote is not None and remote != commit:
            raise ReleaseError(f"remote tag {tag} points to another commit")
        if remote is not None and local_tag is None:
            raise ReleaseError(f"fetch the existing remote tag {tag} before retrying")

    if not stage.joinpath("manifest.json").exists() and (
        local_tag is not None or (directory is None and remote is not None)
    ):
        raise ReleaseError(f"tag {tag} already exists; restore the original dist/releases/{tag} before retrying")

    stage = stage_assets(version, commit, out, edition, page)
    if directory is not None:
        release_cli(edition, stage, "immutable", directory)
        release_cli(edition, stage, "stable", directory)
        print(f"Verified local directory release {tag} at {directory}; no tag, GitHub, or S3 changes")
        return

    if local_tag is None:
        run("git", "tag", "-a", tag, "-m", f"AXE {tag}")
    if remote_tag(tag) is None:
        run("git", "push", "origin", f"refs/tags/{tag}")

    release = gh_release(tag)
    if release is None:
        run("gh", "release", "create", tag, "--verify-tag", "--draft", "--title", tag, "--notes-file", str(stage / "notes.md"))
        release = gh_release(tag)
    if release is None or release["body"].strip() != (stage / "notes.md").read_text().strip():
        raise ReleaseError("GitHub release body differs from reviewed notes")

    release_cli(edition, stage, "immutable", None)
    records = json.loads((stage / "axe-releases.json").read_text())
    assets = json.loads((stage / "manifest.json").read_text())["assets"]
    baseline = subprocess.run(("git", "show", "HEAD:README.md"), cwd=ROOT, check=True, capture_output=True, text=True).stdout
    readme = render_downloads(baseline, tag, records, assets)
    if dirty and "README.md" in {line[3:] for line in dirty.splitlines()} and (ROOT / "README.md").read_text() != readme:
        raise ReleaseError("README changes are not the expected staged download table")
    verify_public_objects(stage)

    expected = json.loads((stage / "manifest.json").read_text())["assets"]
    for name, digest in expected.items():
        if name not in {asset["name"] for asset in release["assets"]}:
            run("gh", "release", "upload", tag, str(stage / name))
        with tempfile.TemporaryDirectory(prefix="axe-gh-asset-") as temporary:
            run("gh", "release", "download", tag, "--pattern", name, "--dir", temporary)
            if sha256(Path(temporary) / name) != digest:
                raise ReleaseError(f"GitHub release asset differs from staged binary: {name}")

    if release["isDraft"]:
        run("gh", "release", "edit", tag, "--draft=false")
    release_cli(edition, stage, "stable", None)
    verify_public_objects(stage, stable=True)

    data = (stage / "axe-releases.json").read_text()
    records = json.loads(data)
    if set(records) != set(TARGETS) or any(item["version"] != version for item in records.values()):
        raise ReleaseError("release metadata missing a target or has mismatched versions")

    atomic_write(edition / "nix" / "axe-releases.json", data)
    changelog = ROOT / "web" / "content" / "changelog.md"
    atomic_write(changelog, changelog_with_release(changelog.read_text(), tag, datetime.now(timezone.utc).date().isoformat(), release_notes(page)))
    atomic_write(ROOT / "README.md", readme)
    print(f"Published {tag}; review/commit nix/axe-releases.json, README.md and {changelog}, then push main for website deployment.")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    subcommands = parser.add_subparsers(dest="command", required=True)
    prepare_parser = subcommands.add_parser("prepare")
    prepare_parser.add_argument("bump", choices=("initial", "major", "minor", "patch"))
    publish_parser = subcommands.add_parser("publish")
    publish_parser.add_argument("--edition-root", type=Path, default=ROOT)
    publish_parser.add_argument("--output", type=Path, default=ROOT / "dist")

    publish_parser.add_argument("--local-directory", type=Path, help="exercise complete local publication without GitHub, S3, or tags")
    arguments = parser.parse_args()

    if arguments.command == "prepare":
        prepare(arguments.bump)
    else:
        publish(current_version(), arguments.output.resolve(), arguments.edition_root.resolve(), arguments.local_directory.resolve() if arguments.local_directory else None)


if __name__ == "__main__":
    try:
        main()
    except (ReleaseError, OSError, subprocess.CalledProcessError) as error:
        print(f"release: {error}", file=sys.stderr)
        sys.exit(1)
