"""Create a native NDM Next plugin package and SHA-256 sidecar."""
import argparse
import hashlib
import io
import json
from pathlib import Path
import shutil
import tempfile
import tomllib
import urllib.request
from zipfile import ZIP_DEFLATED, ZipFile

ROOT = Path(__file__).resolve().parents[1]
PLATFORMS = ("windows-x86_64", "macos-x86_64", "macos-aarch64", "linux-x86_64")


def fetch(url, digest=None):
    with urllib.request.urlopen(url, timeout=90) as response:
        content = response.read(240 * 1024 * 1024 + 1)
    if len(content) > 240 * 1024 * 1024:
        raise ValueError("runtime asset exceeds the package limit")
    if digest and hashlib.sha256(content).hexdigest() != digest:
        raise ValueError(f"checksum mismatch for {url}")
    return content


def add_runtimes(stage, platform, suffix):
    lock = ROOT / "runtime-lock.json"
    if not lock.exists():
        return
    assets = json.loads(lock.read_text())
    runtime = stage / "runtime"
    runtime.mkdir()
    notices = stage / "notices"
    notices.mkdir()
    for tool, specification in assets.items():
        asset = specification["platforms"][platform]
        content = fetch(
            f"https://github.com/{specification['repository']}/releases/download/{specification['version']}/{asset['name']}",
            asset["sha256"],
        )
        target = runtime / (tool + suffix)
        if asset["name"].endswith(".zip"):
            with ZipFile(io.BytesIO(content)) as archive:
                with archive.open(tool + suffix) as bundled, target.open("wb") as destination:
                    shutil.copyfileobj(bundled, destination)
        else:
            target.write_bytes(content)
        target.chmod(0o755)
        for license_name in specification["licenses"]:
            license_url = f"https://raw.githubusercontent.com/{specification['repository']}/{specification['version']}/{license_name}"
            (notices / f"{tool}-{license_name}").write_bytes(fetch(license_url))


def package(binary, platform, destination, audit):
    manifest = json.loads((ROOT / "plugin.json").read_text(encoding="utf-8"))
    version = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))["package"]["version"]
    if manifest["version"] != version:
        raise ValueError("plugin.json and Cargo.toml versions differ")
    suffix = ".exe" if platform.startswith("windows") else ""
    manifest["engine"] += suffix
    destination.mkdir(parents=True, exist_ok=True)
    name = f"ndm-{manifest['id']}-{version}-{platform}.zip"
    output = destination / name

    with tempfile.TemporaryDirectory() as scratch:
        stage = Path(scratch)
        (stage / "plugin.json").write_text(
            json.dumps(manifest, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
        )
        shutil.copyfile(binary, stage / manifest["engine"])
        (stage / manifest["engine"]).chmod(0o755)
        for filename in ("panel.js", "README.md", "LICENSE"):
            shutil.copyfile(ROOT / filename, stage / filename)
        if audit:
            shutil.copyfile(audit, stage / "THIRD-PARTY-LICENSES.html")
        add_runtimes(stage, platform, suffix)
        with ZipFile(output, "w", ZIP_DEFLATED, compresslevel=6) as archive:
            for path in sorted(stage.rglob("*")):
                if path.is_file():
                    archive.write(path, path.relative_to(stage).as_posix())
    with output.open("rb") as payload:
        digest = hashlib.file_digest(payload, "sha256").hexdigest()
    (destination / f"{name}.sha256").write_text(f"{digest}  {name}\n", encoding="ascii")
    print(output)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--platform", required=True, choices=PLATFORMS)
    parser.add_argument("--output", default="dist", type=Path)
    parser.add_argument("--audit", type=Path)
    args = parser.parse_args()
    package(args.binary, args.platform, args.output, args.audit)
