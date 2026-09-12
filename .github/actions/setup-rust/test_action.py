import pathlib
import unittest


ACTION = pathlib.Path(__file__).with_name("action.yml")


class SetupRustActionContractTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.action = ACTION.read_text(encoding="utf-8")

    def test_pinned_toolchain_controls_current_and_future_command_lookup(self):
        locate = "rustup which --toolchain $toolchain rustc"
        current_path = '$env:PATH = "$toolchainBin$([IO.Path]::PathSeparator)$env:PATH"'
        future_path = "Add-Content -Path $env:GITHUB_PATH -Value $toolchainBin"
        future_toolchain = 'Add-Content -Path $env:GITHUB_ENV -Value "RUSTUP_TOOLCHAIN=$toolchain"'

        self.assertIn(locate, self.action)
        self.assertIn("$toolchainBin = Split-Path -Parent $rustcPath", self.action)
        self.assertIn('$env:RUSTUP_TOOLCHAIN = $toolchain', self.action)
        self.assertIn(current_path, self.action)
        self.assertIn(future_path, self.action)
        self.assertIn(future_toolchain, self.action)
        self.assertLess(self.action.index(locate), self.action.index(current_path))
        self.assertLess(self.action.index(current_path), self.action.index("& rustc --version"))

    def test_plain_commands_must_exactly_match_rustup_selected_versions(self):
        self.assertIn("$actualRustcVersion -ne $expectedRustcVersion", self.action)
        self.assertIn("$actualCargoVersion -ne $expectedCargoVersion", self.action)
        self.assertIn("rustc $([regex]::Escape($toolchain))", self.action)
        self.assertIn("throw \"Effective rustc version", self.action)
        self.assertIn("throw \"Effective cargo version", self.action)


if __name__ == "__main__":
    unittest.main()
