#!/usr/bin/env python3
"""Puts the in-app installer on the connected iPhone or iPad, without DFU.

One app: the pinned Dopamine with the installer in it. resize-apfs does the work, from its
checkout (RESIZE_APFS, default ../../resize-apfs-engine): scripts/embed.py puts the installer
(out/device/access, from build.sh) into the Dopamine app, and its Dopamine route signs the app
with the user's Apple ID and installs it. On the device the app gets storage access itself,
starts its helper and opens again; nothing is copied over the cable afterwards.
"""
import argparse
import os
import pathlib
import shutil
import sys
import tempfile

ROOT = pathlib.Path(__file__).resolve().parent
RESIZE_APFS = pathlib.Path(os.environ.get('RESIZE_APFS', ROOT.parents[1] / 'resize-apfs-engine'))
sys.path.insert(0, str(RESIZE_APFS / 'scripts'))

import dopamine_route  # noqa: E402
import sideload  # noqa: E402
import tools as pinned  # noqa: E402
from image_format import require  # noqa: E402

PAYLOAD = ROOT / 'out/device/access'


def arguments(index, allow_unsupported):
    """The helper's arguments the developer asked for, one a line: the app passes them on
    when it starts its helper."""
    if index:
        require(index.startswith('https://') and not any(c in index for c in ' \'"\\;&|$`<>\n'), 'The index must be a plain https:// URL.')
    return (['--index', index] if index else []) + (['--allow-unsupported'] if allow_unsupported else [])


def main():
    parser = argparse.ArgumentParser(description='Install the in-app installer on a device, without DFU.')
    parser.add_argument('--index', help='the release index to install from (default: the newest Omarchy A)')
    parser.add_argument('--allow-unsupported', action='store_true',
                        help='install on a board the release does not cover yet, with the kernel of its chip (no device tree)')
    args = parser.parse_args()
    asked = arguments(args.index, args.allow_unsupported)
    require((PAYLOAD / 'OrchardInstaller.dylib').is_file(), 'The installer is not built yet (out/device/access). Build it with ./build.sh.')
    sideload.APP = 'Orchard Installer'
    with tempfile.TemporaryDirectory(prefix='orchard-installer-') as work:
        payload = pathlib.Path(work) / 'access'
        shutil.copytree(PAYLOAD, payload)
        if asked:
            (payload / 'Resources/helper-arguments.txt').write_text('\n'.join(asked) + '\n')
        dopamine_route.run(pinned.provide('dopamine'), payload=payload, work=pathlib.Path(work) / 'sideload')


if __name__ == '__main__':
    try:
        main()
    except KeyboardInterrupt:
        raise SystemExit('\nCancelled.')
    except (ValueError, OSError, KeyError, EOFError) as error:
        raise SystemExit(str(error))
