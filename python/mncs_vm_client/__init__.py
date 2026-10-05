"""Selected VM stdio transport. The VM owns admission, values and enforcement."""
from __future__ import annotations
import hashlib
import json
import os
import selectors
import shutil
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


def validate_toolchain_executables(configuration):
    tools = configuration.get('toolchain_executables') if isinstance(configuration, dict) else None
    if not isinstance(tools, dict) or set(tools) != {'cargo', 'rustc'}:
        return ['toolchain-executable-identities']
    mismatches = []
    for name, identity in tools.items():
        try:
            if not isinstance(identity, dict):
                raise ValueError('invalid identity')
            configured_path = identity['configured_path']
            configured = Path(configured_path)
            if not configured.is_absolute() and configured.parent == Path('.'):
                configured = Path(shutil.which(configured_path) or configured_path)
            configured = configured.resolve(strict=True)
            resolved = Path(identity['resolved_path']).resolve(strict=True)
            if configured != resolved or sha(resolved) != identity['sha256']:
                mismatches.append('build-tool:' + name)
        except (OSError, KeyError, TypeError, ValueError):
            mismatches.append('build-tool:' + name)
    return mismatches


def inspect_runtime(executable: Path):
    executable = Path(executable).resolve()
    result = subprocess.run([str(executable), 'describe'], capture_output=True, text=True, timeout=10, check=True)
    description = json.loads(result.stdout)
    if description.get('schema_version') != 'mncs.vm.runtime-provider/1':
        raise RuntimeError('selected executable is not a VM provider')
    executable_sha256 = sha(executable)
    embedded = description.get('build_origin')
    origin = {'status': 'unknown', 'assurance': 'no embedded provider build receipt',
              'executable_sha256': executable_sha256}
    if isinstance(embedded, dict) and isinstance(embedded.get('receipt'), dict):
        receipt = embedded['receipt']
        inputs = receipt.get('source_inputs')
        closures = receipt.get('dependency_closure')
        mismatches = []
        if (receipt.get('schema_version') != 'mncs.vm-build-receipt/1'
                or embedded.get('identity') != 'sha256:' + digest(receipt)
                or not isinstance(inputs, dict) or not isinstance(closures, list)):
            mismatches.append('receipt-integrity')
        else:
            mismatches.extend(validate_toolchain_executables(receipt.get('build_configuration')))
            roots = [Path(item['checkout']).resolve() for item in closures
                     if isinstance(item, dict) and isinstance(item.get('checkout'), str)]
            closures_by_name = {item.get('repository'): item for item in closures if isinstance(item, dict)}
            for name, closure in closures_by_name.items():
                root = Path(closure['checkout']).resolve()
                try:
                    revision = subprocess.run(['git', '-C', str(root), 'rev-parse', 'HEAD'], capture_output=True, text=True, timeout=5, check=True).stdout.strip()
                    if revision != closure.get('revision'):
                        mismatches.append('source-revision:' + str(name))
                except (OSError, subprocess.SubprocessError):
                    mismatches.append('source-revision-unavailable:' + str(name))
            dirty_inputs = {}
            for root in roots:
                try:
                    rows = subprocess.run(['git', '-C', str(root), 'status', '--porcelain=v1', '--untracked-files=all'], capture_output=True, text=True, timeout=10, check=True).stdout.splitlines()
                except (OSError, subprocess.SubprocessError):
                    mismatches.append('dirty-checkout-unavailable:' + str(root))
                    continue
                changed = {line[3:].rsplit(' -> ', 1)[-1] for line in rows if len(line) >= 4}
                for raw_path, expected in inputs.items():
                    path = Path(raw_path).resolve()
                    if path.is_relative_to(root) and path.relative_to(root).as_posix() in changed:
                        dirty_inputs[str(path)] = expected
            if digest(dirty_inputs) != receipt.get('dirty_content_identity') or len(dirty_inputs) != receipt.get('dirty_input_count'):
                mismatches.append('dirty-checkout-identity')
            for raw_path, expected in inputs.items():
                try:
                    path = Path(raw_path).resolve(strict=True)
                    if not any(path.is_relative_to(root) for root in roots):
                        mismatches.append('input-outside-selected-closure')
                        continue
                    if sha(path) != expected:
                        mismatches.append(raw_path)
                except (OSError, ValueError, TypeError):
                    mismatches.append(raw_path)
            input_identity = digest(inputs)
            if input_identity != receipt.get('source_inputs_identity'):
                mismatches.append('source-input-identity')
        receipt_identity = embedded.get('identity')
        origin = {
            'status': 'stale-inputs' if mismatches else 'matches-embedded-inputs',
            'receipt_schema': receipt.get('schema_version'),
            'receipt_identity': receipt_identity,
            'source_revisions': receipt.get('source_revisions', {}),
            'dirty_content_identity': receipt.get('dirty_content_identity'),
            'dirty_input_count': receipt.get('dirty_input_count'),
            'source_inputs_identity': receipt.get('source_inputs_identity'),
            'source_input_count': len(inputs) if isinstance(inputs, dict) else 0,
            'dependency_closure': receipt.get('dependency_closure', []),
            'build_configuration': receipt.get('build_configuration', {}),
            'mismatch_count': len(mismatches),
            'mismatches': mismatches[:32],
            'assurance': receipt.get('assurance', 'unknown'),
            'executable_sha256': executable_sha256,
        }
        origin['runtime_build_identity'] = digest({
            'kind': 'mncs.vm.runtime-build/1',
            'receipt_identity': receipt_identity,
            'executable_sha256': executable_sha256,
        })
    return {'schema_version': 'mncs.vm.selected-runtime/1', 'executable': str(executable),
            'executable_sha256': executable_sha256, 'contract': description,
            'build_origin': origin}


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
