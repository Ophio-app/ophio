# SPDX-License-Identifier: Apache-2.0
"""Builder folders stay out of compiled programs wherever the checkout, the
target folder and the staged copy built inside it sit, including when one is
inside another or a folder name contains spaces."""

from __future__ import annotations

import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from release_common import release_environment

RUST_SOURCE = 'fn main() {\n    print!("{}", file!());\n}\n'
C_SOURCE = 'const char *ophio_where = __FILE__;\n'
MARKER = "private-checkout-marker"
PROBE_MANIFEST = """[package]
name = "native-probe"
version = "0.0.0"
edition = "2021"
publish = false

[build-dependencies]
cc = "=1.5.1"

[workspace]
"""
PROBE_BUILD = """fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    cc::Build::new().file(format!("{dir}/where.c")).compile("where");
}
"""


def cc_rs_words(environment: dict[str, str], name: str) -> list[str]:
    """Split a flags variable exactly as the locked cc 1.5.1 does: shell words
    when CC_SHELL_ESCAPED_FLAGS is set, otherwise ASCII whitespace."""
    value = environment.get(name, "")
    if environment.get("CC_SHELL_ESCAPED_FLAGS", "") not in ("", "0", "false", "no"):
        return shlex.split(value)
    return [word for word in re.split(r"[ \t\n\x0c\r]+", value) if word]


def c_compilers() -> list[str]:
    found = [name for name in ("cc", "clang") if shutil.which(name)]
    ndk = os.environ.get("NDK_HOME")
    if ndk:
        clang = Path(ndk) / "toolchains/llvm/prebuilt/linux-x86_64/bin/aarch64-linux-android29-clang"
        if clang.is_file():
            found.append(str(clang))
    return found


def make_tree(base: Path) -> None:
    (base / "crates/protocol/src").mkdir(parents=True, exist_ok=True)
    (base / "crates/protocol/src/main.rs").write_text(RUST_SOURCE, encoding="utf-8")
    (base / "crates/native").mkdir(parents=True, exist_ok=True)
    (base / "crates/native/where.c").write_text(C_SOURCE, encoding="utf-8")
    (base / "client/src-tauri").mkdir(parents=True, exist_ok=True)


class Layout(unittest.TestCase):
    """A checkout, a target folder and the staged copy where Android packaging
    puts it, arranged as `arrange` chooses."""

    def arrange(self, layout: str) -> None:
        scratch = Path(tempfile.mkdtemp(prefix="remap-"))
        self.addCleanup(shutil.rmtree, scratch, True)
        if layout == "separate":
            self.checkout, self.target = scratch / "checkout", scratch / "target"
        elif layout == "checkout inside target":
            self.target = scratch / "target"
            self.checkout = self.target / MARKER
        elif layout == "target inside checkout":
            self.checkout = scratch / "checkout"
            self.target = self.checkout / "target"
        elif layout == "spaces":
            self.checkout, self.target = scratch / "check out", scratch / "remap target"
        elif layout == "sibling":
            # GCC matches prefixes as text: the target's name begins the checkout's.
            self.checkout, self.target = scratch / f"work-{MARKER}", scratch / "work"
        else:
            raise ValueError(layout)
        self.scratch = scratch
        self.staged = self.target / "packaging/android/project"
        make_tree(self.checkout)
        make_tree(self.staged)
        # Build output, as a build script's generated code would sit there.
        self.generated = self.target / "generated"
        make_tree(self.generated)
        variables = {"CARGO_TARGET_DIR": str(self.target), "OPHIO_PUBLIC_RELEASE": "0"}
        patch = mock.patch.dict(os.environ, variables)
        patch.start()
        self.addCleanup(patch.stop)
        for name in ("CARGO_ENCODED_RUSTFLAGS", "RUSTFLAGS", "CFLAGS", "CXXFLAGS", "CC_SHELL_ESCAPED_FLAGS"):
            os.environ.pop(name, None)

    def builder_paths(self, data: bytes) -> list[str]:
        found = []
        for folder in (self.scratch, self.target, self.checkout):
            for form in {str(folder), str(folder.resolve())}:
                if form.encode() in data:
                    found.append(form)
        if MARKER.encode() in data:
            found.append(MARKER)
        return sorted(set(found))


class RemapFlagTests(Layout):
    def setUp(self):
        self.arrange("separate")

    def test_mappings_run_from_the_shortest_prefix_to_the_longest(self):
        environment = release_environment("android", self.checkout, staged=self.staged)
        flags = environment["CARGO_ENCODED_RUSTFLAGS"].split("\x1f")
        remaps = [flag for flag in flags if flag.startswith("--remap-path-prefix=")]
        target = f"--remap-path-prefix={self.target.resolve()}=/target"
        staged = f"--remap-path-prefix={self.staged.resolve()}=/src"
        self.assertLess(remaps.index(target), remaps.index(staged))
        prefixes = [flag.split("=", 1)[1].rsplit("=", 1)[0] for flag in remaps]
        self.assertEqual([len(p) for p in prefixes], sorted(len(p) for p in prefixes))
        self.assertIn("-Clink-arg=-landroid", flags)
        for variable in ("CFLAGS", "CXXFLAGS"):
            words = cc_rs_words(environment, variable)
            self.assertLess(
                words.index(f"-ffile-prefix-map={self.target.resolve()}=/target"),
                words.index(f"-ffile-prefix-map={self.staged.resolve()}=/src"),
            )

    def test_a_symlinked_target_is_remapped_as_given_and_resolved(self):
        link = self.scratch / "link"
        link.symlink_to(self.target, target_is_directory=True)
        os.environ["CARGO_TARGET_DIR"] = str(link)
        flags = release_environment("linux", self.checkout)["CARGO_ENCODED_RUSTFLAGS"]
        self.assertIn(f"--remap-path-prefix={link}=/target", flags.split("\x1f"))
        self.assertIn(f"--remap-path-prefix={self.target.resolve()}=/target", flags.split("\x1f"))

    def test_plain_folders_leave_existing_c_flags_as_cc_rs_already_read_them(self):
        os.environ["CFLAGS"] = "-O2  -DQUOTE='x'"
        environment = release_environment("android", self.checkout, staged=self.staged)
        self.assertNotIn("CC_SHELL_ESCAPED_FLAGS", environment)
        self.assertTrue(environment["CFLAGS"].startswith("-O2  -DQUOTE='x' "))
        words = cc_rs_words(environment, "CFLAGS")
        self.assertEqual(words[:2], ["-O2", "-DQUOTE='x'"])
        self.assertIn(f"-ffile-prefix-map={self.staged.resolve()}=/src", words)


class SpacedFlagTests(Layout):
    def setUp(self):
        self.arrange("spaces")

    def test_cc_rs_reads_a_spaced_mapping_as_one_flag_and_keeps_existing_flags(self):
        os.environ["CFLAGS"] = "-O2 -DQUOTE='x'"
        os.environ["CFLAGS_aarch64-linux-android"] = "-Wall\t-DA=1"
        environment = release_environment("android", self.checkout, staged=self.staged)
        self.assertEqual(environment["CC_SHELL_ESCAPED_FLAGS"], "1")
        words = cc_rs_words(environment, "CFLAGS")
        self.assertEqual(words[:2], ["-O2", "-DQUOTE='x'"])
        self.assertIn(f"-ffile-prefix-map={self.target.resolve()}=/target", words)
        self.assertIn(f"-ffile-prefix-map={self.staged.resolve()}=/src", words)
        self.assertEqual(cc_rs_words(environment, "CFLAGS_aarch64-linux-android"), ["-Wall", "-DA=1"])

    def test_flags_already_written_for_shell_parsing_are_kept(self):
        os.environ["CC_SHELL_ESCAPED_FLAGS"] = "1"
        os.environ["CFLAGS"] = "'-DNAME=a b'"
        environment = release_environment("android", self.checkout, staged=self.staged)
        words = cc_rs_words(environment, "CFLAGS")
        self.assertEqual(words[0], "-DNAME=a b")
        self.assertIn(f"-ffile-prefix-map={self.staged.resolve()}=/src", words)


LAYOUTS = ("separate", "checkout inside target", "target inside checkout", "spaces", "sibling")


@unittest.skipUnless(shutil.which("rustc"), "needs rustc")
class RustCompileTests(Layout):
    def compile(self, source: Path, environment: dict[str, str], cwd: Path) -> tuple[str, bytes]:
        program = self.scratch / "where"
        flags = [f for f in environment["CARGO_ENCODED_RUSTFLAGS"].split("\x1f") if f]
        flags = [f for f in flags if not f.startswith("-Clink-arg=-l")]
        subprocess.run(
            ["rustc", "--edition", "2024", *flags, str(source), "-o", str(program)],
            cwd=cwd, env=environment, check=True, capture_output=True,
        )
        printed = subprocess.run(
            [str(program)], check=True, capture_output=True, text=True, encoding="utf-8"
        ).stdout
        return printed, program.read_bytes()

    def test_checkout_and_staged_sources_compile_as_src_in_every_layout(self):
        for layout in LAYOUTS:
            with self.subTest(layout=layout):
                self.arrange(layout)
                checkout_build = release_environment("linux", self.checkout)
                printed, program = self.compile(
                    self.checkout / "crates/protocol/src/main.rs", checkout_build, self.checkout
                )
                self.assertEqual(printed, "/src/crates/protocol/src/main.rs")
                self.assertEqual(self.builder_paths(program), [])
                # Cargo names a path dependency by its absolute path and runs
                # rustc from the phone shell's workspace, as Tauri's build does.
                staged_build = release_environment("android", self.checkout, staged=self.staged)
                printed, program = self.compile(
                    self.staged / "crates/protocol/src/main.rs",
                    staged_build,
                    self.staged / "client/src-tauri",
                )
                self.assertEqual(printed, "/src/crates/protocol/src/main.rs")
                self.assertEqual(self.builder_paths(program), [])
                printed, program = self.compile(
                    self.generated / "crates/protocol/src/main.rs", staged_build, self.generated
                )
                self.assertEqual(printed, "/target/generated/crates/protocol/src/main.rs")
                self.assertEqual(self.builder_paths(program), [])


@unittest.skipUnless(c_compilers(), "needs a C compiler")
class NativeCompileTests(Layout):
    def test_c_file_names_compile_as_src_in_every_layout_and_compiler(self):
        for layout in LAYOUTS:
            for compiler in c_compilers():
                with self.subTest(layout=layout, compiler=Path(compiler).name):
                    self.arrange(layout)
                    environment = release_environment("android", self.checkout, staged=self.staged)
                    expected = {
                        self.checkout: b"/src/crates/native/where.c",
                        self.staged: b"/src/crates/native/where.c",
                        self.generated: b"/target/generated/crates/native/where.c",
                    }
                    for base, name in expected.items():
                        objects = self.scratch / "where.o"
                        subprocess.run(
                            [compiler, *cc_rs_words(environment, "CFLAGS"), "-c",
                             str(base / "crates/native/where.c"), "-o", str(objects)],
                            cwd=base, env=environment, check=True, capture_output=True,
                        )
                        data = objects.read_bytes()
                        self.assertIn(name, data)
                        self.assertEqual(self.builder_paths(data), [])


@unittest.skipUnless(shutil.which("cargo") and shutil.which("cc"), "needs cargo and a C compiler")
class LockedCcCrateTests(Layout):
    """The real cc 1.5.1 crate, offline from the local Cargo cache, compiling
    C in the staged copy under a target folder whose name contains spaces."""

    def test_the_locked_cc_crate_compiles_staged_c_without_builder_paths(self):
        self.arrange("spaces")
        probe = self.staged / "crates/native-probe"
        probe.mkdir(parents=True)
        (probe / "Cargo.toml").write_text(PROBE_MANIFEST, encoding="utf-8")
        (probe / "build.rs").write_text(PROBE_BUILD, encoding="utf-8")
        (probe / "where.c").write_text(C_SOURCE, encoding="utf-8")
        (probe / "src").mkdir()
        (probe / "src/lib.rs").write_text("", encoding="utf-8")
        environment = release_environment("linux", self.checkout, staged=self.staged)
        environment["CARGO_NET_OFFLINE"] = "true"
        result = subprocess.run(
            ["cargo", "build", "--offline", "--quiet", "--manifest-path", str(probe / "Cargo.toml")],
            env=environment, capture_output=True, text=True, encoding="utf-8",
        )
        if result.returncode != 0 and re.search(
            r"failed to select a version|no matching package|failed to download|"
            r"attempting to make an HTTP request|failed to load source",
            result.stderr,
        ):
            self.skipTest("the locked cc 1.5.1 crate is not in the local Cargo cache")
        self.assertEqual(result.returncode, 0, "\n".join(result.stderr.splitlines()[-12:]))
        objects = [path for path in self.target.rglob("*.o") if "where" in path.name]
        self.assertTrue(objects)
        for path in objects:
            data = path.read_bytes()
            self.assertIn(b"/src/crates/native-probe/where.c", data)
            self.assertEqual(self.builder_paths(data), [])


if __name__ == "__main__":
    unittest.main()
