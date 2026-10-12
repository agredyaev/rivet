"""Check production dependency edges; Cargo tools and fuzzing stay outside runtime."""

import json
import subprocess

TARGETS = (
    "aarch64-apple-darwin",
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
)
TOOLS = {
    "arbitrary", "libfuzzer-sys", "cargo-deny", "cargo-machete", "cargo-fuzz",
    "cargo-zigbuild", "cc", "cmake", "pkg-config", "syn", "quote", "proc-macro2",
    "toml", "toml_parser", "toml_writer", "serde_spanned",
}

for target in TARGETS:
    metadata = json.loads(subprocess.check_output([
        "cargo", "metadata", "--locked", "--format-version", "1",
        "--filter-platform", target,
    ]))
    packages = {package["id"]: package for package in metadata["packages"]}
    nodes = {node["id"]: node for node in metadata["resolve"]["nodes"]}
    runtime = set()
    macros = set()
    pending = [metadata["resolve"]["root"]]
    while pending:
        package_id = pending.pop()
        if package_id in runtime or package_id in macros:
            continue
        package = packages[package_id]
        # Procedural macros and their dependencies run in the compiler host.
        if any("proc-macro" in item["kind"] for item in package["targets"]):
            macros.add(package_id)
            continue
        runtime.add(package_id)
        for dependency in nodes[package_id]["deps"]:
            if any(kind["kind"] is None for kind in dependency["dep_kinds"]):
                pending.append(dependency["pkg"])
    names = {packages[package_id]["name"] for package_id in runtime}
    unexpected = sorted(names & TOOLS)
    if unexpected:
        raise SystemExit(f"{target}: tooling in runtime graph: {unexpected}")
    print(f"{target}: {len(runtime)} runtime packages; tooling excluded; "
          f"{len(macros)} proc-macro packages stay on host")
