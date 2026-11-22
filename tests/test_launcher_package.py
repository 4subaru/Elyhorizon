import json
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from tools import check_launcher_package as smoke


class PackageSmokeTests(unittest.TestCase):
    def run_package(self, change=None, launcher='launcher', client='client', require_marker=False, require_wasm_std=False):
        homes = []
        def run(args, **kwargs):
            env = kwargs['env']
            self.assertFalse(any(key.startswith('HORIZON_ELY_') for key in env))
            self.assertNotIn('HORIZON_MSA_CLIENT_ID', env)
            self.assertEqual(env['HORIZON_LAUNCHER_NO_SELF_UPDATE'], '1')
            self.assertTrue(all(Path(env[key]).is_dir() for key in smoke.HOMES))
            homes.append(kwargs['cwd'])
            if args[-1] == 'ely-status':
                result = dict(client_id='elyhorizondesktop', bundled_client_id='elyhorizondesktop',
                              flow='authorization_code_pkce', code_challenge_method='S256', client_secret_required=False)
                if change:
                    result.update(change)
                return SimpleNamespace(returncode=0, stdout=json.dumps(result))
            if args[-1] == 'list':
                return SimpleNamespace(returncode=0, stdout=json.dumps(dict(accounts=[], can_play=False)))
            return SimpleNamespace(returncode=0, stdout='horizon-launcher 0.2.2\n')
        smoke.check(launcher, client, 'elyhorizondesktop', require_completion_marker=require_marker,
                    require_wasm_std=require_wasm_std, run=run)
        self.assertEqual(len(homes), 5)
        self.assertEqual(len(set(homes)), 1)
        self.assertFalse(Path(homes[0]).exists())

    def test_installed_configuration_is_checked_without_runtime_credentials(self):
        self.run_package()

    def test_a_shipped_game_without_the_run_holder_marker_is_rejected(self):
        with tempfile.TemporaryDirectory() as scratch:
            folder = Path(scratch)
            client = folder / 'horizon-client'
            client.touch()
            marker = folder / '.installed'
            for missing in [None, 'not empty']:
                with self.subTest(marker=missing):
                    marker.unlink(missing_ok=True)
                    if missing:
                        marker.write_text(missing)
                    with self.assertRaisesRegex(ValueError, 'completion marker'):
                        self.run_package(client=client, require_marker=True)
            marker.unlink()
            marker.touch()
            self.run_package(client=client, require_marker=True)

    def test_a_shipped_game_without_the_wasm_standard_library_is_rejected(self):
        with tempfile.TemporaryDirectory() as scratch:
            folder = Path(scratch)
            client = folder / 'horizon-client'
            client.touch()
            library = folder / 'horizon-wasm-std.wasm'
            for missing in [None, '']:
                with self.subTest(library=missing):
                    library.unlink(missing_ok=True)
                    if missing is not None:
                        library.write_bytes(b'')
                    with self.assertRaisesRegex(ValueError, 'wasm standard library'):
                        self.run_package(client=client, require_wasm_std=True)
            library.write_bytes(b'\0asm\x01\x00\x00\x00')
            self.run_package(client=client, require_wasm_std=True)

    def test_missing_embedded_id_or_insecure_flow_is_rejected(self):
        for change in [dict(bundled_client_id=None), dict(client_secret_required=True), dict(code_challenge_method='plain')]:
            with self.subTest(change=change), self.assertRaisesRegex(ValueError, 'bundled public'):
                self.run_package(change)
