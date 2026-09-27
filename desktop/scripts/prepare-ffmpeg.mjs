import { spawnSync } from "node:child_process";
import { mkdir, mkdtemp, readdir, rm, writeFile, copyFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const scriptDir = dirname(fileURLToPath(import.meta.url));
const resourceDir = resolve(scriptDir, "../src-tauri/resources");
const ffmpegPath = join(resourceDir, "ffmpeg.exe");
const ffprobePath = join(resourceDir, "ffprobe.exe");

if (process.platform !== "win32") {
  throw new Error("当前桌面发布目标是 Windows；FFmpeg 资源下载脚本仅准备 Windows x64 构建所需文件。");
}

try {
  const { access } = await import("node:fs/promises");
  await Promise.all([access(ffmpegPath), access(ffprobePath)]);
  process.exit(0);
} catch {
  // Download only when binaries are missing. They are ignored by Git and bundled into releases.
}

const scratch = await mkdtemp(join(tmpdir(), "tmd-ffmpeg-"));
const archivePath = join(scratch, "ffmpeg.zip");
const extracted = join(scratch, "unpacked");
try {
  const response = await fetch("https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/ffmpeg-master-latest-win64-lgpl.zip");
  if (!response.ok) throw new Error(`下载 FFmpeg 失败：HTTP ${response.status}`);
  await writeFile(archivePath, new Uint8Array(await response.arrayBuffer()));
  const expanded = spawnSync("powershell.exe", ["-NoProfile", "-NonInteractive", "-Command", "Expand-Archive", "-LiteralPath", archivePath, "-DestinationPath", extracted, "-Force"], { stdio: "inherit" });
  if (expanded.error || expanded.status !== 0) throw expanded.error ?? new Error("解压 FFmpeg 发布包失败");

  async function findBinary(directory, name) {
    for (const entry of await readdir(directory, { withFileTypes: true })) {
      const path = join(directory, entry.name);
      if (entry.isDirectory()) {
        const match = await findBinary(path, name);
        if (match) return match;
      } else if (entry.name.toLowerCase() === name) return path;
    }
    return null;
  }

  const foundFfmpeg = await findBinary(extracted, "ffmpeg.exe");
  if (!foundFfmpeg) throw new Error("FFmpeg 发布包中没有找到 ffmpeg.exe");
  const foundFfprobe = await findBinary(extracted, "ffprobe.exe");
  if (!foundFfprobe) throw new Error("FFmpeg 发布包中没有找到 ffprobe.exe");
  await mkdir(resourceDir, { recursive: true });
  await copyFile(foundFfmpeg, ffmpegPath);
  await copyFile(foundFfprobe, ffprobePath);
  const version = spawnSync(ffmpegPath, ["-version"], { encoding: "utf8" });
  const versionLine = version.stdout?.split(/\r?\n/, 1)[0] ?? "unknown version";
  const commit = versionLine.match(/-g([0-9a-f]{7,})\b/i)?.[1];
  await writeFile(join(resourceDir, "FFmpeg-SOURCE.txt"), [
    "The bundled FFmpeg binary is from the BtbN LGPL Windows x64 build.",
    `FFmpeg build: ${versionLine}`,
    "Build source and configuration: https://github.com/BtbN/FFmpeg-Builds",
    `FFmpeg source: https://github.com/FFmpeg/FFmpeg/tree/${commit ?? "master"}`,
    `Release asset: ${response.url}`,
    "Upstream license texts are included as FFmpeg-LICENSE.txt and FFmpeg-LICENSE-v3.txt.",
    "",
  ].join("\n"));
  const licenses = [
    ["COPYING.LGPLv2.1", "https://raw.githubusercontent.com/FFmpeg/FFmpeg/master/COPYING.LGPLv2.1"],
    ["COPYING.LGPLv3", "https://raw.githubusercontent.com/FFmpeg/FFmpeg/master/COPYING.LGPLv3"],
  ];
  for (const [name, url] of licenses) {
    const license = await fetch(url);
    if (!license.ok) throw new Error(`获取 FFmpeg 许可证失败：${name} HTTP ${license.status}`);
    await writeFile(join(resourceDir, name === "COPYING.LGPLv2.1" ? "FFmpeg-LICENSE.txt" : "FFmpeg-LICENSE-v3.txt"), new Uint8Array(await license.arrayBuffer()));
  }
  console.log("Prepared LGPL FFmpeg for the Windows release.");
} finally {
  await rm(scratch, { recursive: true, force: true });
}
