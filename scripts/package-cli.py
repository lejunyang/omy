#!/usr/bin/env python3
"""统一生成各平台的独立 CLI 包，并确认包里的程序能正常启动。"""

import argparse
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import zipfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--output-dir", type=Path, default=Path("."))
    args = parser.parse_args()

    binary = args.binary.resolve(strict=True)
    readme = Path(__file__).resolve().parents[1] / "README.md"
    windows = "windows" in args.target
    executable = "omy.exe" if windows else "omy"
    extension = "zip" if windows else "tar.gz"
    args.output_dir.mkdir(parents=True, exist_ok=True)
    archive = args.output_dir / f"omy-{args.version}-{args.target}-cli.{extension}"

    with tempfile.TemporaryDirectory(prefix="omy-cli-package-") as temporary:
        staging = Path(temporary)
        shutil.copy(binary, staging / executable)
        shutil.copy(readme, staging / "README.md")
        if windows:
            with zipfile.ZipFile(archive, "w", zipfile.ZIP_DEFLATED) as bundle:
                for name in (executable, "README.md"):
                    bundle.write(staging / name, name)
        else:
            with tarfile.open(archive, "w:gz") as bundle:
                for name in (executable, "README.md"):
                    bundle.add(staging / name, arcname=name)

        # 运行解包后的文件，才能发现打包路径出错或缺少运行时动态库。
        unpacked = staging / "unpacked"
        shutil.unpack_archive(archive, unpacked)
        result = subprocess.run(
            [str(unpacked / executable), "--version"],
            check=True, stdout=subprocess.PIPE, text=True,
        )
        if result.stdout.strip() != f"omy {args.version}":
            raise SystemExit(f"CLI 版本与发布版本不一致：{result.stdout.strip()!r}")

    print(archive)


if __name__ == "__main__":
    main()
