#!/usr/bin/env python3
"""Host-side FTP server that stands in for sys-ftpd when running under an emulator.

On a real Switch, RustParameterManager (RPM) writes effect-edit/transaction files to
`sd:/slight/user/debuggables/` over FTP, served by the sys-ftpd sysmodule. An emulator
cannot run that sysmodule, but its virtual SD card is just a host directory. This serves
that declared directory over FTP so RPM's
uploads land exactly where the in-emulator plugin reads `sd:/...`.

Point RPM's FTP connection (Address / User / Password) at this server. RPM uses
`ftp://<Address>/object-...` with MakeDirectory + UploadFile, so set
  Address = <host>:<port>/slight/user/debuggables
  User / Password = whatever you pass below (default: slight / slight)

Usage:
  python tools/ftp_server.py                       # serve Eden sdmc on 0.0.0.0:5000
  python tools/ftp_server.py --port 5000 --user slight --password slight
  python tools/ftp_server.py --root <sd-root>      # other emulator / custom SD root
  python tools/ftp_server.py --anonymous           # no auth (anonymous read/write)
"""

from __future__ import annotations

import argparse
import os
from pathlib import Path

from host_paths import eden_sd_directory

# Full perms: e=cwd l=list r=retr a=append d=dele f=rnfr m=mkd w=stor M=chmod T=mfmt
FULL_PERM = "elradfmwMT"


def main() -> None:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    default_root = eden_sd_directory()
    p.add_argument("--root", type=Path, default=default_root,
                   help=f"SD root to serve (default: {default_root})")
    p.add_argument("--host", default="0.0.0.0", help="Bind address (default: 0.0.0.0)")
    p.add_argument("--port", type=int, default=5000, help="FTP port (sys-ftpd default: 5000)")
    p.add_argument("--user", default="slight", help="FTP username (default: slight)")
    p.add_argument("--password", default="slight", help="FTP password (default: slight)")
    p.add_argument("--anonymous", action="store_true",
                   help="Allow anonymous read/write instead of user/password")
    p.add_argument("--passive-ports", default="60000-60100",
                   help="Passive port range (default: 60000-60100)")
    args = p.parse_args()

    try:
        from pyftpdlib.authorizers import DummyAuthorizer
        from pyftpdlib.handlers import FTPHandler
        from pyftpdlib.servers import FTPServer
    except ImportError as error:
        raise SystemExit(
            "pyftpdlib is required; install it with `python -m pip install pyftpdlib`"
        ) from error

    root: Path = args.root.expanduser()
    if not root.is_dir():
        raise SystemExit(f"SD root does not exist: {root}\n"
                         f"Pass --root pointing at your emulator's sdmc directory.")
    # Make sure the path RPM writes into exists so MakeDirectory/UploadFile succeed.
    (root / "slight/user/debuggables").mkdir(parents=True, exist_ok=True)

    authorizer = DummyAuthorizer()
    if args.anonymous:
        authorizer.add_anonymous(str(root), perm=FULL_PERM)
        login = "anonymous (no password)"
    else:
        authorizer.add_user(args.user, args.password, str(root), perm=FULL_PERM)
        login = f"{args.user} / {args.password}"

    handler = FTPHandler
    handler.authorizer = authorizer
    handler.banner = "SLight emulator FTP bridge (sys-ftpd stand-in)"
    lo, hi = (int(x) for x in args.passive_ports.split("-"))
    handler.passive_ports = range(lo, hi + 1)

    server = FTPServer((args.host, args.port), handler)
    print(f"SLight FTP bridge serving {root}")
    print(f"  listening   : ftp://{args.host}:{args.port}")
    print(f"  login       : {login}")
    print(f"  RPM Address : <this-host>:{args.port}/slight/user/debuggables")
    print(f"  uploads ->  : {root / 'slight/user/debuggables'}")
    print("Ctrl-C to stop.")
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        print("\nstopped.")


if __name__ == "__main__":
    if os.name != "nt":
        os.umask(0o022)
    main()
