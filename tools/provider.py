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
from mncs_vm_client import inspect_runtime, Session


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('operation', choices=['inspect', 'admit', 'call'])
    parser.add_argument('--artifact')
    parser.add_argument('--request')
    parser.add_argument('--executable', default=os.environ.get('MNCS_VM_BIN', str(ROOT / 'target/debug/mncs-vm')))
    args = parser.parse_args(argv)
    executable = Path(args.executable).resolve()
    try:
        if args.operation == 'inspect':
            value = inspect_runtime(executable)
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
