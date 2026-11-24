#!/usr/bin/env python3
"""Stamps the generic installer for one distro: its name, wordmark, release
index and the launcher assets it installs (pinned by size and SHA-256). The
result is that distro's release artifacts, ready to upload next to its release:

  Install-<Name>-macOS.zip            "Install <Name>.app" (stamped, re-signed)
  Install-<Name>-linux-<arch>.tar.gz  install-<name> (stamped), desktop entry, icons
  Orchard-macOS.zip, Orchard-linux-<arch>.tar.gz   the launcher, as pinned
  install.sh                          curl … | sh
  SHA256SUMS

  packaging/stamp.py --generic out/generic --out out/omarchy-a \\
    --name "Omarchy A" --wordmark packaging/omarchy-a-wordmark.svg \\
    --release https://github.com/OrchardLinux/omarchy-a/releases/download/v0.3.3

--release is where these files will be downloadable (the launcher pins point
there); the index is <release>/../../latest/download/index.toml unless --index
says otherwise, so the installer always installs the newest release.

macOS stamping re-signs ad hoc with codesign (on a Mac) or rcodesign.
"""
import argparse, hashlib, json, os, plistlib, shutil, struct, subprocess, sys, tarfile, tempfile, zipfile
from pathlib import Path

MAGIC = b"ORCHARD-STAMP-1\0"
HERE = Path(__file__).resolve().parent
ARCHES = ["x86_64", "aarch64"]


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def pin(path, base):
    return {"url": f"{base}/{path.name}", "bytes": path.stat().st_size, "sha256": sha256(path)}


def run(*args, **kw):
    subprocess.run(args, check=True, **kw)


def resign(app):
    if shutil.which("codesign"):
        run("codesign", "--force", "--sign", "-", "--timestamp=none", str(app))
    elif shutil.which("rcodesign"):
        run("rcodesign", "sign", str(app))
    else:
        sys.exit("stamp.py: macOS stamping needs codesign (a Mac) or rcodesign to re-sign the app")


def stamp_macos(generic, out, stamp, name, slug):
    src = generic / "Orchard-Installer-macOS.zip"
    if not src.exists():
        print(f"skip macOS: no {src}")
        return
    with tempfile.TemporaryDirectory() as tmp:
        tmp = Path(tmp)
        run("ditto", "-x", "-k", str(src), str(tmp)) if shutil.which("ditto") else zipfile.ZipFile(src).extractall(tmp)
        app = tmp / f"Install {name}.app"
        (tmp / "Orchard Installer.app").rename(app)
        exe = app / "Contents/MacOS/orchard-installer"
        exe.chmod(0o755)  # zipfile drops modes
        info = app / "Contents/Info.plist"
        plist = plistlib.loads(info.read_bytes())
        plist.update(CFBundleName=f"Install {name}", CFBundleDisplayName=f"Install {name}", CFBundleIdentifier=f"dev.orchard.install-{slug.lower()}")
        info.write_bytes(plistlib.dumps(plist))
        (app / "Contents/Resources/stamp.json").write_text(json.dumps(stamp))
        resign(app)
        dest = out / f"Install-{slug}-macOS.zip"
        dest.unlink(missing_ok=True)
        if shutil.which("ditto"):
            run("ditto", "-c", "-k", "--sequesterRsrc", "--keepParent", str(app), str(dest))
        else:
            run("sh", "-c", f'cd "{tmp}" && zip -qry "{dest.resolve()}" "{app.name}"')
    print(f"stamped {dest.name}")


def stamp_linux(generic, out, stamp, name, slug, arch):
    src = generic / f"orchard-installer-linux-{arch}"
    if not src.exists():
        print(f"skip linux-{arch}: no {src}")
        return
    exe_name = f"install-{slug.lower()}"
    with tempfile.TemporaryDirectory() as tmp:
        tmp = Path(tmp)
        exe = tmp / exe_name
        payload = json.dumps(stamp).encode()
        exe.write_bytes(src.read_bytes() + payload + struct.pack("<Q", len(payload)) + MAGIC)
        exe.chmod(0o755)
        (tmp / "applications").mkdir()
        (tmp / "applications" / f"{exe_name}.desktop").write_text(
            "[Desktop Entry]\nType=Application\n"
            f"Name=Install {name}\nComment=Install {name} on your iPad or iPhone\n"
            f"Exec={exe_name}\nIcon=orchard\nCategories=System;\nTerminal=false\n"
        )
        (tmp / "icons").mkdir()
        shutil.copytree(HERE / "icons/hicolor", tmp / "icons/hicolor")
        dest = out / f"Install-{slug}-linux-{arch}.tar.gz"
        with tarfile.open(dest, "w:gz") as tar:
            for entry in sorted(tmp.iterdir()):
                tar.add(entry, arcname=entry.name)
    print(f"stamped {dest.name}")


def install_sh(out, name, slug, release, cli_script):
    """install.sh downloads from the release it is published in (also a
    pre-release); --cli hands over to the distribution's own CLI installer."""
    text = (HERE / "install.sh").read_text()
    text = text.replace("@NAME@", name).replace("@SLUG@", slug).replace("@SLUG_LOWER@", slug.lower()).replace("@BASE@", release)
    text = text.replace("@CLI@", cli_script)
    (out / "install.sh").write_text(text)
    (out / "install.sh").chmod(0o755)


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--generic", type=Path, required=True)
    p.add_argument("--out", type=Path, required=True)
    p.add_argument("--name", required=True)
    p.add_argument("--wordmark", type=Path)
    p.add_argument("--release", required=True, help="where these files will be downloadable (the versioned release)")
    p.add_argument("--cli-script", help="the distribution's command-line installer script for install.sh --cli "
                   "(default: install-cli.sh in the distribution's latest release)")
    p.add_argument("--index", help="the release index (default: the latest release's index.toml)")
    a = p.parse_args()
    release = a.release.rstrip("/")
    # …/releases/download/<tag> → …/releases/latest/download
    latest = release.rsplit("/download/", 1)[0] + "/latest/download" if "/download/" in release else release
    slug = a.name.replace(" ", "-")
    a.out.mkdir(parents=True, exist_ok=True)

    version = (a.generic / "VERSION").read_text().strip()
    launcher = {}
    for key, file in [("macos", "Orchard-macOS.zip")] + [(f"linux-{arch}", f"Orchard-linux-{arch}.tar.gz") for arch in ARCHES]:
        src = a.generic / file
        if src.exists():
            shutil.copy2(src, a.out / file)
            launcher[key] = dict(pin(a.out / file, release), version=version)
    stamp = {
        "name": a.name,
        "wordmark": a.wordmark.read_text() if a.wordmark else None,
        "index": a.index or f"{latest}/index.toml",
        "launcher": launcher,
    }
    stamp_macos(a.generic, a.out, stamp, a.name, slug)
    for arch in ARCHES:
        stamp_linux(a.generic, a.out, stamp, a.name, slug, arch)
    cli_script = a.cli_script or f"{latest}/install-cli.sh"
    install_sh(a.out, a.name, slug, release, cli_script)
    files = sorted(f for f in a.out.iterdir() if f.is_file() and f.name != "SHA256SUMS")
    (a.out / "SHA256SUMS").write_text("".join(f"{sha256(f)}  {f.name}\n" for f in files))
    print(f"\n{a.out}:")
    for f in files:
        print(f"  {f.name}  {f.stat().st_size / 1e6:.1f} MB")
    print(f"index: {stamp['index']}")
    print(f"orchard: {version}")


if __name__ == "__main__":
    main()
