"""Independent HTTP lifecycle regressions for the measurement client."""
import http.server
import json
from pathlib import Path
import subprocess
import sys
import threading
import time
import unittest


class CloseServer(http.server.BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'
    connection_header = 'close'
    idle_delay = 0

    def do_GET(self):
        self.send_response(200)
        self.send_header('Content-Length', '2')
        self.send_header('Connection', self.connection_header)
        self.end_headers()
        self.wfile.write(b'ok')
        time.sleep(self.idle_delay)
        self.close_connection = True

    def log_message(self, *args):
        pass


class IdleCloseServer(CloseServer):
    connection_header = 'keep-alive'
    idle_delay = .02


class ClientLifecycle(unittest.TestCase):
    def test_expected_connection_close_is_not_a_failed_request(self):
        self.exercise(CloseServer)

    def test_idle_keepalive_close_is_detected_before_sending(self):
        self.exercise(IdleCloseServer)

    def exercise(self, handler):
        server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            result = subprocess.run([
                sys.executable, str(Path(__file__).with_name('loadgen.py')),
                '--port', str(server.server_port), '--path', '/small',
                '--duration', '2', '--rate', '20', '--conns', '2', '--procs', '1'],
                capture_output=True, text=True, timeout=10, check=True)
            data = json.loads(result.stdout)
            self.assertEqual(data['offered'], 40)
            self.assertEqual(data['ok'], 40)
            self.assertEqual(data['conn_error'] + data['timeout'] + data['http_error'] + data['expired'], 0)
        finally:
            server.shutdown()
            server.server_close()
            thread.join()


if __name__ == '__main__':
    unittest.main()
