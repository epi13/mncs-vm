#!/usr/bin/env python3
"""Selected VM identity and runtime-owned admission/execution transport."""
import argparse
import json
import os
import subprocess
import sys
from pathlib import Path
ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'python'))
from mncs_vm_client import inspect_runtime, Session, validate_toolchain_executables


def build_selected_runtime(cargo: str, executable: Path):
    """Rebuild only this selected checkout's debug VM and verify its receipt."""
    expected = (ROOT / 'target/debug/mncs-vm').resolve()
    if Path(executable).resolve() != expected:
        raise RuntimeError('provider build is bound to target/debug/mncs-vm')
    result = subprocess.run(
        [cargo, 'build', '--offline', '--locked', '--bin', 'mncs-vm'],
        cwd=ROOT, capture_output=True, text=True, timeout=900, check=False,
        env={**os.environ, 'GIT_OPTIONAL_LOCKS': '0'},
    )
    if result.returncode:
        raise RuntimeError('selected VM build failed: ' + result.stderr[-3000:])
    runtime = inspect_runtime(expected)
    origin = runtime.get('build_origin', {})
    if origin.get('status') != 'matches-embedded-inputs':
        raise RuntimeError('rebuilt VM does not match its embedded build inputs')
    if validate_toolchain_executables(origin.get('build_configuration')):
        raise RuntimeError('rebuilt VM receipt does not match selected build-tool bytes')
    return {
        'schema_version': 'mncs.vm.build-operation/1',
        'status': 'built-and-verified-locally',
        'build_command': [cargo, 'build', '--offline', '--locked', '--bin', 'mncs-vm'],
        'executable': str(expected),
        'runtime_identity': runtime,
        'build_stderr_tail': result.stderr[-1000:],
    }


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('operation', choices=['inspect', 'admit', 'call', 'build'])
    parser.add_argument('--artifact')
    parser.add_argument('--request')
    parser.add_argument('--executable', default=os.environ.get('MNCS_VM_BIN', str(ROOT / 'target/debug/mncs-vm')))
    parser.add_argument('--cargo', default=os.environ.get('CARGO', 'cargo'))
    args = parser.parse_args(argv)
    executable = Path(args.executable).resolve()
    try:
        if args.operation == 'inspect':
            value = inspect_runtime(executable)
        elif args.operation == 'build':
            value = build_selected_runtime(args.cargo, executable)
        elif args.operation == 'admit':
            if not args.artifact: parser.error('admit requires --artifact')
            result = subprocess.run([str(executable), 'admit', '--artifact', args.artifact], capture_output=True, text=True, timeout=120)
            value = json.loads(result.stdout)
            value['runtime_identity'] = inspect_runtime(executable)
            print(json.dumps(value, sort_keys=True)); return result.returncode
        else:
            if not args.artifact or not args.request: parser.error('call requires --artifact and --request')
            with Session(executable, Path(args.artifact)) as session:
                value = session.call(json.loads(Path(args.request).read_text()))
        print(json.dumps(value, sort_keys=True)); return 0
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
        print(json.dumps({'schema_version':'mncs.vm.provider-error/1','status':'error','reason':str(error)})); return 2


if __name__ == '__main__':
    raise SystemExit(main())
