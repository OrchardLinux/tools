"""Starts the in-app installer on a device jailbroken with Dopamine: copies the app, its helper
and e2fsprogs in over the SSH server of Dopamine's bootstrap, through the USB cable, for one
session, and removes them when the user is done.

The session itself (Dopamine signed and installed, SSH through usbmuxd, sudo as `mobile`, the
app registered from the jailbreak root's Applications folder) is resize-apfs's Dopamine route,
used from its checkout (scripts/dopamine_route.py, checkra1n_route.py); only what is copied
and started differs. The route moves into orchard2 later.
"""
import argparse
import os
import pathlib
import sys
import tempfile
import time

ROOT = pathlib.Path(__file__).resolve().parent
RESIZE_APFS = pathlib.Path(os.environ.get('RESIZE_APFS', ROOT.parents[1] / 'resize-apfs-engine'))
sys.path.insert(0, str(RESIZE_APFS / 'scripts'))

import checkra1n_route  # noqa: E402
import dopamine_route  # noqa: E402
import tools as pinned  # noqa: E402
from image_format import require  # noqa: E402

PAYLOAD = ROOT / 'out/device'
# Root-owned, on the data partition, as resize-apfs's session folder.
SESSION = '/private/var/OrchardInstaller'
HELPER = SESSION + '/orchard-installer-helper'
PIDFILE = SESSION + '/helper.pid'
HELPER_LOG = SESSION + '/helper.log'
APP = 'OrchardInstaller.app'
EXECUTABLE = 'OrchardInstaller'
E2FSPROGS = ('tune2fs', 'resize2fs', 'debugfs', 'e2fsck')
PROMPT = 'Orchard Installer is on the Home Screen. When you are done, type done and press Enter to take it off the device: '


def payload():
    app, helper, tools = PAYLOAD / APP, PAYLOAD / 'orchard-installer-helper', PAYLOAD / 'e2fsprogs'
    require(app.is_dir() and helper.is_file() and all((tools / name).is_file() for name in E2FSPROGS),
            'The installer is not built yet (out/device). Build it with ./build.sh.')
    return app, helper, tools


def leftover_script(app_path):
    return (f'if [ -f {PIDFILE} ]; then kill "$(cat {PIDFILE})" 2>/dev/null; rm -f {PIDFILE}; fi; '
            f'if [ -x {HELPER} ] && [ -d {app_path} ]; then {HELPER} --unregister-app {app_path}; fi; '
            f'rm -rf {app_path} {SESSION}')


def install(session, app, helper, tools, app_path):
    session.sh(leftover_script(app_path), 'Removing an earlier session')
    # 755: the app runs as mobile and must read its bundle; the helper trusts it only while
    # nobody but root can change it.
    session.sh(f'umask 022; mkdir -p {SESSION}/e2fsprogs && chmod 755 {SESSION} {SESSION}/e2fsprogs', 'Preparing the session folder')
    session.put_file(helper, HELPER, 0o755)
    for name in E2FSPROGS:
        session.put_file(tools / name, f'{SESSION}/e2fsprogs/{name}', 0o755)
    session.put_tree(app, app_path)
    session.sh(f'{HELPER} --register-app {app_path}', 'Adding the app to the home screen')


def start_helper(session, app_path, index, allow_unsupported):
    """The helper as root, detached from the SSH connection: a dropped connection must not
    take the helper, and a running resize or install, with it."""
    options = (f' --index {index}' if index else '') + (' --allow-unsupported' if allow_unsupported else '')
    session.sh(f"cd /; trap '' HUP; {HELPER} --ui-path {app_path}/{EXECUTABLE}{options} "
               f'</dev/null >{HELPER_LOG} 2>&1 & echo $! > {PIDFILE}', 'Starting the helper')


def busy(session):
    """True while a resize or an install runs, False when the helper clearly says none does,
    None when the device could not be asked."""
    try:
        code = session.sh(f'{HELPER} --busy', 'Asking the helper', check=False, timeout=45).returncode
    except Exception:
        return None
    return {0: True, 1: False}.get(code)


def leave(session, app_path, ask=input):
    """After the user types `done`: stop the helper and remove everything, never while a
    resize or an install runs and never on silence."""
    while True:
        while (ask(PROMPT) or '').strip().lower() != 'done':
            print('Orchard Installer stays on the device.', flush=True)
        state = busy(session)
        while state is None:
            print('Waiting for the device to answer. Unlock it if it is locked.', flush=True)
            time.sleep(3)
            state = busy(session)
        if state:
            print('The installer is still working. Wait until the app shows it has finished.')
            continue
        session.sh(leftover_script(app_path), 'Removing the app', timeout=120)
        print('Removed.')
        return


def main():
    parser = argparse.ArgumentParser(description='Start the in-app installer on a device jailbroken with Dopamine.')
    parser.add_argument('--index', help='the release index to install from (default: the newest Omarchy A)')
    parser.add_argument('--allow-unsupported', action='store_true',
                        help='install on a board the release does not cover yet, with the kernel of its chip (no device tree)')
    parser.add_argument('--jailbroken', action='store_true',
                        help='the device runs Dopamine with its jailbreak active already: skip signing and installing Dopamine')
    args = parser.parse_args()
    if args.index:
        require(args.index.startswith('https://') and not any(c in args.index for c in ' \'"\;&|$`<>'), 'The index must be a plain https:// URL.')
    app, helper, tools = payload()
    if not args.jailbroken:
        dopamine_route.prepare(pinned.provide('dopamine'), input)
    checkra1n_route.wait_for_ssh(timeout=900, problem=dopamine_route.ssh_problem,
                                 waiting=f'Waiting for {dopamine_route.SERVER}...', hint=dopamine_route.HINT)
    # A short folder: the SSH control socket's path must stay under 104 bytes.
    with tempfile.TemporaryDirectory(prefix='orchard-installer-', dir='/tmp') as work:
        session = dopamine_route.connect(work)
        try:
            real = dopamine_route.app_path(session)
            app_path = real[:real.rindex('/') + 1] + APP
            install(session, app, helper, tools, app_path)
            start_helper(session, app_path, args.index, args.allow_unsupported)
            print('Orchard Installer opens on the device. If it does not, open it from the Home Screen.', flush=True)
            leave(session, app_path)
        finally:
            session.close()


if __name__ == '__main__':
    try:
        main()
    except KeyboardInterrupt:
        raise SystemExit('\nCancelled. Orchard Installer stays on the device; run this again to remove it.')
    except (ValueError, OSError, KeyError, EOFError) as error:
        raise SystemExit(str(error))
