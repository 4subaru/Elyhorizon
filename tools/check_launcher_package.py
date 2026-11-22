#!/usr/bin/env python3
"""Offline smoke check of installed programs, using only fresh disposable player homes."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile

if __package__:
    from . import lab
else:
    import lab

HOMES = ("HORIZON_HOME", "HORIZON_LAUNCHER_HOME", "XDG_DATA_HOME", "XDG_CONFIG_HOME",
         "HORIZON_LAUNCHER_TRASH", "HORIZON_LAUNCHER_CLIPBOARD", "HORIZON_LAUNCHER_DESKTOP")


def check(launcher, client, client_id, appimage=False, require_completion_marker=False,
          require_wasm_std=False, run=subprocess.run):
    launcher, client = Path(launcher).resolve(), Path(client).resolve()
    if not client_id or not client_id.strip():
        raise ValueError("Expected public Client ID is missing")
    if require_completion_marker:
        # A package that ships the game beside the launcher starts it in that folder, and the
        # run holder opens that folder's completion marker before it starts it (horizon-mod-package
        # holder::lease_paths). An empty regular file, never a link: without it a play from the
        # installed package is refused.
        marker = client.parent / ".installed"
        if marker.is_symlink() or not marker.is_file() or marker.stat().st_size:
            raise ValueError(f"The game's folder has no empty completion marker: {marker}")
    if require_wasm_std:
        # Wasm mods are members of one group linked at load, against the shared standard library
        # the game reads beside its own executable (horizon-mod-wasm, `current_exe()`). Without it
        # every wasm mod is refused: "this game has no wasm standard library member".
        std = client.parent / "horizon-wasm-std.wasm"
        if std.is_symlink() or not std.is_file() or not std.stat().st_size:
            raise ValueError(f"The game's folder has no wasm standard library: {std}")
    with tempfile.TemporaryDirectory(prefix="elyhorizon-package-") as scratch:
        env = dict(os.environ)
        for key in list(env):
            if key.startswith("HORIZON_"):
                env.pop(key)
        for key in HOMES:
            directory = Path(scratch, key)
            directory.mkdir()
            env[key] = str(directory)
        env = lab.game_env(env, HORIZON_TELEMETRY_URL="off")
        prefix = [str(launcher)] + (["--appimage-extract-and-run"] if appimage else [])
        def call(args):
            result = run(args, cwd=scratch, env=env, capture_output=True, text=True, timeout=60, check=False)
            if result.returncode:
                # Do not echo arbitrary stderr, which might contain provider data.
                raise ValueError(f"Packaged program failed its offline smoke check (exit {result.returncode})")
            return result.stdout
        config = json.loads(call(prefix + ["--json", "account", "ely-status"]))
        if not (config.get("client_id") == client_id and config.get("bundled_client_id") == client_id
                and config.get("flow") == "authorization_code_pkce" and config.get("code_challenge_method") == "S256"
                and config.get("client_secret_required") is False):
            raise ValueError("Installed launcher lacks the expected bundled public S256 PKCE configuration")
        for _ in range(2):
            accounts = json.loads(call(prefix + ["--json", "account", "list"]))
            if accounts.get("accounts") != [] or accounts.get("can_play") is not False:
                raise ValueError("Fresh launcher home unexpectedly contains accounts or remembered authorization")
        if not call(prefix + ["--version"]).startswith("horizon-launcher "):
            raise ValueError("Packaged launcher returned no version")
        call([str(client), "--game-dir", env["HORIZON_HOME"], "--help"])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--launcher", required=True)
    parser.add_argument("--client", required=True)
    parser.add_argument("--client-id", required=True)
    parser.add_argument("--appimage", action="store_true")
    parser.add_argument("--require-completion-marker", action="store_true",
                        help="require the empty .installed marker the run holder opens in the game's folder")
    parser.add_argument("--require-wasm-std", action="store_true",
                        help="require the shared wasm standard library the game reads in the game's folder")
    args = parser.parse_args()
    try:
        check(args.launcher, args.client, args.client_id, args.appimage, args.require_completion_marker,
              args.require_wasm_std)
    except (ValueError, OSError, subprocess.SubprocessError) as error:
        print(f"Package verification failed: {error}")
        return 1
    print("Installed programs passed isolated offline configuration/startup checks; interactive login and gameplay are not verified by this check.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
