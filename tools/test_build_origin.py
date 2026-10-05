import tempfile
import unittest
from pathlib import Path

from mncs_vm_client import sha, validate_toolchain_executables


class ToolchainReceiptTests(unittest.TestCase):
    def test_exact_selected_tool_bytes_are_verified(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            tools = {}
            for name in ("cargo", "rustc"):
                path = root / name
                path.write_bytes((name + " selected bytes").encode())
                tools[name] = {"configured_path": str(path), "resolved_path": str(path.resolve()), "sha256": sha(path)}
            self.assertEqual(validate_toolchain_executables({"toolchain_executables": tools}), [])
            (root / "cargo").write_bytes(b"different bytes")
            self.assertEqual(validate_toolchain_executables({"toolchain_executables": tools}), ["build-tool:cargo"])
            self.assertEqual(validate_toolchain_executables({}), ["toolchain-executable-identities"])


if __name__ == "__main__":
    unittest.main()
