"""Selected VM stdio transport. The VM owns admission, values and enforcement."""
from __future__ import annotations
import hashlib
import json
import os
import selectors
import subprocess
import tempfile
import threading
import time
from pathlib import Path

SCHEMA = 'mncs.vm.session/1'
MAX_FRAME = 16 * 1024 * 1024


def sha(path):
    h = hashlib.sha256()
    with Path(path).open('rb') as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b''):
            h.update(block)
    return h.hexdigest()


def digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(',', ':'), ensure_ascii=False).encode()).hexdigest()


def inspect_runtime(executable: Path):
    executable = Path(executable).resolve()
    result = subprocess.run([str(executable), 'describe'], capture_output=True, text=True, timeout=10, check=True)
    description = json.loads(result.stdout)
    if description.get('schema_version') != 'mncs.vm.runtime-provider/1':
        raise RuntimeError('selected executable is not a VM provider')
    return {'schema_version': 'mncs.vm.selected-runtime/1', 'executable': str(executable),
            'executable_sha256': sha(executable), 'contract': description,
            'build_origin': 'unknown; exact executable bytes observed'}


class Session:
    """One immutable artifact admission and index build, isolated calls thereafter."""
    def __init__(self, executable: Path, artifact: Path, *, timeout: float = 120, build_receipt=None):
        self.executable, self.artifact = Path(executable).resolve(), Path(artifact).resolve()
        self.timeout = timeout
        self.runtime = inspect_runtime(self.executable)
        self.artifact_sha256 = sha(self.artifact)
        self.signature = self._signature()
        if build_receipt is not None:
            if (build_receipt['identity'] != digest(build_receipt['core'])
                    or build_receipt['core']['artifact']['sha256'] != self.artifact_sha256):
                raise RuntimeError('artifact does not match compiler build receipt')
        self.build_receipt = build_receipt
        self.count = 0
        self._lock = threading.Lock()
        self._stderr = tempfile.TemporaryFile()
        self.proc = subprocess.Popen([str(self.executable), 'serve', '--artifact', str(self.artifact)],
                                     stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=self._stderr, bufsize=0)
        self._buffer = bytearray()
        try:
            self.info = self._read()
            if self.info.get('schema_version') != SCHEMA or self.info.get('status') != 'ready':
                raise RuntimeError('VM did not admit artifact')
            if build_receipt is not None and self.info.get('artifact_id') != build_receipt['core']['artifact']['identity']:
                raise RuntimeError('admitted artifact identity does not match compiler build receipt')
            if self._signature() != self.signature:
                raise RuntimeError('selected runtime or artifact changed during admission')
            if self.info.get('runtime') != self.runtime['contract']:
                raise RuntimeError('loaded VM contract differs from selected runtime')
        except BaseException:
            self.close()
            raise

    def _signature(self):
        return tuple((p.stat().st_mtime_ns, p.stat().st_ctime_ns, p.stat().st_size, p.stat().st_ino) for p in (self.executable, self.artifact))

    def _read(self):
        deadline = time.monotonic() + self.timeout
        with selectors.DefaultSelector() as selector:
            selector.register(self.proc.stdout, selectors.EVENT_READ)
            while b'\n' not in self._buffer:
                remaining = deadline - time.monotonic()
                if remaining <= 0 or not selector.select(remaining):
                    self.close()
                    raise RuntimeError('VM response timeout')
                block = os.read(self.proc.stdout.fileno(), 65536)
                if not block:
                    self._stderr.seek(0)
                    raise RuntimeError('VM transport closed: ' + self._stderr.read(4000).decode(errors='replace'))
                self._buffer.extend(block)
                if len(self._buffer) > MAX_FRAME:
                    self.close()
                    raise RuntimeError('VM response exceeds transport bound')
        line, _, remaining = self._buffer.partition(b'\n')
        self._buffer = bytearray(remaining)
        return json.loads(line)

    def call(self, request, *, envelope=None, callable_reference=None):
        with self._lock:
            if self._signature() != self.signature:
                raise RuntimeError('selected runtime or artifact changed; reopen session')
            frame = {'schema_version': SCHEMA, 'id': self.count, 'request': request}
            if callable_reference is not None:
                frame['callable_reference'] = callable_reference
            if envelope is not None:
                frame['envelope'] = envelope
            data = json.dumps(frame, separators=(',', ':')).encode() + b'\n'
            if len(data) > MAX_FRAME:
                raise RuntimeError('VM request exceeds transport bound')
            pending = memoryview(data)
            while pending:
                written = self.proc.stdin.write(pending)
                if not written: raise RuntimeError('VM request transport closed')
                pending = pending[written:]
            self.proc.stdin.flush()
            response = self._read()
            if response.get('schema_version') != SCHEMA or response.get('id') != self.count:
                raise RuntimeError('VM response correlation mismatch')
            self.count += 1
            if not response.get('ok'):
                raise RuntimeError('VM request refused: ' + response.get('reason', 'unknown'))
            result = response['result']
            if result['record']['artifact_id'] != self.info['artifact_id']:
                raise RuntimeError('VM result artifact mismatch')
            core = {'provider':'mncs-vm:vm-call', 'build_receipt':self.build_receipt['identity'] if self.build_receipt else None,
                    'artifact_identity':self.info['artifact_id'], 'artifact_sha256':self.artifact_sha256,
                    'executor_sha256':self.runtime['executable_sha256'], 'abi':self.runtime['contract'],
                    'operation':request['target']['module']+'::'+request['target']['function'],
                    'input_identity':digest({'request':request,'envelope_override':envelope,'callable_reference':callable_reference}),
                    'inventory_identity':self.build_receipt['core']['inventory_identity'] if self.build_receipt else None,
                    'subject_identity':'mncs.native-decision-input:'+digest(request.get('arguments',[])),
                    'runtime': self.runtime, 'request': request, 'envelope_override': envelope, 'result_identity': digest(result)}
            result['execution_provenance'] = {'schema_version': 'mncs.provider-execution-provenance/1', 'identity': digest(core), 'core': core}
            return result

    def close(self):
        proc = getattr(self, 'proc', None)
        if proc is not None:
            if proc.stdin:
                proc.stdin.close()
            try:
                proc.wait(timeout=2)
            except subprocess.TimeoutExpired:
                proc.terminate()
                try: proc.wait(timeout=2)
                except subprocess.TimeoutExpired:
                    proc.kill(); proc.wait()
            if proc.stdout: proc.stdout.close()
            self.proc = None
        stream = getattr(self, '_stderr', None)
        if stream is not None and not stream.closed:
            stream.close()

    def __enter__(self):
        return self

    def __exit__(self, *args):
        self.close()
