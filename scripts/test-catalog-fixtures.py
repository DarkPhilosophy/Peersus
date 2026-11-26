#!/usr/bin/env python3
"""Exercise the public Linux catalog payload through the real native executor."""
import json
import os
from pathlib import Path
import platform
import socket
import struct
import subprocess
import sys
import tarfile
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
FIXTURES = ROOT / 'crates/peersus-app/tests/fixtures'


def receive(stream):
    def exact(count):
        result = bytearray()
        while len(result) < count:
            block = stream.recv(count - len(result))
            if not block:
                raise AssertionError('Catalog payload exited before replying to a real call')
            result.extend(block)
        return bytes(result)
    size, = struct.unpack('!I', exact(4))
    if not 0 < size <= 65536:
        raise AssertionError('Invalid control frame')
    return json.loads(exact(size))


@unittest.skipUnless(sys.platform.startswith('linux') and platform.machine() == 'x86_64',
                     'Committed demo payload targets Linux x86_64')
class CatalogRuntime(unittest.TestCase):
    def test_both_catalog_versions_reply_and_remain_available(self):
        with tempfile.TemporaryDirectory(prefix='catalog-runtime-') as temporary:
            directory = Path(temporary)
            helper = directory / 'native-host'
            subprocess.run([os.environ.get('CC', 'cc'), '-std=c11', '-O2',
                            str(ROOT / 'crates/peersus-plugin-runtime/native/host.c'),
                            '-ldl', '-o', str(helper)], check=True, timeout=30)
            for version in ('catalog', 'catalog-v2'):
                with self.subTest(version=version):
                    module = directory / 'echo.so'
                    with tarfile.open(FIXTURES / version / 'echo.tar.gz') as archive:
                        module.write_bytes(archive.extractfile('echo/echo.so').read())
                    address = directory / f'{version}.sock'
                    with socket.socket(socket.AF_UNIX) as listener:
                        listener.bind(str(address))
                        listener.listen(1)
                        listener.settimeout(5)
                        environment = os.environ.copy()
                        environment.update(PEERSUS_PLUGIN_SOCKET=str(address),
                                           PEERSUS_PLUGIN_DATA_SOCKET=str(directory / 'unused.sock'),
                                           PEERSUS_PLUGIN_TOKEN='0' * 64)
                        process = subprocess.Popen([str(helper), str(module)], env=environment,
                                                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                        try:
                            stream, _ = listener.accept()
                            with stream:
                                stream.settimeout(5)
                                self.assertEqual(receive(stream), {'token': '0' * 64})
                                for ticket in (17, 28):
                                    request = json.dumps({'type': 'call', 'ticket': ticket,
                                                          'service': 'echo', 'args': None},
                                                         separators=(',', ':')).encode()
                                    stream.sendall(struct.pack('!I', len(request)) + request)
                                    self.assertEqual(receive(stream), {
                                        'op': 'reply', 'ticket': ticket,
                                        'result': {'status': 'ok', 'value': 'portable echo'}})
                                    self.assertIsNone(process.poll())
                            self.assertEqual(process.wait(timeout=5), 0)
                        finally:
                            if process.poll() is None:
                                process.terminate()
                                try:
                                    process.wait(timeout=5)
                                except subprocess.TimeoutExpired:
                                    process.kill()
                                    process.wait()
                    address.unlink()


if __name__ == '__main__':
    unittest.main()
