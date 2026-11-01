"""Independent RabbitMQ publish/consume bridge, including multi-frame bodies."""
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

compiler, repo = map(Path, sys.argv[1:])
for repeat in range(15):
    with tempfile.TemporaryDirectory(prefix='tinox-amqp-bytes-') as directory:
        work = Path(directory) / 'work'
        shutil.copytree(repo / 'benchmarks/ai/tasks/broker/starter', work)
        shutil.copyfile(repo / 'benchmarks/ai/checks/controls/broker/Processor.tnx', work / 'Processor.tnx')
        result = subprocess.run([sys.executable, repo / 'benchmarks/ai/checks/accept.py', 'broker-bytes', work, compiler],
                                capture_output=True, text=True, timeout=120)
        assert result.returncode == 0, result.stderr
        print('broker compact trial', repeat + 1, 'passed', flush=True)
