#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Offline per-artifact notices and CycloneDX 1.5 inventory (Python 3.11+)."""

from __future__ import annotations

import argparse
import base64
from collections import defaultdict
from dataclasses import dataclass, field
import io
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tomllib
from urllib.parse import quote
import xml.etree.ElementTree as ET
import zipfile

from release_common import (
    ROOT,
    ReleaseError,
    atomic_write,
    digest,
    identity,
    notice,
    readable_output,
)

TARGETS = {
    "linux": "x86_64-unknown-linux-gnu",
    "windows": "x86_64-pc-windows-gnullvm",
    "android": "aarch64-linux-android",
}
LEGAL = re.compile(
    r"^(?:licen[cs]es?|copying|copyrights?|notices?|authors|ofl|unlicense)(?:[._-].*)?$",
    re.I,
)
SKIP = {"node_modules", "target", ".git", "__pycache__"}
CRATES_IO = {
    "registry+https://github.com/rust-lang/crates.io-index",
    "sparse+https://index.crates.io/",
}


class LicenseError(ReleaseError):
    """Missing or inconsistent licence/provenance evidence."""


def run(args: list[str], cwd: Path) -> str:
    result = subprocess.run(
        args,
        cwd=cwd,
        text=True,
        encoding="utf-8",
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if result.returncode:
        raise LicenseError(
            f"{args[0]} failed ({result.returncode}): {result.stderr.strip()}"
        )
    return result.stdout


def normalize(expression: str | None) -> str:
    # Cargo still accepts the old slash-separated alternative syntax.
    return re.sub(r"\s*/\s*", " OR ", expression or "").strip()


def policy_result(expression: str, policy: dict) -> tuple[int, str]:
    """Return severity (0 allowed, 1 review, 2 refused) and chosen SPDX branch."""
    tokens = re.findall(r"[A-Za-z0-9.+-]+|[()]", normalize(expression))
    if "".join(tokens) != re.sub(r"\s+", "", normalize(expression)) or not tokens:
        return 2, "missing or malformed licence"
    index = 0

    def atom() -> tuple[int, str]:
        nonlocal index
        if index >= len(tokens):
            raise ValueError
        token = tokens[index]
        index += 1
        if token == "(":
            value = either()
            if index >= len(tokens) or tokens[index] != ")":
                raise ValueError
            index += 1
            return value
        if token in {"AND", "OR", "WITH", ")"}:
            raise ValueError
        if index < len(tokens) and tokens[index] == "WITH":
            index += 1
            if index >= len(tokens) or tokens[index] in {"AND", "OR", "WITH", "(", ")"}:
                raise ValueError
            token += " WITH " + tokens[index]
            index += 1
        if token in policy["allowed"]:
            return 0, token
        if token in policy["review"]:
            return 1, token
        return 2, token

    def both() -> tuple[int, str]:
        nonlocal index
        value = atom()
        while index < len(tokens) and tokens[index] == "AND":
            index += 1
            other = atom()
            value = max(value[0], other[0]), f"({value[1]} AND {other[1]})"
        return value

    def either() -> tuple[int, str]:
        nonlocal index
        value = both()
        while index < len(tokens) and tokens[index] == "OR":
            index += 1
            other = both()
            if other[0] < value[0]:
                value = other
        return value

    try:
        value = either()
        return value if index == len(tokens) else (2, "malformed licence")
    except ValueError:
        return 2, "malformed licence"


def legal_texts(directory: Path, extra: str | None = None) -> dict[str, str]:
    if not directory.is_dir():
        raise LicenseError(f"Package source is not cached: {directory.name}")
    candidates = []
    for base, dirs, files in os.walk(directory, followlinks=False):
        dirs[:] = sorted(
            d for d in dirs if d not in SKIP and not (Path(base) / d).is_symlink()
        )
        for name in sorted(files):
            if LEGAL.fullmatch(name) or any(
                p.lower() in {"licenses", "licences"}
                for p in Path(base).relative_to(directory).parts
            ):
                candidates.append(Path(base) / name)
    if extra:
        candidates.append(directory / extra)
    texts = {}
    for path in sorted(set(candidates)):
        if not path.resolve().is_relative_to(directory.resolve()) or path.is_symlink():
            raise LicenseError(f"Licence file escapes package: {directory.name}")
        data = path.read_bytes()
        if b"\0" in data:
            raise LicenseError(f"Non-text licence file: {directory.name}/{path.name}")
        texts[path.relative_to(directory).as_posix()] = data.decode(
            "utf-8", errors="strict"
        )
    return texts


@dataclass
class Component:
    ref: str
    name: str
    version: str
    license: str
    texts: dict[str, str] = field(default_factory=dict)
    kind: str = "library"
    shipped: bool = True
    hashes: list[dict] = field(default_factory=list)
    url: str | None = None
    properties: dict[str, str] = field(default_factory=dict)

    def bom(self) -> dict:
        result = {
            "type": self.kind,
            "bom-ref": self.ref,
            "name": self.name,
            "version": self.version,
            "scope": "required" if self.shipped else "optional",
        }
        if self.ref.startswith("pkg:"):
            result["purl"] = self.ref
        if self.license:
            result["licenses"] = [{"expression": normalize(self.license)}]
        if self.hashes:
            result["hashes"] = self.hashes
        if self.url:
            result["externalReferences"] = [{"type": "distribution", "url": self.url}]
        props = {"distribution:shipped": str(self.shipped).lower(), **self.properties}
        result["properties"] = [
            {"name": k, "value": v} for k, v in sorted(props.items())
        ]
        return result


def cargo_closure(
    metadata: dict, root_names: list[str], selected: set[tuple[str, str]] | None = None
) -> tuple[list[dict], dict[str, set[str]]]:
    """Walk only normal/build edges; selected is Cargo's target/feature tree."""
    packages = {p["id"]: p for p in metadata["packages"]}
    nodes = {n["id"]: n for n in metadata["resolve"]["nodes"]}
    pending = [
        p["id"]
        for p in packages.values()
        if p["name"] in root_names and p.get("source") is None
    ]
    if len(pending) != len(root_names):
        raise LicenseError("Artifact roots missing from Cargo metadata")
    seen, edges = set(), {}
    while pending:
        current = pending.pop()
        if current in seen:
            continue
        seen.add(current)
        children = set()
        for dep in nodes[current]["deps"]:
            if not any(
                k["kind"] in (None, "normal", "build") for k in dep["dep_kinds"]
            ):
                continue
            child = packages[dep["pkg"]]
            if selected is None or (child["name"], child["version"]) in selected:
                children.add(child["id"])
        edges[current] = children
        pending.extend(children - seen)
    return [packages[k] for k in sorted(seen)], edges


def cargo_components(
    root: Path, artifact: str, target: str
) -> tuple[list[Component], dict[str, set[str]]]:
    manifest = root / (
        "client/src-tauri/Cargo.toml" if artifact == "android" else "Cargo.toml"
    )
    roots = ["phone-shell"] if artifact == "android" else ["companion", "computer-mcp"]
    common = ["--manifest-path", str(manifest), "--locked", "--offline"]
    metadata = json.loads(
        run(
            [
                "cargo",
                "metadata",
                "--format-version",
                "1",
                "--filter-platform",
                target,
                *common,
            ],
            root,
        )
    )
    tree = run(
        [
            "cargo",
            "tree",
            *common,
            "--target",
            target,
            "--edges",
            "normal,build",
            "--prefix",
            "none",
            "--format",
            "{p}",
            *[s for n in roots for s in ("-p", n)],
        ],
        root,
    )
    selected = set(re.findall(r"^([^\s]+) v([^\s]+)", tree, re.M))
    packages, graph = cargo_closure(metadata, roots, selected)
    lockfile = manifest.with_name("Cargo.lock")
    locks = tomllib.loads(lockfile.read_text(encoding="utf-8"))["package"]
    checksums = {
        (p["name"], p["version"], p.get("source")): p.get("checksum") for p in locks
    }
    refs = {
        p["id"]: f"pkg:cargo/{quote(p['name'], safe='')}@{p['version']}"
        for p in packages
    }
    components = []
    for package in packages:
        owned = package["source"] is None
        texts = (
            {
                "LICENSE": (root / "LICENSE").read_text(encoding="utf-8"),
                "NOTICE": notice(root),
            }
            if owned
            else legal_texts(
                Path(package["manifest_path"]).parent, package.get("license_file")
            )
        )
        checksum = checksums.get(
            (package["name"], package["version"], package["source"])
        )
        comp = Component(
            refs[package["id"]],
            package["name"],
            package["version"],
            package["license"] or "",
            texts,
        )
        comp.properties = {
            "distribution:ecosystem": "cargo",
            "distribution:dependency-selection": "target-specific normal+build closure; dev edges excluded",
        }
        if owned:
            comp.properties["distribution:owned"] = "true"
        elif package["source"] in CRATES_IO:
            # The exact source archive, whose checksum the inventory records:
            # where to obtain the source form, as MPL-2.0 section 3.2 requires.
            name = quote(package["name"], safe="")
            comp.url = f"https://crates.io/api/v1/crates/{name}/{package['version']}/download"
        if checksum:
            comp.hashes = [{"alg": "SHA-256", "content": checksum}]
        components.append(comp)
    return components, {refs[k]: {refs[c] for c in v} for k, v in graph.items()}


def npm_closure(lock: dict) -> tuple[set[str], dict[str, set[str]]]:
    packages = lock["packages"]
    seen, graph, pending = set(), {}, [""]

    def resolve(parent: str, name: str) -> str | None:
        current = parent
        while True:
            candidate = f"{current}/node_modules/{name}".lstrip("/")
            if candidate in packages:
                return candidate
            if not current:
                return None
            current = (
                current.rsplit("/node_modules/", 1)[0]
                if "/node_modules/" in current
                else ""
            )

    while pending:
        key = pending.pop()
        if key in seen:
            continue
        seen.add(key)
        package = packages[key]
        children = set()
        optional = package.get("optionalDependencies", {})
        peers = package.get("peerDependencies", {}) if key else {}
        for name in {**package.get("dependencies", {}), **optional, **peers}:
            child = resolve(key, name)
            if child is None:
                if name in optional or package.get("peerDependenciesMeta", {}).get(
                    name, {}
                ).get("optional"):
                    continue
                raise LicenseError(
                    f"Unresolved production npm dependency: {key or 'client'} -> {name}"
                )
            children.add(child)
        graph[key] = children
        pending.extend(children - seen)
    return seen, graph


def npm_components(root: Path) -> tuple[list[Component], dict[str, set[str]]]:
    lock = json.loads((root / "client/package-lock.json").read_text(encoding="utf-8"))
    selected, graph = npm_closure(lock)
    refs, components = {}, []
    for path in sorted(selected):
        package = lock["packages"][path]
        directory = root / "client" / path
        installed = json.loads((directory / "package.json").read_text(encoding="utf-8"))
        name, version = installed["name"], package["version"]
        if installed["version"] != version:
            raise LicenseError(f"Installed npm version differs from lock: {name}")
        ref = f"pkg:npm/{quote(name, safe='/')}@{version}"
        refs[path] = ref
        texts = (
            legal_texts(directory)
            if path
            else {
                "LICENSE": (root / "LICENSE").read_text(encoding="utf-8"),
                "NOTICE": notice(root),
            }
        )
        comp = Component(
            ref,
            name,
            version,
            installed.get("license", package.get("license", "")),
            texts,
        )
        comp.properties = {
            "distribution:ecosystem": "npm",
            "distribution:dependency-selection": "conservative production closure including peers; not a tree-shaken module inventory",
        }
        for item in package.get("integrity", "").split():
            algorithm, _, encoded = item.partition("-")
            if algorithm in {"sha1", "sha256", "sha384", "sha512"}:
                value = base64.b64decode(encoded, validate=True).hex()
                comp.hashes.append(
                    {
                        "alg": {
                            "sha1": "SHA-1",
                            "sha256": "SHA-256",
                            "sha384": "SHA-384",
                            "sha512": "SHA-512",
                        }[algorithm],
                        "content": value,
                    }
                )
        components.append(comp)
    return components, {refs[k]: {refs[c] for c in v} for k, v in graph.items()}


def speech_components(root: Path) -> list[Component]:
    source = (root / "crates/speech/src/catalog.rs").read_text(encoding="utf-8")
    pattern = r'model!\(\s*"([^"]+)",\s*"([a-f0-9]{40})",\s*([\d_]+),\s*"([a-f0-9]{64})",\s*([\d_]+),\s*"([a-f0-9]{64})"\s*\)'
    entries = re.findall(pattern, source)
    tokenizer = re.search(
        r'name: "tokenizer.json",\s*size_bytes: ([\d_]+),\s*sha256: "([a-f0-9]{64})"',
        source,
    )
    license_id = re.search(r'license: "([^"]+)"', source)
    if (
        not entries
        or source.count("model!(") != len(entries)
        or source.count("https://huggingface.co/openai/whisper-") != 4
        or not tokenizer
        or not license_id
    ):
        raise LicenseError(
            "Speech catalog format changed; update inventory parser before packaging"
        )
    licence = root / "packaging/licenses/whisper-LICENSE"
    texts = {"LICENSE": licence.read_text(encoding="utf-8")}
    result = []
    for name, revision, size, checksum, config_size, config_hash in entries:
        for filename, size, checksum in [
            ("model.safetensors", size, checksum),
            ("config.json", config_size, config_hash),
            ("tokenizer.json", *tokenizer.groups()),
        ]:
            ref = f"pkg:generic/openai/whisper-{name}@{revision}?file={filename}"
            comp = Component(
                ref,
                f"Whisper {name}/{filename}",
                revision,
                license_id[1],
                texts,
                "machine-learning-model"
                if filename.endswith("safetensors")
                else "file",
                False,
            )
            comp.hashes = [{"alg": "SHA-256", "content": checksum}]
            comp.url = f"https://huggingface.co/openai/whisper-{name}/resolve/{revision}/{filename}"
            comp.properties = {
                "distribution:role": "external runtime download; explicit owner install only",
                "distribution:size": str(int(size.replace("_", ""))),
            }
            result.append(comp)
    return result


def embedded_legal(path: Path) -> dict[str, str]:
    result = {}
    with zipfile.ZipFile(path) as archive:
        for name in sorted(archive.namelist()):
            if name.endswith("/"):
                continue
            if LEGAL.fullmatch(Path(name).name):
                result[name] = archive.read(name).decode("utf-8")
            elif name == "classes.jar":
                with zipfile.ZipFile(io.BytesIO(archive.read(name))) as classes:
                    for entry in sorted(classes.namelist()):
                        if not entry.endswith("/") and LEGAL.fullmatch(
                            Path(entry).name
                        ):
                            result[f"classes.jar/{entry}"] = classes.read(entry).decode(
                                "utf-8"
                            )
    return result


def maven_license(pom: Path, cache: Path, visited: set[Path] | None = None) -> str:
    visited = set() if visited is None else visited
    if pom in visited:
        raise LicenseError("Cyclic Maven parent POM")
    visited.add(pom)
    xml = ET.parse(pom).getroot()
    ns = {"m": "http://maven.apache.org/POM/4.0.0"}
    names = [
        x.findtext("m:name", "", ns) for x in xml.findall("m:licenses/m:license", ns)
    ]
    expressions = []
    for name in names:
        if re.search(r"apache", name, re.I) and re.search(r"2(?:\.0)?", name):
            expressions.append("Apache-2.0")
        elif name in {"MIT", "MIT License", "The MIT License"}:
            expressions.append("MIT")
        elif name in {"BSD-3-Clause", "BSD-2-Clause", "ISC"}:
            expressions.append(name)
        else:
            raise LicenseError(f"Unrecognized Maven licence in {pom.name}: {name}")
    if expressions:
        # Do not invent an OR where the POM did not explicitly offer a choice.
        return " AND ".join(sorted(set(expressions)))
    parent = xml.find("m:parent", ns)
    if parent is not None:
        parts = [
            parent.findtext(f"m:{key}", "", ns)
            for key in ("groupId", "artifactId", "version")
        ]
        if not all(parts) or any("${" in p for p in parts):
            raise LicenseError(f"Unresolved Maven parent in {pom.name}")
        matches = sorted(cache.joinpath(*parts).glob("*/*.pom"))
        if len(matches) == 1:
            return maven_license(matches[0], cache, visited)
    raise LicenseError(f"No cached Maven licence declaration for {pom.name}")


def gradle_components(
    root: Path, report: Path
) -> tuple[list[Component], dict[str, set[str]]]:
    if not report.is_file():
        raise LicenseError(
            "Android releaseRuntimeClasspath report missing; run scripts/package-android.sh (offline Gradle resolution required)"
        )
    data = json.loads(report.read_text(encoding="utf-8"))
    if data.get("configuration") != "releaseRuntimeClasspath" or not data.get(
        "components"
    ):
        raise LicenseError("Invalid/empty Android runtime dependency report")
    cache = (
        Path(os.environ.get("GRADLE_USER_HOME", str(Path.home() / ".gradle")))
        / "caches/modules-2/files-2.1"
    )
    components, graph = [], {}
    for row in data["components"]:
        group, name, version = row["group"], row["name"], row["version"]
        ref = f"pkg:maven/{quote(group, safe='')}/{quote(name, safe='')}@{version}"
        base = cache / group / name / version
        poms = sorted(base.glob("*/*.pom"))
        if len(poms) != 1:
            raise LicenseError(
                f"Cached POM unavailable/ambiguous: {group}:{name}:{version}"
            )
        expression = maven_license(poms[0], cache)
        paths = [Path(p) for p in row["artifacts"]]
        if not paths:
            # BOM/platform constraints contain no packaged bytes.
            continue
        texts = {}
        hashes = []
        for path in paths:
            if not path.resolve().is_relative_to(cache.resolve()) or not path.is_file():
                raise LicenseError(f"Gradle artifact outside dependency cache: {name}")
            if path.suffix in {".jar", ".aar"}:
                texts.update(
                    {f"{path.name}/{k}": v for k, v in embedded_legal(path).items()}
                )
            hashes.append({"alg": "SHA-256", "content": digest(path)})
        if "Apache-2.0" in expression:
            texts["Apache-2.0.txt"] = (root / "LICENSE").read_text(encoding="utf-8")
        # Preserve upstream POM attribution as well as all embedded NOTICE files.
        texts[poms[0].name] = poms[0].read_text(encoding="utf-8")
        comp = Component(
            ref, f"{group}:{name}", version, expression, texts, hashes=hashes
        )
        comp.properties = {
            "distribution:ecosystem": "maven",
            "distribution:dependency-selection": "Gradle releaseRuntimeClasspath; conservative before R8 shrinking",
        }
        components.append(comp)
        graph[ref] = set(row.get("dependencies", []))
    refs = {c.ref for c in components}
    return components, {k: v & refs for k, v in graph.items()}


def extra_components(
    root: Path, artifact: str, components: list[Component], bundle: Path | None
) -> list[Component]:
    result = []
    inventory = tomllib.loads(
        (root / "packaging/components.toml").read_text(encoding="utf-8")
    )
    for item in inventory.get("component", []):
        if artifact not in item["artifacts"]:
            continue
        texts = {p: (root / p).read_text(encoding="utf-8") for p in item["texts"]}
        result.append(
            Component(
                item["purl"],
                item["name"],
                item["version"],
                item["license"],
                texts,
                item.get("type", "library"),
                item["shipped"],
                url=item["url"],
                properties={
                    "distribution:role": item["role"],
                    **(
                        {"distribution:review": item["review"]}
                        if item.get("review")
                        else {}
                    ),
                },
            )
        )
    if artifact != "android":
        sqlite = next((c for c in components if c.name == "libsqlite3-sys"), None)
        if sqlite:
            metadata = json.loads(
                run(
                    [
                        "cargo",
                        "metadata",
                        "--format-version",
                        "1",
                        "--locked",
                        "--offline",
                        "--filter-platform",
                        TARGETS[artifact],
                    ],
                    root,
                )
            )
            package = next(
                p
                for p in metadata["packages"]
                if p["name"] == "libsqlite3-sys" and p["version"] == sqlite.version
            )
            source_path = Path(package["manifest_path"]).parent / "sqlite3/sqlite3.c"
            source = source_path.read_text(encoding="utf-8")
            version_match = re.search(r'#define SQLITE_VERSION\s+"([^"]+)"', source)
            blessing = re.search(r"/\*\n\*\* 2001 September 15.*?\*/", source, re.S)
            if not version_match or not blessing:
                raise LicenseError("Bundled SQLite provenance changed")
            version = version_match[1]
            result.append(
                Component(
                    f"pkg:generic/sqlite@{version}",
                    "SQLite amalgamation",
                    version,
                    "blessing",
                    {"sqlite3.c copyright disclaimer": blessing[0]},
                    hashes=[{"alg": "SHA-256", "content": digest(source_path)}],
                    url="https://sqlite.org/copyright.html",
                    properties={"distribution:parent": sqlite.ref},
                )
            )
        result.extend(speech_components(root))
        return result
    parents = {c.name: c for c in components}
    for package in (
        "@fontsource-variable/inter",
        "@fontsource-variable/jetbrains-mono",
    ):
        parent = parents.get(package)
        if parent is None:
            continue
        directory = root / "client/node_modules" / package
        fonts = sorted(directory.rglob("*.woff2"))
        bundle_hashes = (
            {digest(p) for p in bundle.rglob("*.woff2")}
            if bundle and bundle.is_dir()
            else None
        )
        for path in fonts:
            checksum = digest(path)
            if bundle_hashes is not None and checksum not in bundle_hashes:
                continue
            ref = f"pkg:generic/{quote(package.removeprefix('@fontsource-variable/'))}-font/{path.stem}@{parent.version}"
            comp = Component(
                ref,
                path.name,
                parent.version,
                "OFL-1.1",
                parent.texts,
                "file",
                hashes=[{"alg": "SHA-256", "content": checksum}],
            )
            comp.properties = {
                "distribution:parent": parent.ref,
                "distribution:selection": "built font hash match"
                if bundle_hashes is not None
                else "conservative production font package inventory",
            }
            result.append(comp)
    if "lucide-react" in parents:
        parent = parents["lucide-react"]
        result.append(
            Component(
                f"pkg:generic/lucide-icons@{parent.version}",
                "Lucide icons (including Feather-derived icons)",
                parent.version,
                "ISC AND MIT",
                parent.texts,
                properties={
                    "distribution:parent": parent.ref,
                    "distribution:selection": "conservative icon-set inventory; SVGs compiled into JavaScript",
                },
            )
        )
    return result


def emit(
    root: Path,
    artifact: str,
    output: Path,
    components: list[Component],
    graph: dict[str, set[str]],
    gaps: list[str],
    policy: dict,
) -> bool:
    name, slug, version = identity(root)
    owned = Component(
        f"pkg:generic/{slug}-{artifact}@{version}",
        f"{name} ({artifact})",
        version,
        "Apache-2.0",
        {
            "LICENSE": (root / "LICENSE").read_text(encoding="utf-8"),
            "NOTICE": notice(root),
        },
        "application",
    )
    merged = {}
    for comp in [owned, *components]:
        if comp.ref in merged and comp != merged[comp.ref]:
            raise LicenseError(f"Conflicting component identity: {comp.ref}")
        merged[comp.ref] = comp
    components = sorted(merged.values(), key=lambda c: c.ref)
    failures, reviews, exceptions = list(gaps), [], []
    evidence_path = root / "packaging/license-evidence.toml"
    evidence = (
        tomllib.loads(evidence_path.read_text(encoding="utf-8")).get("evidence", [])
        if evidence_path.exists()
        else []
    )
    for comp in components:
        if comp.properties.get("distribution:review"):
            reviews.append(f"{comp.ref}: {comp.properties['distribution:review']}")
        for item in evidence:
            if item["purl"] == comp.ref:
                path = root / item["path"]
                if (
                    not path.resolve().is_relative_to(root.resolve())
                    or digest(path) != item["sha256"]
                    or not item["reason"].strip()
                ):
                    raise LicenseError(
                        f"Invalid supplemental licence evidence: {comp.ref}"
                    )
                label = item["path"] + " (" + item["url"] + ")"
                comp.texts[label] = path.read_text(encoding="utf-8")
        expression = comp.license
        for entry in policy.get("exceptions", []):
            if entry["purl"] == comp.ref:
                if not entry["reason"].strip() or normalize(
                    entry["original"]
                ) != normalize(expression):
                    raise LicenseError(f"Invalid/stale policy exception: {comp.ref}")
                expression = entry["replacement"]
                exceptions.append(f"{comp.ref}: {entry['reason']}")
                comp.properties["distribution:license-exception"] = entry["reason"]
        severity, chosen = policy_result(expression, policy)
        comp.properties["distribution:selected-license"] = chosen
        if severity == 2:
            failures.append(f"{comp.ref}: unapproved/missing licence {comp.license!r}")
        elif severity == 1:
            reviews.append(
                f"{comp.ref}: {chosen} ({'shipped' if comp.shipped else 'external runtime'})"
            )
        if not comp.texts:
            failures.append(f"{comp.ref}: no full licence/notice text found offline")
    refs = set(merged)
    graph[owned.ref] = refs - {owned.ref}
    bom = {
        "bomFormat": "CycloneDX",
        "specVersion": "1.5",
        "version": 1,
        "metadata": {
            "component": owned.bom(),
            "properties": [
                {"name": "distribution:artifact", "value": artifact},
                {
                    "name": "distribution:inventory-complete",
                    "value": str(not failures).lower(),
                },
                {
                    "name": "distribution:selection",
                    "value": "Cargo normal+build dependencies; npm production closure; Gradle release runtime closure. Build inputs may be conservatively included.",
                },
            ],
        },
        "components": [c.bom() for c in components if c.ref != owned.ref],
        "dependencies": [
            {"ref": ref, "dependsOn": sorted(graph.get(ref, set()) & refs)}
            for ref in sorted(refs)
        ],
    }
    groups = defaultdict(list)
    for comp in components:
        groups[normalize(comp.license) or "UNKNOWN"].append(comp)
    lines = [
        f"{name} {version} — {artifact}",
        "Third-party notices and owned component attributions",
        "",
        "Inventory includes normal and build inputs. Runtime downloads are labelled",
        "external and are not files shipped in the archive. npm entries conservatively",
        "cover production dependencies, not just tree-shaken modules. All upstream",
        "licence alternatives and supplied notices below are preserved verbatim.",
        "",
    ]
    for expression, members in sorted(groups.items()):
        lines.extend(["=" * 78, f"LICENCE GROUP: {expression}", "=" * 78])
        for comp in members:
            lines.extend(
                [
                    "",
                    comp.ref,
                    f"Distribution: {'shipped/build input' if comp.shipped else 'external runtime (not shipped)'}",
                ]
            )
            if comp.url:
                lines.append(f"Source: {comp.url}")
            for filename, content in sorted(comp.texts.items()):
                lines.extend(
                    [f"--- {filename} ---", content, f"--- end {filename} ---"]
                )
    report = {
        "artifact": artifact,
        "components": len(components),
        "failures": sorted(set(failures)),
        "review": sorted(set(reviews)),
        "exceptions": exceptions,
    }
    for filename, data in [
        ("sbom.cdx.json", json.dumps(bom, indent=2) + "\n"),
        ("THIRD-PARTY-NOTICES.txt", "\n".join(lines) + "\n"),
        ("license-report.json", json.dumps(report, indent=2) + "\n"),
        ("NOTICE", notice(root)),
    ]:
        atomic_write(output / filename, data.encode())
    print(
        f"{artifact}: {len(components)} components, {len(report['failures'])} failures, {len(report['review'])} review items, {len(exceptions)} exceptions"
    )
    for category in ("failures", "review", "exceptions"):
        for message in report[category]:
            print(f"  {category.upper()}: {message}")
    return not failures


def main() -> int:
    target = Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target")).resolve()
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--artifact", choices=[*TARGETS, "all"], default="all")
    parser.add_argument(
        "--check",
        action="store_true",
        help="fail if evidence/policy is incomplete; always writes reports",
    )
    parser.add_argument("--output", type=Path, default=target / "packaging/licenses")
    parser.add_argument(
        "--gradle-report",
        type=Path,
        default=target / "packaging/android/gradle-runtime.json",
    )
    parser.add_argument("--web-bundle", type=Path)
    parser.add_argument("--root", type=Path, default=ROOT, help=argparse.SUPPRESS)
    args = parser.parse_args()
    if args.web_bundle and not args.web_bundle.is_dir():
        parser.error("--web-bundle must name an existing built frontend directory")
    root = args.root.resolve()
    policy = tomllib.loads(
        (root / "packaging/license-policy.toml").read_text(encoding="utf-8")
    )
    ok = True
    for artifact in TARGETS if args.artifact == "all" else [args.artifact]:
        components, graph, gaps = [], {}, []
        try:
            native, edges = cargo_components(root, artifact, TARGETS[artifact])
            components.extend(native)
            graph.update(edges)
        except (LicenseError, OSError) as error:
            gaps.append(str(error))
        if artifact == "android":
            for getter in (
                lambda: npm_components(root),
                lambda: gradle_components(root, args.gradle_report),
            ):
                try:
                    values, edges = getter()
                    components.extend(values)
                    graph.update(edges)
                except (LicenseError, OSError) as error:
                    gaps.append(str(error))
        try:
            components.extend(
                extra_components(root, artifact, components, args.web_bundle)
            )
        except (LicenseError, OSError) as error:
            gaps.append(str(error))
        ok = (
            emit(
                root, artifact, args.output / artifact, components, graph, gaps, policy
            )
            and ok
        )
    return 1 if args.check and not ok else 0


if __name__ == "__main__":
    readable_output()
    try:
        sys.exit(main())
    except (
        ReleaseError,
        OSError,
        ValueError,
        KeyError,
        ET.ParseError,
        zipfile.BadZipFile,
    ) as error:
        sys.exit(f"Licence generation failed: {error}")
