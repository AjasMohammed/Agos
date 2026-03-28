#!/usr/bin/env python3
"""
gen_types.py — Generate Python type stubs from Rust KernelCommand/KernelResponse.

Reads crates/agentos-bus/src/message.rs (with simple regex parsing) and emits
agentos/types_generated.py with the enum variant names as string constants.
Run this in CI to catch schema drift between Rust and Python.

Usage:
    python sdk/python/scripts/gen_types.py
    # or from repo root:
    python sdk/python/scripts/gen_types.py --rust crates/agentos-bus/src/message.rs \
        --out sdk/python/agentos/types_generated.py
"""
from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path


_REPO_ROOT = Path(__file__).resolve().parent.parent.parent.parent
_DEFAULT_RUST = _REPO_ROOT / "crates" / "agentos-bus" / "src" / "message.rs"
_DEFAULT_OUT = Path(__file__).resolve().parent.parent / "agentos" / "types_generated.py"


def _extract_enum_body(rust_src: str, enum_name: str) -> str | None:
    """
    Extract the body of a Rust enum by tracking brace depth.

    Handles struct variants that contain nested `{}` blocks.
    """
    header_pattern = rf"pub enum {enum_name}\s*\{{"
    m = re.search(header_pattern, rust_src)
    if not m:
        return None

    start = m.end()  # position after the opening `{`
    depth = 1
    i = start
    while i < len(rust_src) and depth > 0:
        if rust_src[i] == "{":
            depth += 1
        elif rust_src[i] == "}":
            depth -= 1
        i += 1

    return rust_src[start : i - 1]  # body between the outer braces


def parse_enum_variants(rust_src: str, enum_name: str) -> list[str]:
    """
    Extract variant names from a Rust enum using brace-depth scanning.

    Handles:
      - Unit variants: `VariantName,`
      - Struct variants: `VariantName { field: Type, ... },`
      - Tuple variants: `VariantName(Type),`
    """
    body = _extract_enum_body(rust_src, enum_name)
    if body is None:
        return []

    # Extract variant names: first PascalCase identifier on a non-comment,
    # non-attribute, non-blank line, skipping lines that start with lowercase
    # (Rust field names / type names inside struct variants).
    variants: list[str] = []
    seen: set[str] = set()

    for line in body.splitlines():
        stripped = line.strip()
        if not stripped or stripped.startswith("//") or stripped.startswith("#"):
            continue
        m = re.match(r"([A-Z][A-Za-z0-9_]*)", stripped)
        if m:
            name = m.group(1)
            if name not in seen:
                seen.add(name)
                variants.append(name)
    return variants


def generate_python(
    command_variants: list[str], response_variants: list[str]
) -> str:
    """Emit a Python module with string constants for all variant names."""
    lines = [
        '"""',
        "AUTO-GENERATED — do not edit by hand.",
        "Run sdk/python/scripts/gen_types.py to regenerate.",
        '"""',
        "# KernelCommand variants",
        "KERNEL_COMMAND_VARIANTS: tuple[str, ...] = (",
    ]
    for v in command_variants:
        lines.append(f'    "{v}",')
    lines.append(")")
    lines.append("")
    lines.append("# KernelResponse variants")
    lines.append("KERNEL_RESPONSE_VARIANTS: tuple[str, ...] = (")
    for v in response_variants:
        lines.append(f'    "{v}",')
    lines.append(")")
    lines.append("")
    return "\n".join(lines)


def main() -> int:
    parser = argparse.ArgumentParser(description="Generate Python types from Rust message.rs")
    parser.add_argument(
        "--rust",
        type=Path,
        default=_DEFAULT_RUST,
        help="Path to agentos-bus/src/message.rs",
    )
    parser.add_argument(
        "--out",
        type=Path,
        default=_DEFAULT_OUT,
        help="Output Python file path",
    )
    parser.add_argument(
        "--check",
        action="store_true",
        help="Check mode: exit 1 if generated output differs from existing file",
    )
    args = parser.parse_args()

    if not args.rust.exists():
        print(f"ERROR: Rust source not found: {args.rust}", file=sys.stderr)
        return 1

    rust_src = args.rust.read_text()
    command_variants = parse_enum_variants(rust_src, "KernelCommand")
    response_variants = parse_enum_variants(rust_src, "KernelResponse")

    if not command_variants:
        print("ERROR: No KernelCommand variants found. Is the path correct?", file=sys.stderr)
        return 1

    generated = generate_python(command_variants, response_variants)

    if args.check:
        if not args.out.exists():
            print(f"ERROR: Output file does not exist: {args.out}", file=sys.stderr)
            return 1
        existing = args.out.read_text()
        if existing != generated:
            print(
                "DRIFT DETECTED: types_generated.py is out of sync with message.rs",
                file=sys.stderr,
            )
            print("Run: python sdk/python/scripts/gen_types.py", file=sys.stderr)
            return 1
        print("OK: types_generated.py is up to date.")
        return 0

    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(generated)
    print(f"Written {len(command_variants)} command variants, "
          f"{len(response_variants)} response variants → {args.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
