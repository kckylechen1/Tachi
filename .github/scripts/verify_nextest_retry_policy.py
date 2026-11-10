#!/usr/bin/env python3
"""Exercise the repository's CI retry policy with the actual nextest binary."""

import json
import os
from pathlib import Path
import subprocess
import tempfile
import xml.etree.ElementTree as ET


ROOT = Path(__file__).resolve().parents[2]
SOURCE = r'''
#[test]
fn first_try_passes() {}

#[test]
fn retry_passes() {
    assert_ne!(std::env::var("NEXTEST_ATTEMPT").unwrap(), "1");
}

#[test]
fn always_fails() { panic!("deliberate persistent failure"); }
'''


def main():
    nextest = os.environ["AUDITED_NEXTEST"]
    # The fixture shares the allocated build target but owns its temporary
    # source and reports. It must never overwrite the workspace JUnit report.
    target = str(Path(os.environ["CARGO_TARGET_DIR"]).resolve())
    with tempfile.TemporaryDirectory(prefix="nextest-ci-policy-") as temporary:
        fixture = Path(temporary)
        (fixture / "src").mkdir()
        (fixture / "src/lib.rs").write_text(SOURCE)
        (fixture / "Cargo.toml").write_text(
            '[package]\nname = "nextest-ci-policy-probe"\n'
            'version = "0.0.0"\nedition = "2021"\n[workspace]\n'
        )
        (fixture / "Cargo.lock").write_text(
            'version = 3\n[[package]]\nname = "nextest-ci-policy-probe"\n'
            'version = "0.0.0"\n'
        )
        config = (ROOT / ".config/nextest.toml").read_text()
        # Preserve the HTTP JUnit override while matching this standalone probe.
        # Nextest rejects an override whose package/binary exists only in Tachi.
        report_dir = fixture / "reports"
        config = config.replace('dir = "target/nextest"',
                                f'dir = {json.dumps(str(report_dir))}', 1)
        product_filter = "package(=tachi-server) & binary(/^http_dogfood_/)"
        if config.count(product_filter) != 1:
            raise AssertionError("expected exactly one HTTP dogfood JUnit override")
        config = config.replace(product_filter, "package(=nextest-ci-policy-probe)")
        config_file = fixture / "nextest.toml"
        config_file.write_text(config)
        for name, expected_exit, expected_failure, retry_tag in (
            ("first_try_passes", 0, False, None),
            ("retry_passes", 100, True, "flakyFailure"),
            ("always_fails", 100, True, "rerunFailure"),
        ):
            report = report_dir / "ci/junit.xml"
            report.unlink(missing_ok=True)
            result = subprocess.run(
                [nextest, "nextest", "run", "--manifest-path",
                 str(fixture / "Cargo.toml"), "--target-dir", target,
                 "--config-file", str(config_file), "--profile", "ci",
                 "--offline", "--locked", "--color", "never",
                 "--no-tests=fail", "-E", f"test(={name})"],
                cwd=fixture, text=True, capture_output=True, timeout=120,
            )
            if result.returncode != expected_exit:
                raise AssertionError(
                    f"{name}: exit {result.returncode}, expected {expected_exit}\n"
                    f"{result.stdout}\n{result.stderr}"
                )
            cases = list(ET.parse(report).iter("testcase"))
            assert len(cases) == 1 and cases[0].get("name") == name, cases
            case = cases[0]
            assert (case.find("failure") is not None) == expected_failure, ET.tostring(case)
            if retry_tag:
                assert case.find(retry_tag) is not None, ET.tostring(case)
            print(f"{name}: exit={result.returncode}, JUnit verified", flush=True)


if __name__ == "__main__":
    main()
